#!/usr/bin/env bash
# Builds the dev image. Network is allowed here (distro mirrors, rustup, crates.io).
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

version="$(rust_version)"
if [[ -z "$version" ]]; then
    echo "could not read channel from rust-toolchain.toml" >&2
    exit 1
fi

podman build \
    --build-arg RUST_VERSION="$version" \
    -t "$IMAGE" \
    -f "$REPO_ROOT/containers/dev.Containerfile" \
    "$REPO_ROOT/containers"
