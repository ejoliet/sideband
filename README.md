# sideband

> Slack carries the intent. iroh carries the bytes. Unlimited, end-to-end encrypted file transfer between teammates — brokered by Slack, never stored on it.

**Type:** RDD spec (Type A) — a coding agent implements end-to-end from this document.
**Status:** pre-spike. **License:** MIT (core). **Owner:** ejoliet.

---

## Purpose

**Problem.** Slack free tier caps storage, purges history after 90 days, and chokes on large files. Teams fall back to Drive links, S3 presigned URLs, or ngrok hacks — all server-side, all another account, all leaving the tool they're already in.

**Solution.** `sideband` uses Slack purely as a signaling/control plane. Files transfer directly laptop-to-laptop over iroh QUIC connections: content-addressed, resumable, BLAKE3-verified, end-to-end encrypted. Slack sees a ~300-byte ticket, never the payload.

**Who benefits.** Dev/ops/science teams already living in Slack who move large artifacts (builds, datasets, FITS tarballs, videos) between each other.

**Wow demo (the launch GIF).** `sideband send ./roman-l2.tar` on laptop A → Block Kit message appears in `#data` with file name, size, hash, and a **Receive** button → teammate on another network runs the copied one-liner → 50 GB streams direct, resumable, verified.

---

## Architecture

Data flow (v1):

1. `sideband send <path>` — hashes the file (BLAKE3 via iroh-blobs), binds an iroh endpoint, generates a blob ticket, POSTs a Block Kit message to Slack (bot token or webhook). Process stays resident to seed until N confirmed downloads or TTL.
2. Slack message — renders file card: name, size, short hash, sender, `Receive` button (copies `sideband get <ticket>` to clipboard via Block Kit button + ephemeral response), and expiry.
3. `sideband get <ticket>` — dials the sender's endpoint key via iroh (direct QUIC, hole-punched; relay fallback), streams blob with resume support, verifies BLAKE3, writes to `./` or `--out`.
4. `sideband bot` (optional resident daemon) — Socket Mode listener. Handles `/drop` slash command (opens file-path modal for machines running the daemon), button interactions, and posts receive confirmations back to the thread.

Components:

| Component | Runs where | Talks to |
|---|---|---|
| `sideband` CLI (send/get) | each user's machine | iroh relays + peers, Slack Web API |
| `sideband bot` | any one machine or small VM | Slack Socket Mode (no public HTTP endpoint) |
| iroh public relays (n0) | n0 infra (v1) | fallback transport only, E2E encrypted |

> 💡 Design rationale: bot is optional in v1. The CLI alone posting via bot token delivers the full demo. Socket Mode avoids exposing any HTTP endpoint — critical for a "no servers" story.

---

## Recommended Stack

| Layer | Chosen | Signal | Why | Rejected |
|---|---|---|---|---|
| P2P transport | `iroh` 1.x + `iroh-blobs` | 1.0 stable Jun 2026, ~10k stars, 200M endpoint conns/mo | Dial-by-key, hole punching, relay fallback, BLAKE3 verified resumable transfer out of the box | `libp2p` (heavier, no blobs story), `magic-wormhole` (no resume, no resident ticket model) |
| Slack client | `slack-morphism` 2.x | 218 stars, releases through May 2026, typed Block Kit | Only maintained Rust crate with Web API + Socket Mode + Block Kit | Bolt-Python sidecar (second runtime kills single-binary story), raw `reqwest` (re-implement Socket Mode WS) |
| CLI | `clap` 4 (derive) | de facto standard | — | `argh`, `pico-args` |
| Async runtime | `tokio` | required by iroh + slack-morphism | — | — |
| Config | env vars + `~/.config/sideband/config.toml` (`figment`) | — | XDG-friendly, env override for CI | — |
| License keys (paid tier, M3+) | `ed25519-dalek` offline verification | — | Emmanuel's established Lemon Squeezy + Ed25519 pattern; no license server | hosted license API |

Sources: iroh.computer docs, docs.rs/iroh-blobs, github.com/n0-computer/iroh, lib.rs/crates/slack-morphism, slack-rust.abdolence.dev/socket-mode.html.

---

## Repository Layout

```
sideband/
├── Cargo.toml              # workspace: sideband (bin), sideband-core (lib)
├── crates/
│   ├── sideband-core/      # ticket model, transfer engine, slack client wrapper
│   │   └── src/
│   │       ├── ticket.rs   # Ticket {blob_ticket, meta, expiry} + serde + compact encoding
│   │       ├── transfer.rs # send/seed/get over iroh-blobs; progress events
│   │       ├── slack.rs    # post card, update card, socket-mode handlers
│   │       └── config.rs   # figment config + env
│   └── sideband/           # bin: clap CLI (send, get, bot, doctor)
├── tests/                  # integration: two in-process iroh endpoints, mock Slack
├── docs/
│   ├── DEVELOPER.md        # build, release, architecture notes
│   └── BURNLOG.md          # mistakes log (agent-maintained)
├── CLAUDE.md               # agent invariants (generate via ai-agent-md-architect)
├── LICENSE                 # MIT
└── README.md               # this file → becomes Type C OSS README at M2
```

---

## Prerequisites

| Requirement | Version | Notes |
|---|---|---|
| Rust | 1.78+ | `rustup` |
| Slack app | — | Socket Mode enabled; scopes: `chat:write`, `commands`, `files:read` (metadata only) |
| Tokens | — | `xoxb-` bot token; `xapp-` app token (bot mode only) |

## Quick Start

```bash
git clone https://github.com/ejoliet/sideband && cd sideband
cargo build --release
export SLACK_BOT_TOKEN=xoxb-...          # never committed; .env is gitignored
sideband send ./big.tar --channel data   # posts card, stays resident seeding
# teammate, anywhere:
sideband get <ticket> --out ~/Downloads
```

## Configuration Reference

| Var / key | Type | Default | Required | Purpose |
|---|---|---|---|---|
| `SLACK_BOT_TOKEN` | string | — | yes (send/bot) | post messages |
| `SLACK_APP_TOKEN` | string | — | bot mode only | Socket Mode WS |
| `SIDEBAND_CHANNEL` | string | — | no | default channel |
| `SIDEBAND_TTL_HOURS` | u32 | 24 | no | seeding duration |
| `SIDEBAND_MAX_DOWNLOADS` | u32 | 0 (unlimited) | no | stop seeding after N gets |
| `SIDEBAND_RELAY_URL` | url | n0 public relays | no | self-hosted relay (paid/self-host story) |
| `SIDEBAND_LICENSE_KEY` | string | — | M3+ only | Ed25519-signed team license, verified offline |

## CLI Contract

```text
sideband send <PATH> [--channel <name>] [--ttl <h>] [--max-downloads <n>] [--quiet]
sideband get  <TICKET> [--out <dir>]        # resumable; re-run to resume
sideband bot                                # Socket Mode daemon: /drop, buttons, confirmations
sideband doctor                             # connectivity: relay reach, hole-punch test, slack auth test
```

Exit codes: `0` ok, `2` config/auth, `3` network/relay unreachable, `4` hash mismatch, `5` ticket expired.

## Error Handling

| Error | Behavior |
|---|---|
| `TicketExpired` | `get` fails fast with human message; card auto-updates to "expired" if bot running |
| `HashMismatch` | delete partial, exit 4, never keep unverified bytes |
| `RelayUnreachable` | retry with backoff 3×, then exit 3 with `sideband doctor` hint |
| Slack API 429 | honor `Retry-After`; card posting is best-effort, transfer never blocks on Slack |
| Sender offline mid-transfer | `get` keeps partial; resume on re-run (iroh-blobs range requests) |

## Testing

- `cargo test` — unit + integration. Integration spins two in-process iroh endpoints (loopback, no relay) and transfers a 10 MB random blob; Slack mocked with `wiremock`.
- `cargo test --features relay-e2e` — optional, hits n0 public relay (CI-skipped by default).
- Never hit real Slack in tests.

## Security

- No secrets in repo: tokens via env / gitignored `.env` only. Run `ship-check` gate before any public push (cullroom incident rule).
- Payloads: E2E encrypted (QUIC/TLS 1.3, iroh); relays cannot decrypt.
- Ticket in Slack = capability token: anyone in the channel can fetch. Document this loudly. `--max-downloads` and TTL are the v1 mitigations; per-recipient encryption is v2.
- License verification fully offline (Ed25519 pubkey embedded in binary).

## Non-Goals (v1)

- No P2P chat, channel mirroring, or history archive (that's the M4+ gossip story).
- No mobile receivers.
- No per-recipient access control beyond TTL/max-downloads.
- No Slack App Directory listing (M4).
- No self-hosted relay automation (documented manually only).

## Open Questions

1. Receive UX for users without the CLI installed: show `curl | sh` installer in the ephemeral response, or ship signed binaries + `brew tap`? (Decide at M1; default: both, brew first.)
2. Does the `Receive` button copy the command (Block Kit limitation: needs ephemeral text fallback) — verify interaction pattern during M1.
3. Slack App Directory review stance on "files bypass Slack storage" — research before M4; fallback is manifest-based install (works fine without Directory).
4. Rename check: confirm `sideband` is free on crates.io, GitHub, and as a Slack app name before M2.

---

## Agent Build Instructions

> Implement end-to-end using only this README. Resolve Open Questions 1–2 before M1 exit. Maintain `docs/BURNLOG.md`. `AIDEV-` comments for non-obvious logic only.

### Build Order

| Phase | Deliverable | Done when |
|---|---|---|
| 0 | Workspace scaffold, CI (fmt+clippy+test), CLAUDE.md | `cargo clippy -- -D warnings` passes |
| 1 | `sideband-core::transfer` — send/seed/get over iroh-blobs | in-process 10 MB transfer test passes with resume |
| 2 | `ticket` compact encoding + `send`/`get` CLI | round-trip on two machines on different networks |
| 3 | Slack card post + update (`slack.rs`) | card renders in real workspace; transfer independent of Slack availability |
| 4 | `sideband bot` Socket Mode: `/drop`, buttons, confirmations | button flow works end-to-end |
| 5 | `doctor`, error taxonomy, signed release binaries (macOS arm64/x64, Linux x64) | `sideband doctor` green on fresh machine; GitHub Release artifacts |

### Constraints

- Rust 2021, `#![deny(warnings)]` in CI, typed everywhere.
- Transfer path must never block on Slack API calls (spawn, best-effort).
- Socket Mode callbacks ack within 2 s — spawn long work (Slack retries otherwise).
- Binary size sanity: `--release` + `strip`; target < 25 MB.
- Secrets via env only; tests offline by default.

### Acceptance Criteria

- [ ] 10 GB file transfers between two real networks with one mid-transfer interrupt + resume, hash verified.
- [ ] Slack down/rate-limited → transfer still completes.
- [ ] `sideband doctor` diagnoses: bad token, blocked UDP, expired ticket.
- [ ] Fresh-machine install-to-first-transfer ≤ 5 minutes following README only.
- [ ] `ship-check` passes before repo goes public.

---

## Milestones & Business Gates

| M | Scope | Gate (GO/NO-GO) |
|---|---|---|
| **M0 Spike** (day 1) | iroh-blobs transfer between two networks + ticket posted to Slack via webhook | GO if cross-network transfer works with relay fallback; NO-GO kills project cheap |
| **M1 MVP** (week 1) | Build phases 0–3; signed binaries; brew tap | GO if a non-Emmanuel human completes install→transfer unaided |
| **M2 Launch** (week 2) | Type C README, launch GIF, HN "Show HN" + r/rust + r/selfhosted post, manifesto blog post | Signal gate: ≥300 GitHub stars OR ≥20 workspaces running it within 2 weeks |
| **M3 Monetize** (only on M2 GO) | Team license (Ed25519 + Lemon Squeezy): private relay config, admin policy file (allowed channels, max TTL), audit log (local JSONL) | GO if ≥3 unsolicited "can we pay for audit/compliance/private relay" asks |
| **M4 Product** (only on M3 GO) | Slack App Directory, per-recipient encryption, hosted admin dashboard, team directory | First 10 paying workspaces |

> ⚠️ Do not build M3 features speculatively. The asks must come in first.

## Business Model (summary — see mentor notes)

| Tier | Price | Gets |
|---|---|---|
| Free / OSS | $0, MIT | full CLI + bot, public n0 relays, unlimited personal/team use |
| Team | $4/user/mo or $290/yr/workspace (Lemon Squeezy) | private relay support, policy file, local audit log, priority fixes |
| Business (M4) | custom | hosted admin, SSO, compliance exports, Slack Directory install |

## Next Steps

1. Run M0 spike today: `iroh-blobs` example + webhook post. GO/NO-GO by tonight.
2. Resolve Open Question 4 (name availability) before writing more code.
3. Generate `CLAUDE.md` via `ai-agent-md-architect` at Phase 0.
4. Hand this README to the coding agent; agent starts at Build Order Phase 0.
