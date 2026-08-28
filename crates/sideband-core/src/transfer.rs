//! Transfer engine: import and seed a file over iroh-blobs, and fetch one back.
//!
//! Both directions use a persistent `FsStore`, which is what makes an
//! interrupted `get` resume on re-run: `Remote::fetch` asks only for the ranges
//! the local store is still missing.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context};
use iroh::{endpoint::presets, protocol::Router, Endpoint, EndpointAddr, RelayMap, RelayMode};
use iroh_blobs::{
    api::{
        blobs::{AddPathOptions, ExportMode, ExportOptions, ImportMode},
        proto::BlobStatus,
        remote::GetProgressItem,
    },
    provider::events::{EventMask, EventSender, ProviderMessage, RequestMode, RequestUpdate},
    store::fs::FsStore,
    ticket::BlobTicket,
    BlobFormat,
};
use n0_future::StreamExt;
use tokio::sync::mpsc;

use crate::{
    error::SidebandError,
    ticket::{FileMeta, Ticket},
};

/// Which relays the endpoint may fall back to when hole punching fails.
#[derive(Debug, Clone, Default)]
pub enum Relays {
    /// The n0 public relays.
    #[default]
    N0,
    /// A self-hosted relay, from `SIDEBAND_RELAY_URL`.
    Custom(String),
    /// No relays at all. Direct addresses only — used by the loopback tests.
    Disabled,
}

impl Relays {
    /// `Custom` when a relay URL is configured, otherwise the n0 defaults.
    pub fn from_config(relay_url: Option<&str>) -> Self {
        match relay_url.filter(|u| !u.is_empty()) {
            Some(url) => Self::Custom(url.to_string()),
            None => Self::N0,
        }
    }
}

/// Everything `send` needs that is not the file itself.
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// Ticket lifetime and seeding duration. 0 means no expiry.
    pub ttl_hours: u32,
    /// Stop seeding after N completed downloads. 0 means unlimited.
    pub max_downloads: u32,
    /// Relay configuration.
    pub relays: Relays,
    /// Blob store root.
    pub store_dir: PathBuf,
}

/// Everything `get` needs that is not the ticket.
#[derive(Debug, Clone)]
pub struct GetOptions {
    /// Relay configuration.
    pub relays: Relays,
    /// Blob store root. Must be stable across runs for resume to work.
    pub store_dir: PathBuf,
}

/// Observable seeding activity, for CLI output and Slack card updates.
#[derive(Debug, Clone, Copy)]
pub enum SeedEvent {
    /// A peer opened a get request.
    DownloadStarted,
    /// A peer finished a download. `total` counts completions so far.
    DownloadCompleted { total: u32 },
}

/// Why the seeder stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedStop {
    /// `--max-downloads` reached.
    MaxDownloads(u32),
    /// TTL elapsed.
    Expired,
    /// Ctrl-C.
    Interrupted,
}

/// Fetch progress, in payload bytes.
#[derive(Debug, Clone, Copy)]
pub struct GetProgress {
    /// Bytes transferred so far in this run.
    pub done: u64,
    /// Total size from the ticket metadata.
    pub total: u64,
}

/// A file imported into the store, with an endpoint accepting blob requests.
pub struct Seeder {
    router: Router,
    store: FsStore,
    ticket: Ticket,
    events: mpsc::Receiver<ProviderMessage>,
    max_downloads: u32,
}

impl Seeder {
    /// Hashes `path`, binds an endpoint and starts accepting blob requests.
    ///
    /// Returns as soon as the ticket is valid; call [`Seeder::seed`] to stay
    /// resident and serve it.
    pub async fn prepare(path: &Path, opts: SendOptions) -> anyhow::Result<Self> {
        let path =
            std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))?;
        let file_meta = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if !file_meta.is_file() {
            return Err(anyhow!("{} is not a regular file", path.display()));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;

        let store = open_store(&opts.store_dir).await?;
        // TryReference avoids copying the payload into the store, so seeding a
        // 50 GB file costs no extra disk.
        let tag = store
            .blobs()
            .add_path_with_opts(AddPathOptions {
                path: path.clone(),
                format: BlobFormat::Raw,
                mode: ImportMode::TryReference,
            })
            .await
            .with_context(|| format!("hashing {}", path.display()))?;

        // NotifyLog gives us per-request transfer updates, which is how
        // --max-downloads counts completions.
        let mask = EventMask {
            get: RequestMode::NotifyLog,
            ..EventMask::DEFAULT
        };
        let (event_sender, events) = EventSender::channel(64, mask);
        let blobs = iroh_blobs::BlobsProtocol::new(&store, Some(event_sender));

        let endpoint = bind_endpoint(&opts.relays).await?;
        let addr = reachable_addr(&endpoint, &opts.relays).await?;
        let ticket = Ticket::new(
            BlobTicket::new(addr, tag.hash, tag.format),
            FileMeta {
                name,
                size: file_meta.len(),
            },
            opts.ttl_hours,
        );

        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs)
            .spawn();

        Ok(Self {
            router,
            store,
            ticket,
            events,
            max_downloads: opts.max_downloads,
        })
    }

    /// The ticket to publish. Valid as soon as `prepare` returns.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Serves the blob until max-downloads, TTL or Ctrl-C, then shuts down.
    pub async fn seed(self, mut on_event: impl FnMut(SeedEvent)) -> anyhow::Result<SeedStop> {
        let Self {
            router,
            store,
            ticket,
            mut events,
            max_downloads,
        } = self;

        // Kept alive for the whole loop so neither channel can close and
        // disable its select! branch.
        let (completed_tx, mut completed_rx) = mpsc::channel::<()>(16);
        let deadline = ticket
            .remaining_secs()
            .map(|secs| tokio::time::Instant::now() + Duration::from_secs(secs));
        let mut completed = 0u32;

        let stop = loop {
            tokio::select! {
                () = sleep_until(deadline) => break SeedStop::Expired,
                _ = tokio::signal::ctrl_c() => break SeedStop::Interrupted,
                Some(()) = completed_rx.recv() => {
                    completed += 1;
                    on_event(SeedEvent::DownloadCompleted { total: completed });
                    if max_downloads > 0 && completed >= max_downloads {
                        break SeedStop::MaxDownloads(completed);
                    }
                }
                Some(message) = events.recv() => {
                    if let ProviderMessage::GetRequestReceivedNotify(request) = message {
                        on_event(SeedEvent::DownloadStarted);
                        let completed_tx = completed_tx.clone();
                        let mut updates = request.rx;
                        tokio::spawn(async move {
                            while let Ok(Some(update)) = updates.recv().await {
                                if matches!(update, RequestUpdate::Completed(_)) {
                                    let _ = completed_tx.send(()).await;
                                }
                            }
                        });
                    }
                }
            }
        };

        router.shutdown().await.ok();
        store.shutdown().await.ok();
        Ok(stop)
    }
}

/// Downloads the blob named by `ticket` into `out_dir` and returns its path.
///
/// Resumable: re-running with the same store picks up where it left off.
pub async fn fetch(
    ticket: &Ticket,
    out_dir: &Path,
    opts: GetOptions,
    mut on_progress: impl FnMut(GetProgress),
) -> anyhow::Result<PathBuf> {
    ticket.check_fresh()?;
    tokio::fs::create_dir_all(out_dir)
        .await
        .with_context(|| format!("creating {}", out_dir.display()))?;

    let store = open_store(&opts.store_dir).await?;
    let endpoint = bind_endpoint(&opts.relays).await?;
    let hash = ticket.blob.hash();

    let connection = connect_with_retry(&endpoint, ticket).await?;
    let mut progress = store
        .remote()
        .fetch(connection, ticket.blob.hash_and_format())
        .stream();

    let mut result = None;
    while let Some(item) = progress.next().await {
        match item {
            GetProgressItem::Progress(done) => on_progress(GetProgress {
                done,
                total: ticket.meta.size,
            }),
            GetProgressItem::Done(_) => result = Some(Ok(())),
            GetProgressItem::Error(err) => result = Some(Err(err)),
        }
    }

    match result {
        Some(Ok(())) | None => {}
        // A decode failure in BLAKE3 verified streaming means the peer sent
        // bytes that did not verify. Nothing unverified was written to the
        // store, and we stop before exporting anything.
        Some(Err(err @ iroh_blobs::get::GetError::Decode { .. })) => {
            endpoint.close().await;
            store.shutdown().await.ok();
            return Err(anyhow::Error::new(SidebandError::HashMismatch {
                hash: hash.to_string(),
            })
            .context(err.to_string()));
        }
        Some(Err(err)) => {
            endpoint.close().await;
            store.shutdown().await.ok();
            return Err(anyhow::Error::new(SidebandError::Network(format!(
                "download failed: {err}"
            ))));
        }
    }

    if let BlobStatus::Partial { size } = store.blobs().status(hash).await? {
        endpoint.close().await;
        store.shutdown().await.ok();
        return Err(anyhow::Error::new(SidebandError::Network(format!(
            "sender went away with {} of {} bytes transferred; re-run to resume",
            size.unwrap_or(0),
            ticket.meta.size
        ))));
    }

    let dest = unique_path(out_dir, &ticket.meta.name);
    // TryReference moves rather than copies where the store allows it, so a
    // large receive does not need twice the free space.
    store
        .blobs()
        .export_with_opts(ExportOptions {
            hash,
            target: dest.clone(),
            mode: ExportMode::TryReference,
        })
        .await
        .with_context(|| format!("writing {}", dest.display()))?;

    endpoint.close().await;
    store.shutdown().await.ok();
    Ok(dest)
}

/// Binds an endpoint and reports whether relays and hole punching work.
///
/// Used by `sideband doctor`; returns the endpoint's own address on success.
pub async fn probe_endpoint(relays: &Relays) -> anyhow::Result<EndpointAddr> {
    let endpoint = bind_endpoint(relays).await?;
    let addr = reachable_addr(&endpoint, relays).await?;
    endpoint.close().await;
    Ok(addr)
}

async fn open_store(root: &Path) -> anyhow::Result<FsStore> {
    tokio::fs::create_dir_all(root)
        .await
        .with_context(|| format!("creating blob store at {}", root.display()))?;
    FsStore::load(root)
        .await
        .with_context(|| format!("opening blob store at {}", root.display()))
}

async fn bind_endpoint(relays: &Relays) -> anyhow::Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0);
    builder = match relays {
        Relays::N0 => builder,
        Relays::Disabled => builder.relay_mode(RelayMode::Disabled),
        Relays::Custom(url) => {
            let map = RelayMap::try_from_iter([url.as_str()]).map_err(|err| {
                SidebandError::Config(format!("SIDEBAND_RELAY_URL is not a relay URL: {err}"))
            })?;
            builder.relay_mode(RelayMode::Custom(map))
        }
    };
    builder
        .bind()
        .await
        .map_err(|err| SidebandError::Network(format!("could not bind an iroh endpoint: {err}")))
        .map_err(Into::into)
}

/// Waits for a home relay so the address we publish is dialable from anywhere.
///
/// Retries with backoff 3x, matching the `RelayUnreachable` contract, then
/// gives up with exit code 3. With `Relays::Disabled` there is nothing to wait
/// for and the local direct addresses are returned immediately.
async fn reachable_addr(endpoint: &Endpoint, relays: &Relays) -> anyhow::Result<EndpointAddr> {
    if matches!(relays, Relays::Disabled) {
        return Ok(endpoint.addr());
    }
    for attempt in 1..=3u32 {
        if tokio::time::timeout(
            Duration::from_secs(5 * u64::from(attempt)),
            endpoint.online(),
        )
        .await
        .is_ok()
        {
            return Ok(endpoint.addr());
        }
        tracing::warn!(attempt, "no relay connection yet, retrying");
    }
    Err(
        SidebandError::Network("no relay reachable after 3 attempts; run `sideband doctor`".into())
            .into(),
    )
}

async fn connect_with_retry(
    endpoint: &Endpoint,
    ticket: &Ticket,
) -> anyhow::Result<iroh::endpoint::Connection> {
    let mut last = None;
    for attempt in 1..=3u32 {
        match endpoint
            .connect(ticket.blob.addr().clone(), iroh_blobs::ALPN)
            .await
        {
            Ok(connection) => return Ok(connection),
            Err(err) => {
                tracing::warn!(attempt, %err, "dial failed, retrying");
                last = Some(err.to_string());
                tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
            }
        }
    }
    Err(SidebandError::Network(format!(
        "could not reach the sender after 3 attempts: {}",
        last.unwrap_or_default()
    ))
    .into())
}

/// Never overwrite: `report.tar` becomes `report-1.tar` if taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()));
    for n in 1u32.. {
        let candidate = dir.join(format!("{stem}-{n}{}", ext.as_deref().unwrap_or("")));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("u32 range is not exhaustible in practice")
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_avoids_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a.tar"), dir.path().join("a.tar"));
        std::fs::write(dir.path().join("a.tar"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.tar"), dir.path().join("a-1.tar"));
        std::fs::write(dir.path().join("a-1.tar"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.tar"), dir.path().join("a-2.tar"));
    }

    #[test]
    fn unique_path_handles_names_without_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "data"), dir.path().join("data-1"));
    }

    #[test]
    fn relays_from_config_ignores_empty_url() {
        assert!(matches!(Relays::from_config(None), Relays::N0));
        assert!(matches!(Relays::from_config(Some("")), Relays::N0));
        assert!(matches!(
            Relays::from_config(Some("https://r")),
            Relays::Custom(_)
        ));
    }
}
