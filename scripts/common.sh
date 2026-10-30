#!/usr/bin/env bash
# Shared settings for the container wrapper scripts. Source, do not execute.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${BOORUCRAFT_IMAGE:-localhost/boorucraft-dev:latest}"
# Personal settings (not committed): scripts/local.env, for example
#   BOORUCRAFT_PICTURES_HOST_DIR=/path/to/pictures
if [[ -f "$REPO_ROOT/scripts/local.env" ]]; then
    # shellcheck source=/dev/null
    source "$REPO_ROOT/scripts/local.env"
fi
# A pictures folder to mount read-only at /pictures for manual testing. Unset: none.
PICTURES_HOST_DIR="${BOORUCRAFT_PICTURES_HOST_DIR:-}"

# PlayCraft's checkout, for its video decoders (a path dependency at ../playcraft).
PLAYCRAFT_DIR="${BOORUCRAFT_PLAYCRAFT_DIR:-$(cd "$REPO_ROOT/.." && pwd)/playcraft}"

# Reads the pinned toolchain version from rust-toolchain.toml.
rust_version() {
    sed -n 's/^channel *= *"\(.*\)"/\1/p' "$REPO_ROOT/rust-toolchain.toml"
}

# Builds the podman run argument list into the RUN_ARGS array.
# Usage: podman_args <network: none|default>
podman_args() {
    local network="$1"
    RUN_ARGS=(
        --rm
        --userns=keep-id
        --network="$network"
        --memory=8g
        --cpus=4
        --security-opt=no-new-privileges
        -v "$REPO_ROOT":/work:Z
        -w /work
        # :U chowns the named volumes to the container user so cargo can write them.
        -v boorucraft-cargo-registry:/usr/local/cargo/registry:U
        -v boorucraft-cargo-git:/usr/local/cargo/git:U
        -v boorucraft-target:/work/target:U
        -e CARGO_TARGET_DIR=/work/target
        -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}"
        -e RUST_BACKTRACE=1
        -e HOME=/tmp
    )
    if [[ "$network" == "none" ]]; then
        RUN_ARGS+=(-e CARGO_NET_OFFLINE=true)
    fi
    # Always read-only, and no relabel option: relabeling would write to the folder.
    if [[ -n "$PICTURES_HOST_DIR" && -d "$PICTURES_HOST_DIR" ]]; then
        RUN_ARGS+=(-v "$PICTURES_HOST_DIR":/pictures:ro)
    fi
    # Read-only and not relabelled, like the pictures folder.
    if [[ -d "$PLAYCRAFT_DIR" ]]; then
        RUN_ARGS+=(-v "$PLAYCRAFT_DIR":/playcraft:ro)
    fi
    if [[ -t 0 && -t 1 ]]; then
        RUN_ARGS+=(-it)
    fi
}
