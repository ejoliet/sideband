#!/usr/bin/env bash
#
# ship-check — the gate that runs before this repository is pushed anywhere
# public, and before every release.
#
# The rule it enforces: nothing token-shaped has ever been committed. A secret
# that was added and later deleted is still in the history, and history is what
# a `git push` publishes, so the working tree alone is not enough to check.
#
# Usage: scripts/ship-check.sh [--fast]
#   --fast   skip the history scan and the test suite (pre-commit use)
#
# Exit 0 clean, 1 dirty. Never prints a matched secret, only where it lives.

set -euo pipefail

fast=0
[[ "${1:-}" == "--fast" ]] && fast=1

cd "$(git rev-parse --show-toplevel)"

failures=0
fail() {
	printf '  FAIL  %s\n' "$*" >&2
	failures=$((failures + 1))
}
pass() { printf '  ok    %s\n' "$*"; }

# Each pattern is written so that this file does not match itself: every one
# requires a real character run after the prefix, which the bracket expressions
# below do not provide.
patterns=(
	'xox[bpars]-[A-Za-z0-9]{8}'
	'xapp-[A-Za-z0-9]{8}'
	'hooks\.slack\.com/services/[A-Z0-9]{5}'
	'BEGIN (RSA|OPENSSH|EC|PGP) PRIVATE KEY'
	'AKIA[0-9A-Z]{12}'
)
joined=$(
	IFS='|'
	echo "${patterns[*]}"
)

echo "ship-check: working tree"
# Tracked *and* untracked-but-not-ignored: an uncommitted file is one `git add`
# away from being published, so it counts. .env.example names the variables and
# holds no values, so it is the one exemption.
hits=$(
	git ls-files -co --exclude-standard -z |
		grep -zv '^\.env\.example$' |
		xargs -0 grep -lIE "$joined" 2>/dev/null || true
)
if [ -n "$hits" ]; then
	echo "$hits" >&2
	fail "something token-shaped is in a file that would be published (listed above)"
else
	pass "no token-shaped strings in the working tree"
fi

if git ls-files --error-unmatch .env >/dev/null 2>&1; then
	fail ".env is tracked; it must be gitignored"
else
	pass ".env is not tracked"
fi

if git check-ignore -q .env; then
	pass ".env is ignored"
else
	fail ".env is not covered by .gitignore"
fi

if [[ $fast -eq 0 ]]; then
	echo "ship-check: history"
	# A tiny repository, so a full-history scan is affordable and worth it.
	if git log --all -p --no-color | grep -qE "$joined"; then
		fail "something token-shaped appears in the git history; rewrite it before pushing"
	else
		pass "no token-shaped strings in the git history"
	fi
fi

echo "ship-check: build gates"
cargo fmt --all -- --check && pass "cargo fmt" || fail "cargo fmt --all -- --check"
cargo clippy --all-targets --all-features -- -D warnings 2>/dev/null &&
	pass "cargo clippy" || fail "cargo clippy --all-targets --all-features -- -D warnings"

if [[ $fast -eq 0 ]]; then
	# The suite must pass with no Slack or relay credentials in the environment:
	# anything that needs them is a test that would fail on a stranger's machine.
	# Output is kept and replayed on failure: a gate that hides which test broke
	# is a gate you learn nothing from.
	log=$(mktemp -t sideband-ship-check)
	if env -u SLACK_BOT_TOKEN -u SLACK_APP_TOKEN cargo test --workspace >"$log" 2>&1; then
		pass "cargo test --workspace (no credentials in env)"
		rm -f "$log"
	else
		cat "$log" >&2
		fail "cargo test --workspace (output above, kept at $log)"
	fi
fi

if [[ $failures -gt 0 ]]; then
	printf '\nship-check: %d failure(s) — do not push\n' "$failures" >&2
	exit 1
fi
printf '\nship-check: clean\n'
