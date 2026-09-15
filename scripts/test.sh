#!/usr/bin/env bash
# Run the checks this project expects to pass before a change ships.
#
#   ./scripts/test.sh           formatting, lints and the unit tests
#   ./scripts/test.sh --live    also the tests that drive a running QBZ
#
# The live tests are not part of the default run: they need QBZ playing on
# the session bus and they move the real volume, so they cannot run in CI or
# on a machine where someone is listening to something.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> formatting"
cargo fmt --check

echo "==> lints"
cargo clippy --all-targets -- -D warnings

echo "==> unit tests"
cargo test

if [[ "${1:-}" == "--live" ]]; then
	echo "==> live tests (needs QBZ running)"
	if ! busctl --user status org.mpris.MediaPlayer2.com.blitzfc.qbz >/dev/null 2>&1; then
		echo "    QBZ is not on the session bus; start it and try again." >&2
		exit 1
	fi
	cargo test --release -- --ignored --nocapture
fi

echo "==> all checks passed"
