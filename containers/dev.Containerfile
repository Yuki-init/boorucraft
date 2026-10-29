# Development image for building and testing BooruCraft.
# Build with scripts/build-image.sh, which passes RUST_VERSION from rust-toolchain.toml.
#
# BooruCraft uses Rust crates only. build-essential is here for the system linker, not to compile
# C code; `cargo xtask rust-only` fails if a build compiles C or links a native library.
# The X11/Wayland/Vulkan packages are run-time libraries the window system loads (dlopen), plus
# Xvfb and Mesa's software Vulkan for headless screenshots.
FROM docker.io/library/debian:trixie-slim

ARG RUST_VERSION
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git build-essential \
        libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon0 libxkbcommon-x11-0 \
        libwayland-client0 libwayland-cursor0 libwayland-egl1 libegl1 libvulkan1 mesa-vulkan-drivers \
        xvfb xauth dbus-daemon xdg-utils \
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
