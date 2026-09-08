#!/bin/sh
# Fast pre-commit legs of `bun run check`: formatting, JS/TS lint, and
# typecheck (seconds). The full gate (`bun run check`, with clippy and the
# Rust test suite) still runs by hand before committing to `main` — see
# AGENTS.md. Install once with:
#   ln -sf ../../scripts/pre-commit.sh .git/hooks/pre-commit
# Bypass with `git commit --no-verify` in emergencies.
set -eu
cd "$(dirname "$0")/.."

cargo fmt --all --check
bun run lint
bun run typecheck
