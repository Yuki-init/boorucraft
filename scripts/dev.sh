#!/usr/bin/env bash
# Runs a command in the dev container with the repo at /work. Offline by default.
# Set DEV_NETWORK=1 to allow network (only for cargo fetch / cargo update).
# Example: scripts/dev.sh cargo build
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

network=none
if [[ "${DEV_NETWORK:-0}" == "1" ]]; then
    network=default
fi
podman_args "$network"

if [[ $# -eq 0 ]]; then
    set -- bash
fi
exec podman run "${RUN_ARGS[@]}" "$IMAGE" "$@"
