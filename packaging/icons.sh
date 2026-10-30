#!/usr/bin/env bash
# Regenerate every derived app icon from assets/app-icon/boorucraft.svg (the canonical artwork; a
# 512-unit full-bleed tile with rx=112). Adapted from FilmCraft's.
#
# Needs resvg (`cargo install resvg`; inside Podman: scripts/dev.sh packaging/icons.sh after
# installing it into .cache/tools). The outputs are committed, so builds and packaging never
# need it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
SVG="$DIR/boorucraft.svg"
APP_ID=io.github.yuki_init.boorucraft
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

RESVG="$(command -v resvg || true)"
[[ -n "$RESVG" ]] || RESVG="$ROOT/.cache/tools/bin/resvg"
[[ -x "$RESVG" ]] || { echo "error: resvg not found (cargo install resvg)" >&2; exit 1; }

# macOS icons follow Apple's grid: the tile body is 824 of 1024 px with a transparent margin, so
# the Dock shows it at the same size as other apps. 512 * 1024 / 824 = 636.27 units, margin 62.14.
MAC="$TMP/mac.svg"
sed 's/viewBox="0 0 512 512"/viewBox="-62.14 -62.14 636.27 636.27"/' "$SVG" >"$MAC"
grep -q 'viewBox="-62.14' "$MAC" || { echo "error: unexpected viewBox in $SVG" >&2; exit 1; }

render() { "$RESVG" -w "$2" -h "$2" "$1" "$3" </dev/null; }

render "$SVG" 1024 "$DIR/boorucraft-1024.png"
# The window and Dock icon on macOS (apps/boorucraft/src/main.rs embeds it).
render "$MAC" 512 "$DIR/boorucraft-macos-512.png"

# Linux hicolor theme. Windows and Linux use the full-bleed tile.
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$APP_ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SVG" "$DIR/hicolor/scalable/apps/$APP_ID.svg"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$SVG" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/boorucraft.ico" "${ICO_PNGS[@]}")

# macOS .icns.
ICNS_PNGS=()
for s in 16 32 64 128 256 512 1024; do
  render "$MAC" "$s" "$TMP/icns-$s.png"
  ICNS_PNGS+=("$TMP/icns-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- icns "$DIR/boorucraft.icns" "${ICNS_PNGS[@]}")
echo "icons written to $DIR"
