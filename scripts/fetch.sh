#!/usr/bin/env bash
# Downloads crate sources into the cargo cache volumes. The only step that needs network.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
DEV_NETWORK=1 exec "$REPO_ROOT/scripts/dev.sh" cargo fetch "$@"
