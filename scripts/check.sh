#!/usr/bin/env bash
# Pre-commit gate, all inside Podman: fetch, then fmt, clippy, tests, layering and the Rust-only
# rule, then dependency licences and bans.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
cd "$REPO_ROOT"

echo "==> fetch (network)"
scripts/fetch.sh --quiet

echo "==> cargo xtask ci (fmt, clippy, tests, layers, rust-only)"
scripts/dev.sh cargo xtask ci

echo "==> cargo deny (licenses, bans, sources)"
scripts/dev.sh cargo deny --offline check licenses bans sources

echo "==> all checks passed"
