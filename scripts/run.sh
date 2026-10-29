#!/usr/bin/env bash
# Runs the BooruCraft desktop app on your own desktop, from the dev container: the Wayland (or
# X11) display and the GPU (/dev/dri) are passed through.
#   scripts/run.sh --collection .cache/sample           # any app options work, see --help
# For safety while developing, collections must be inside the repository (e.g. a copy under
# .cache/), and settings, the tag list and caches live in .cache/run-*, not in your home folder.
# "Open in default app" doesn't work from inside the container. Builds the release app first.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

podman_args none
RUN_ARGS+=(--device /dev/dri --group-add keep-groups)
RUN_ARGS+=(-e BOORUCRAFT_CONFIG_DIR=/work/.cache/run-config -e BOORUCRAFT_CACHE_DIR=/work/.cache/run-cache)

runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
RUN_ARGS+=(-e XDG_RUNTIME_DIR=/tmp/runtime -v "$runtime":/tmp/runtime:ro)
if [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
    RUN_ARGS+=(-e WAYLAND_DISPLAY="$WAYLAND_DISPLAY")
elif [[ -n "${DISPLAY:-}" && -d /tmp/.X11-unix ]]; then
    RUN_ARGS+=(-e DISPLAY="$DISPLAY" -v /tmp/.X11-unix:/tmp/.X11-unix:ro)
    if [[ -n "${XAUTHORITY:-}" && -f "$XAUTHORITY" ]]; then
        RUN_ARGS+=(-e XAUTHORITY=/tmp/.Xauthority -v "$XAUTHORITY":/tmp/.Xauthority:ro)
    fi
fi

# Map --collection folders to paths inside the container; refuse folders outside the repository.
args=()
next_is_dir=false
for a in "$@"; do
    if $next_is_dir; then
        next_is_dir=false
        real="$(realpath "$a")"
        if [[ "$real" != "$REPO_ROOT"/* ]]; then
            echo "run.sh: $a is outside the repository; copy it under .cache/ first" >&2
            exit 2
        fi
        args+=("/work/${real#"$REPO_ROOT"/}")
        continue
    fi
    [[ "$a" == --collection ]] && next_is_dir=true
    args+=("$a")
done

podman run "${RUN_ARGS[@]}" "$IMAGE" cargo build --quiet --release -p boorucraft
exec podman run "${RUN_ARGS[@]}" "$IMAGE" target/release/boorucraft "${args[@]}"
