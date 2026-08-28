# CLAUDE.md — agent invariants for `sideband`

The README is the specification. If code and README disagree, the README wins
unless a deviation is recorded in `implementation-notes.md` with a `DEVIATION:`
line explaining why.

## Non-negotiables

1. **Slack never sees the payload.** Slack carries a ticket and a card. Any
   change that routes file bytes through a Slack API is out of scope.
2. **The transfer path never blocks on Slack.** Every Slack call from `send` is
   spawned and best-effort. A rate-limited, down, or unauthenticated Slack must
   not delay or fail a transfer. Test this by running `send` with a bogus
   `SLACK_BOT_TOKEN`.
3. **Never keep unverified bytes.** iroh-blobs verifies with BLAKE3 while
   streaming, so a decode failure means nothing valid was stored; `fetch` must
   return before exporting anything on error. Exit code 4.
4. **A ticket is a capability token.** Anyone in the channel can fetch the file.
   TTL and `--max-downloads` are the only v1 mitigations. Never quietly widen
   the window; never remove the "anyone in this channel" warning from the card.
5. **Secrets come from the environment only.** No token, workspace ID, or
   channel ID belongs in a committed file, a test fixture, or a log line.
6. **Tests are offline.** No test may reach Slack or an n0 relay. Loopback iroh
   endpoints use `Relays::Disabled`; anything needing a real relay goes behind
   the `relay-e2e` feature, which CI does not enable.

## Exit codes

`0` ok · `2` config/auth · `3` network/relay · `4` hash mismatch · `5` ticket
expired. These come from `SidebandError` in `crates/sideband-core/src/error.rs`;
add a variant there rather than calling `std::process::exit` anywhere else.

## Socket Mode

Slack retries a callback it does not see acknowledged within three seconds, so
every `bot` callback returns immediately and spawns the real work. slack-morphism
callbacks are `fn` pointers, not closures: shared state travels through
`SlackClientEventsListenerEnvironment::with_user_state` and comes back out via
`states.read().await.get_user_state::<T>()`.

## Working agreements

- `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` must pass.
- Comments explain *why*, and only where the reason is not on the screen. Prefix
  genuinely non-obvious ones with `AIDEV-`.
- Log every non-obvious decision in `implementation-notes.md`: what, why, what
  was rejected. Log mistakes in `docs/BURNLOG.md`.
- Verify before claiming done: paste the command and its output.
- `scripts/ship-check.sh` must pass before anything is pushed to a public remote
  or tagged for release. It is the gate for non-negotiable 5, and it reads the
  git history, not just the working tree.
