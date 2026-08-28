//! End-to-end transfer tests over two in-process iroh endpoints.
//!
//! Everything runs on loopback with relays disabled, so these tests never touch
//! the network and never touch Slack.

use std::{path::Path, sync::Arc, time::Duration};

use sideband_core::transfer::{self, GetOptions, Relays, Seeder, SendOptions};
use tokio::sync::{mpsc, Mutex};

const TEN_MB: usize = 10 * 1024 * 1024;

/// Deterministic pseudo-random payload: incompressible enough to be a real
/// transfer, reproducible enough to diff on failure. Avoids a `rand` dependency.
fn payload(len: usize) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn send_opts(store: &Path) -> SendOptions {
    SendOptions {
        ttl_hours: 0,
        max_downloads: 0,
        relays: Relays::Disabled,
        store_dir: store.to_path_buf(),
    }
}

fn get_opts(store: &Path) -> GetOptions {
    GetOptions {
        relays: Relays::Disabled,
        store_dir: store.to_path_buf(),
    }
}

#[tokio::test]
async fn transfers_a_ten_megabyte_blob_between_two_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("payload.bin");
    let bytes = payload(TEN_MB);
    tokio::fs::write(&source, &bytes).await.unwrap();

    let seeder = Seeder::prepare(&source, send_opts(&dir.path().join("sender")))
        .await
        .unwrap();
    let ticket = seeder.ticket().clone();
    assert_eq!(ticket.meta.name, "payload.bin");
    assert_eq!(ticket.meta.size as usize, TEN_MB);

    // The ticket survives a round trip through the compact text encoding, which
    // is the form that actually travels through Slack.
    let ticket = ticket.to_string().parse().unwrap();

    let seeding = tokio::spawn(async move { seeder.seed(|_| {}).await });

    let out = dir.path().join("out");
    let mut last = 0u64;
    let dest = transfer::fetch(&ticket, &out, get_opts(&dir.path().join("receiver")), |p| {
        assert!(p.done >= last, "progress must not go backwards");
        last = p.done;
    })
    .await
    .unwrap();

    assert_eq!(dest, out.join("payload.bin"));
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), bytes);
    assert_eq!(last, TEN_MB as u64, "progress must reach the full size");

    seeding.abort();
}

#[tokio::test]
async fn resumes_a_partial_transfer_on_re_run() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("payload.bin");
    let bytes = payload(TEN_MB);
    tokio::fs::write(&source, &bytes).await.unwrap();
    let receiver_store = dir.path().join("receiver");

    // First attempt: cancelled once a couple of megabytes have landed, standing
    // in for a laptop lid closing mid-transfer.
    let seeder = Seeder::prepare(&source, send_opts(&dir.path().join("sender-1")))
        .await
        .unwrap();
    let ticket = seeder.ticket().clone();
    let seeding = tokio::spawn(async move { seeder.seed(|_| {}).await });

    let out = dir.path().join("out");
    let (enough_tx, mut enough_rx) = mpsc::channel::<()>(1);
    let interrupted = Arc::new(Mutex::new(0u64));
    let seen = interrupted.clone();
    let attempt = transfer::fetch(&ticket, &out, get_opts(&receiver_store), move |p| {
        if let Ok(mut guard) = seen.try_lock() {
            *guard = p.done;
        }
        if p.done >= 2 * 1024 * 1024 {
            let _ = enough_tx.try_send(());
        }
    });
    let stopped = tokio::select! {
        result = attempt => Some(result),
        _ = enough_rx.recv() => None,
    };
    assert!(
        stopped.is_none(),
        "the first attempt finished before it could be interrupted; \
         the cancellation threshold is too high for this machine"
    );
    let partial = *interrupted.lock().await;
    assert!(partial >= 2 * 1024 * 1024, "expected a partial transfer");
    seeding.abort();
    // Let the aborted sender release its endpoint before the next one binds.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Second attempt: a fresh sender, so a new endpoint and a new ticket, but
    // the same content hash. Resume is keyed on content, not on the peer.
    let seeder = Seeder::prepare(&source, send_opts(&dir.path().join("sender-2")))
        .await
        .unwrap();
    let ticket = seeder.ticket().clone();
    let seeding = tokio::spawn(async move { seeder.seed(|_| {}).await });

    let mut transferred = 0u64;
    let dest = transfer::fetch(&ticket, &out, get_opts(&receiver_store), |p| {
        transferred = p.done;
    })
    .await
    .unwrap();

    assert_eq!(tokio::fs::read(&dest).await.unwrap(), bytes);
    assert!(
        transferred < TEN_MB as u64,
        "re-run transferred {transferred} of {TEN_MB} bytes, so nothing was resumed"
    );

    seeding.abort();
}

/// The one test that needs the internet: can this machine actually reach an n0
/// relay and publish an address a stranger could dial?
#[cfg(feature = "relay-e2e")]
#[tokio::test]
async fn reaches_a_public_relay() {
    let addr = transfer::probe_endpoint(&Relays::N0).await.unwrap();
    assert!(
        addr.relay_urls().next().is_some(),
        "no home relay, so the published address would not be dialable"
    );
}

#[tokio::test]
async fn refuses_an_expired_ticket_without_dialling_anyone() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("small.bin");
    tokio::fs::write(&source, b"hello").await.unwrap();

    let seeder = Seeder::prepare(&source, send_opts(&dir.path().join("sender")))
        .await
        .unwrap();
    let mut ticket = seeder.ticket().clone();
    ticket.expires_at = Some(1);

    let err = transfer::fetch(
        &ticket,
        &dir.path().join("out"),
        get_opts(&dir.path().join("receiver")),
        |_| {},
    )
    .await
    .expect_err("an expired ticket must not transfer");
    assert_eq!(sideband_core::error::exit_code_of(&err), 5);
}
