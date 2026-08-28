# Developer notes

## Layout

| Path | What lives there |
|---|---|
| `crates/sideband-core/src/ticket.rs` | the ticket type and its compact text encoding |
| `crates/sideband-core/src/transfer.rs` | iroh endpoint setup, seeding, fetching, resume |
| `crates/sideband-core/src/slack.rs` | Block Kit card rendering and the Slack Web API calls |
| `crates/sideband-core/src/config.rs` | layered configuration and XDG paths |
| `crates/sideband-core/src/error.rs` | the error taxonomy behind the exit codes |
| `crates/sideband/src/main.rs` | clap CLI: `send`, `get`, `bot`, `doctor` |
| `crates/sideband/src/card.rs` | the detached Slack card publisher |
| `crates/sideband/src/bot.rs` | Socket Mode daemon |
| `crates/sideband/src/doctor.rs` | connectivity and auth diagnostics |
| `tests/transfer.rs` | two in-process iroh endpoints, 10 MB blob, resume |

## Build

```bash
cargo build                      # debug
cargo build --release            # release; the binary strips itself, see Cargo.toml
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

The MSRV is 1.91, set by iroh 1.1. The README's prerequisites table says 1.78,
which predates iroh 1.x; see `implementation-notes.md`.

## How a transfer actually works

`sideband send` does four things, in this order, and only the first three matter
for correctness:

1. **Import.** `FsStore::add_path_with_opts` with `ImportMode::TryReference`
   hashes the file into an outboard without copying the payload. Sharing a 50 GB
   file costs kilobytes of extra disk, not 50 GB.
2. **Bind.** An iroh endpoint comes up and waits for a home relay so the address
   published in the ticket is dialable from other networks. Three attempts with
   growing timeouts, then exit 3.
3. **Ticket.** The blob ticket (endpoint address + hash + format) plus the file
   name, size and absolute expiry, postcard-encoded and base32'd behind a
   `sideband` prefix. Around 300 characters, safe to paste anywhere.
4. **Card.** Posted on a spawned task. `send` never awaits it.

`sideband get` dials the address in the ticket, then `Remote::fetch` streams the
blob with BLAKE3 verification. Two properties fall out of iroh-blobs rather than
out of code here:

- **Resume is free.** `Remote::fetch` computes which ranges the local store
  already has and asks only for the rest. Because the store lives at a stable
  path (`$XDG_CACHE_HOME/sideband/store`), re-running the same `get` resumes.
  This is why the store directory must not be a temporary directory.
- **Unverified bytes are never stored.** Verified streaming means a tampered or
  corrupted range fails to decode and is discarded mid-stream. `fetch` returns
  the `HashMismatch` error before it exports anything, so no partial file with
  the final name can exist. There is no explicit delete because there is nothing
  to delete.

## Relays

`Relays::N0` (default) uses the n0 public relays for hole punching and fallback.
`Relays::Custom(url)` comes from `SIDEBAND_RELAY_URL`. `Relays::Disabled` exists
for the loopback tests: with no relay configured, `Endpoint::online()` never
resolves, so `reachable_addr` short-circuits and returns direct addresses.

## Testing

`cargo test --workspace` is fully offline. `tests/transfer.rs` binds two
endpoints in one process with relays disabled and moves a 10 MB blob, then
interrupts a second transfer part-way and proves the re-run sends less than the
whole file.

Slack is tested by serializing the Block Kit payload and asserting on the JSON.
slack-morphism hardcodes `https://slack.com/api`, so `wiremock` cannot intercept
it without a base-URL seam; asserting on the payload offline is stricter than
mocking anyway, because no request is possible at all. See
`implementation-notes.md`.

## Shipping gate

`scripts/ship-check.sh` is the one command to run before anything goes public.
It scans the working tree *and* the whole git history for token-shaped strings,
checks that `.env` is ignored and untracked, then runs fmt, clippy and the test
suite with `SLACK_BOT_TOKEN`/`SLACK_APP_TOKEN` unset — a test that needs
credentials is a test that fails on a stranger's machine. `--fast` skips the
history scan and the suite for pre-commit use. It runs in CI on every push and
again as the first job of a release.

Measured 2026-08-28: the stripped arm64 binary is 20,258,652 bytes (19 MiB),
against the README's 25 MB budget.

## Releasing

Tag `vX.Y.Z` and push it. `.github/workflows/release.yml` runs `ship-check`,
builds `aarch64-apple-darwin`, `x86_64-apple-darwin` and
`x86_64-unknown-linux-gnu` on native runners, re-checks the size budget,
smoke-tests each binary, and publishes `sideband-<target>.tar.gz` plus
`SHA256SUMS` to a GitHub Release.

"Signed" means build provenance: `actions/attest-build-provenance` produces a
Sigstore attestation with no long-lived key, and anyone can check it with
`gh attestation verify sideband-<target>.tar.gz -R ejoliet/sideband`. Apple
Developer ID signing and notarization need credentials this project does not
have; until it does, macOS users get an unsigned binary and Gatekeeper will ask.
Adding a `MINISIGN_SECRET_KEY` repository secret additionally publishes
`SHA256SUMS.minisig`; the workflow skips that step when the secret is absent.

Install routes, brew first, per the README's Open Question 1:

- `brew install ejoliet/tap/sideband` — formula template in
  `packaging/homebrew/sideband.rb`; the published copy lives in the tap repo and
  needs its `version` and `sha256` lines updated from `SHA256SUMS` per release.
- `curl -fsSL .../scripts/install.sh | sh` — `scripts/install.sh` picks the
  archive for the host, verifies it against `SHA256SUMS`, and refuses to install
  when the checksum does not match or when no `sha256sum`/`shasum` exists.

Both routes are named on every card and in the Receive button's ephemeral reply
(`slack::INSTALL_HINT`), which is the only place a receiver without the CLI
looks.

## Slack app setup

Socket Mode on. Bot scopes: `chat:write`, `commands`. Slash command `/drop`.
Interactivity enabled (Socket Mode delivers the button presses; no request URL is
needed). `SLACK_BOT_TOKEN` is the `xoxb-` token; `SLACK_APP_TOKEN` is the
`xapp-` token and is only needed by `sideband bot`.
