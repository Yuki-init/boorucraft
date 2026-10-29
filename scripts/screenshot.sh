#!/usr/bin/env bash
# Runs the desktop app under Xvfb in the dev container and saves a screenshot, e.g.
#   scripts/screenshot.sh .cache/shot.png --collection .cache/sample --query "rating:s" --after 3
# Paths are relative to the repository root. Config and caches go under .cache/shot-*, so the
# user's real settings are never touched. Builds the app first.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
out="$1"; shift
# Keep bash as PID 1: xvfb-run as PID 1 can hang after the app exits.
exec "$REPO_ROOT/scripts/dev.sh" bash -c '
  set -e
  cargo build --quiet -p boorucraft
  out="$1"; shift
  export BOORUCRAFT_CONFIG_DIR=/work/.cache/shot-config BOORUCRAFT_CACHE_DIR=/work/.cache/shot-cache
  # Xvfb has no window manager, so give the window keyboard focus by hand (egui only treats a
  # text box as focused while its window is): needed to see autocomplete with --focus.
  xvfb-run -a -s "-screen 0 1280x800x24" bash -c '\''
    timeout -s KILL 90 target/debug/boorucraft "$@" &
    app=$!
    timeout 20 xdotool search --sync --name "^BooruCraft$" windowfocus >/dev/null 2>&1 || true
    wait $app
  '\'' app "$@" --screenshot "$out"
  test -f "$out" && echo "saved $out"
' screenshot "$out" "$@"
