# Development image for building and testing BooruCraft.
# Build with scripts/build-image.sh, which passes RUST_VERSION from rust-toolchain.toml.
#
# BooruCraft uses Rust crates only. build-essential is here for the system linker, not to compile
# C code; `cargo xtask rust-only` fails if a C build tool crate enters Cargo.lock.
FROM docker.io/library/debian:trixie-slim

ARG RUST_VERSION
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git build-essential \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN test -n "$RUST_VERSION" \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION" \
    && rustup component add rustfmt clippy \
    && cargo install --locked cargo-deny \
    && rm -rf /usr/local/cargo/registry /usr/local/cargo/git \
    && chmod -R a+rwX /usr/local/rustup /usr/local/cargo

WORKDIR /work
