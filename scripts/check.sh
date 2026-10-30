#!/usr/bin/env bash
# The gate GitHub Actions runs (`cargo xtask ci`: fmt, clippy, tests, layering, the Rust-only rule,
# dependency licences and bans), run locally in Podman. Fetches first; the rest is offline.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
cd "$REPO_ROOT"

echo "==> fetch (network)"
scripts/fetch.sh --quiet

scripts/dev.sh cargo xtask ci
echo "==> all checks passed"
