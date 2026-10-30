# shellcheck shell=bash
# Shared setup for the packaging scripts (adapted from FilmCraft's). Source it:
# `. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"`. The shared release workflow's resolve-version
# action also sources it, for VERSION.
#
# Exports:
#   ROOT                     workspace root
#   VERSION                  [workspace.package] version from Cargo.toml (override: BOORUCRAFT_VERSION)
#   APP_ID                   reverse-DNS id: .desktop file, icon name, Wayland app id, Flatpak id
#   DIST                     output directory for release files (default: $ROOT/dist/release)
#   BOORUCRAFT_BUILD_SHA     git commit being packaged
#   BOORUCRAFT_BUILD_DATE    UTC build date, YYYY-MM-DD
#   CARGO_TARGET_DIR         cargo's target dir (default: $ROOT/target)

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ROOT
export APP_ID=io.github.yuki_init.boorucraft

# The version lives in exactly one place: `[workspace.package] version` in the root Cargo.toml.
# (`cargo xtask version` prints the same thing; awk avoids compiling xtask here.)
workspace_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[workspace.package]") ; next }
    in_pkg && $1 == "version" { gsub(/[" ]/, "", $3); print $3; exit }
  ' "$ROOT/Cargo.toml"
  return 0
}

VERSION="${BOORUCRAFT_VERSION:-$(workspace_version)}"
if [[ -z "$VERSION" ]]; then
  echo "error: could not read [workspace.package] version from $ROOT/Cargo.toml" >&2
  exit 1
fi
export VERSION

DIST="${DIST:-$ROOT/dist/release}"
mkdir -p "$DIST"
export DIST

if [[ -z "${BOORUCRAFT_BUILD_SHA:-}" ]]; then
  BOORUCRAFT_BUILD_SHA="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
fi
export BOORUCRAFT_BUILD_SHA
export BOORUCRAFT_BUILD_DATE="${BOORUCRAFT_BUILD_DATE:-$(date -u +%Y-%m-%d)}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

# Emit a GitHub Actions warning (plain stderr outside Actions).
warn() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::warning::$*"; else echo "warning: $*" >&2; fi
  return 0
}

# Copy the readme, licences and third-party notices into a package directory.
copy_docs() {
  local dest="$1" f
  for f in README.md LICENSE-MIT LICENSE-APACHE ATTRIBUTION.md; do
    if [[ -f "$ROOT/$f" ]]; then cp "$ROOT/$f" "$dest/"; fi
  done
  # The fonts the app embeds, with their licences.
  for f in "$ROOT"/assets/fonts/*-OFL.txt; do
    [[ -f "$f" ]] && cp "$f" "$dest/"
  done
  return 0
}
