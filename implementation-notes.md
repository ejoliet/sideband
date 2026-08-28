# Implementation notes

One line per decision. `DEVIATION:` marks a departure from the README.

## Unknowns audit (before implementation)

**Known knowns.** The README fixes the stack (iroh 1.x + iroh-blobs,
slack-morphism 2.x, clap 4, tokio, figment), the repository layout, the CLI
surface, the exit codes, the error behaviours, and the phase order.

**Known unknowns.** (a) Whether a Block Kit button can copy to the clipboard —
blocking for Phase 3, resolved: it cannot, see Open Question 2 below. (b) Whether
`/drop` should open a modal — deferrable, and deferred. (c) What a "hash
mismatch" looks like in the iroh-blobs error enum — blocking for Phase 1,
resolved as `GetError::Decode`. (d) Whether the ticket fits in ~300 bytes —
measured on 2026-08-28 at 424 base32 characters, above the README's ~300-byte
estimate because the endpoint publishes several direct addresses alongside the
relay URL. Still one ordinary Slack message, so the estimate being off changes
nothing.

**Unknown knowns (assumptions not written down).** The sender's process must stay
resident, so `send` is a foreground command, not fire-and-forget. The blob store
must live at a stable path or resume cannot work. The bot runs on the *sender's*
own machine, not on a shared server — otherwise `/drop <path>` has no meaning.
There is no daemon, no database and no state beyond the blob store.

**Unknown unknowns (what could go wrong that neither of us mentioned).** A
"wait until ready" call that can never become ready (this happened, see
BURNLOG). Cargo's MSRV resolver silently downgrading pinned dependencies (this
happened). Aborting a seeding task leaving an endpoint bound long enough to
collide with the next bind in a test (mitigated with a short sleep in the resume
test).

## Decisions

- 2026-08-28 — `DEVIATION:` workspace `rust-version = "1.91"`, not the README's
  1.78. iroh 1.1.0 declares `rust-version: 1.91`; with 1.78 cargo's MSRV-aware
  resolver silently selects iroh 0.29 and the pinned versions in the stack table
  become unreachable. Rejected: keeping 1.78 and using iroh 0.29, which is not
  the "iroh 1.x" the README specifies.
- 2026-08-28 — Ticket encoding is `postcard` inside `BASE32_NOPAD` behind a
  `sideband` prefix, with the iroh `BlobTicket` nested as its own `Display`
  string. Rejected: depending on `iroh-tickets` for a tighter binary nesting —
  the saving is a few dozen bytes and the ticket already fits the README's
  ~300-byte budget. The wire type is a single-variant enum so postcard writes a
  discriminator and `V1` can be added without breaking `V0` readers.
- 2026-08-28 — `ImportMode::TryReference` on send and `ExportMode::TryReference`
  on receive. Rejected: `Copy` mode, which would need 2× the disk for a 50 GB
  file and contradicts the "unlimited" pitch.
- 2026-08-28 — Resume is not implemented; it is inherited. A persistent `FsStore`
  at `$XDG_CACHE_HOME/sideband/store` plus `Remote::fetch`, which subtracts
  locally-held ranges before requesting, gives resume with no bookkeeping here.
  Consequence worth remembering: the store directory must never be temporary.
- 2026-08-28 — `--max-downloads` counts completions via
  `EventMask { get: RequestMode::NotifyLog }`, spawning one watcher per request
  that waits for `RequestUpdate::Completed` and reports on an mpsc the seed loop
  selects on. Rejected: counting connections, which would over-count retries and
  under-count resumed transfers.
- 2026-08-28 — `Relays::Disabled` is a real variant of the public enum rather
  than a test-only flag, because `reachable_addr` must behave differently for it:
  `Endpoint::online()` waits for a home relay and never resolves when relays are
  off. Loopback tests need this; so would a LAN-only deployment.
- 2026-08-28 — Hash mismatch is classified on `GetError::Decode { .. }` rather
  than by matching error strings. Everything else from the get path becomes
  `Network` (exit 3).
- 2026-08-28 — `DEVIATION:` the README says `HashMismatch` should "delete
  partial". iroh-blobs 0.103 has no `blobs().delete()`; deletion is tag-based
  with garbage collection. The guarantee is met structurally instead: BLAKE3
  verified streaming never writes unverified bytes to the store, and `fetch`
  returns the error before exporting, so no file with the payload's name is ever
  created. Nothing unverified exists to delete.
- 2026-08-28 — `unique_path` never overwrites an existing file; `report.tar`
  becomes `report-1.tar`. Rejected: overwriting, and prompting. A receive is
  often unattended.
- 2026-08-28 — Slack posting from `send` runs on a detached task fed by an
  unbounded channel, and `finish` waits at most 5 s for the final card update.
  This is what makes "transfer never blocks on Slack" structurally true rather
  than a matter of care.
- 2026-08-28 — A missing `SLACK_BOT_TOKEN` during `send` is a warning, not an
  error. The ticket already works; refusing to transfer because Slack is
  unconfigured would invert the product's own claim.
- 2026-08-28 — Bot state reaches the callbacks through
  `SlackClientEventsListenerEnvironment::with_user_state`, because
  slack-morphism's `UserCallbackFunction` is a `fn` pointer and cannot capture.
- 2026-08-28 — `DEVIATION:` `/drop` takes a path as a text argument
  (`/drop ~/big.tar`) instead of opening a Block Kit modal as the README's
  architecture note suggests. A modal needs `views_open` plus a
  `view_submission` handler for no functional gain at this phase; the text form
  is testable today and the modal is a drop-in upgrade behind the same handler.
- 2026-08-28 — `DEVIATION:` Slack is not mocked with `wiremock`. slack-morphism
  2.25 builds every request against a hardcoded `SLACK_API_URI_STR =
  "https://slack.com/api"` const with no base-URL seam, so wiremock cannot
  intercept it without patching the crate. Instead `slack.rs` is tested by
  serializing the Block Kit payload and asserting on the JSON, which satisfies
  the stronger requirement ("never hit real Slack in tests") because no HTTP
  request is issued at all. Rejected: a vendored fork of slack-morphism with a
  configurable base URI, which is a large maintenance cost for test-only reach.
- 2026-08-28 — `#![deny(warnings)]` lives in CI as `RUSTFLAGS` rather than in
  the source, so a new compiler warning breaks CI without breaking every local
  build.
- 2026-08-28 — `tests/transfer.rs` sits at the repository root as the README's
  layout requires, wired in through a `[[test]] path = "../../tests/..."` entry
  in `sideband-core`, because cargo only builds test targets that belong to a
  package.
- 2026-08-28 — The test payload comes from an inline xorshift generator rather
  than `rand`, avoiding a dependency for something that also benefits from being
  reproducible on failure.
- 2026-08-28 — Release binary measured at 20,258,652 bytes (19 MiB) stripped on
  arm64 with `lto = "thin"` and `codegen-units = 1`, against the README's 25 MB
  budget. `panic = "abort"` was tried and dropped: it warns on
  `cargo test --release` and the remaining headroom made it unnecessary.
- 2026-08-28 — Verified non-negotiable 2 by hand: `send --channel sideband-smoke
  --quiet` with a syntactically valid but fake bot token in `SLACK_BOT_TOKEN`
  (not written out here — `ship-check` scans this file too, and it was right to)
  printed its ticket in 12 s, logged `Slack API error: invalid_auth`, and a
  `get` from a separate `XDG_CACHE_HOME` pulled all 8 MiB back with matching
  SHA-256 in 11 s. Separate cache dirs matter: with one store the fetch would be
  a local export and prove nothing.
- 2026-08-28 — `ship-check` scans the full git history, not just the working
  tree, because a secret that was committed and later deleted is still published
  by `git push`. It also scans untracked-but-unignored files, which are one
  `git add` from being published. Its own pattern list is written so the script
  does not match itself (every pattern requires a real character run after the
  prefix), which is cheaper than exempting the file and then trusting the
  exemption.
- 2026-08-28 — `ship-check` runs the suite with `SLACK_BOT_TOKEN` and
  `SLACK_APP_TOKEN` stripped from the environment. A test that quietly depends on
  the developer's own credentials passes locally and fails for everyone else.
- 2026-08-28 — `DEVIATION:` "signed release binaries" is build provenance
  (`actions/attest-build-provenance`, verifiable with `gh attestation verify`),
  not Apple Developer ID signing. Notarization needs a paid Developer ID and
  secrets that do not exist; provenance needs neither and is verifiable today.
  minisign over `SHA256SUMS` is wired in but skipped unless a
  `MINISIGN_SECRET_KEY` secret appears, so the release does not break waiting for
  a key. Rejected: ad-hoc `codesign`, which produces a signature that proves
  nothing.
- 2026-08-28 — Linux artifacts target `x86_64-unknown-linux-gnu` built on
  `ubuntu-22.04` rather than musl. musl would be statically linked and tidier,
  but the rustls/aws-lc path in iroh's dependency tree makes musl cross-builds
  fragile, and building against the oldest supported glibc already covers the
  machines this ships to. aarch64 Linux is deliberately absent; `install.sh` says
  so instead of 404ing.
- 2026-08-28 — `install.sh` refuses to install when it cannot compute a SHA-256
  (no `sha256sum` and no `shasum`) rather than falling back to installing
  unverified bytes. Same instinct as `fetch`: never keep what you could not
  verify.

## Open Questions from the README

1. **Receive UX without the CLI.** Resolved as the README's own default: both,
   brew first. Implemented: `slack::INSTALL_HINT` names the brew tap and then the
   `curl | sh` route, and appears both in the card's context block and in the
   Receive button's ephemeral reply — the two places a receiver without the CLI
   actually looks. `scripts/install.sh` and `packaging/homebrew/sideband.rb` are
   the two routes; the tap repository itself is owner work, not code work.
2. **Does the Receive button copy the command?** Resolved: **no.** Block Kit has
   no clipboard access; a button can only trigger an interaction. Two
   consequences, both implemented: the card body carries the one-liner inside a
   fenced code block, which is what Slack's own copy affordance acts on, and the
   button sends the same one-liner back as an ephemeral message visible only to
   the person who clicked. The button is therefore a convenience over the visible
   text, never the only path.
3. **Slack App Directory stance.** Out of scope until M4; untouched.
4. **Name availability.** Not verified from here — needs a check against
   crates.io, GitHub and Slack app names before M2. Flagged for the owner.
