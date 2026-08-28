# BURNLOG

Mistakes made while building `sideband`, so they are not made twice.

## 2026-08-28 — MSRV silently downgraded every dependency

Set `rust-version = "1.78"` in the workspace because the README's prerequisites
table says 1.78. Cargo's MSRV-aware resolver then quietly picked iroh 0.29 and
iroh-blobs 0.29 instead of the pinned 1.1 and 0.103, and the code failed to
compile against an API surface that no longer existed.

`cargo info iroh@1.1.0` shows `rust-version: 1.91`. **A version pin that
conflicts with `rust-version` loses.** When cargo resolves a version far below
what was asked for, check MSRV before suspecting the pin.

## 2026-08-28 — trusted a documentation snippet over the vendored source

Used `ExportOptions { hash, path, mode, ranges }` from a docs snippet. In
iroh-blobs 0.103 `ExportOptions` is an alias for `ExportPathRequest`, whose
fields are `hash`, `mode` and `target` — no `path`, no `ranges`.

For any crate released after the model's knowledge cutoff, read
`~/.cargo/registry/src/*/<crate>-<version>/src/` and treat every other source as
a hint.

## 2026-08-28 — reached for `rand` to generate a secret key

Wrote `SecretKey::generate(&mut rand::rngs::OsRng)`, which meant adding `rand`
and matching whatever `rand_core` version iroh 1.1 links. iroh's endpoint builder
generates a key when none is set, so the dependency was pure risk for no
functionality. The test helpers wanted a *fixed* key anyway
(`SecretKey::from_bytes(&[7u8; 32])`), which is both simpler and more
deterministic.

Before adding a crypto or RNG dependency, check whether the library already does
the thing.

## 2026-08-28 — `endpoint.online()` hangs forever with relays disabled

The first loopback test never finished. `Endpoint::online()` waits for a *home
relay*, and `RelayMode::Disabled` means there will never be one, so the wait is
unbounded rather than an error. Fixed by making `reachable_addr` return the local
direct addresses immediately for `Relays::Disabled`.

A "wait until ready" API can have a configuration in which ready is unreachable.
Always ask what happens when the thing being waited for cannot exist.

## 2026-08-28 — threw away the output of the only failing test run

`scripts/ship-check.sh` ran `cargo test --workspace >/dev/null`. `tests/transfer.rs`
failed once, on a machine busy finishing a 15-minute release build, and the only
surviving evidence was `error: test failed`. Nine subsequent runs of the suite
passed, so the flake is unidentified and unfixed — the timing-sensitive
cancellation in `resumes_a_partial_transfer_on_re_run` and the `assert_eq!(last,
TEN_MB)` in the 10 MB test are both plausible and neither is confirmed.

Fixed the gate, not the guess: the suite's output is captured to a temp file and
replayed on failure. **Never send the output of a verification step to
`/dev/null`.** The one run that fails is the only one whose output mattered.
