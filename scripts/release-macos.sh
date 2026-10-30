#!/usr/bin/env bash
# Build the macOS release files on a Mac and add them to a draft GitHub release.
#
#   scripts/release-macos.sh [VERSION]
#
# The Release workflow builds every other platform and drafts release vVERSION (VERSION defaults
# to Cargo.toml's; give the one a test run used, e.g. 0.1.0-rc.1). This script builds the commit
# that draft points at, in a temporary git worktree, with packaging/macos/package.sh (a universal
# .app on a DMG, and a CLI zip), then uploads both to the draft and adds them to SHA256SUMS.txt.
#
# Needs, on the Mac: Xcode command-line tools, rustup with both Apple targets
# (`rustup target add aarch64-apple-darwin x86_64-apple-darwin` for the toolchain the build uses),
# and gh logged in with access to the repository.
#
# Signing: ad-hoc by default, so Gatekeeper warns on other Macs (right-click ▸ Open). With a
# Developer ID, set MACOS_SIGN_IDENTITY (and APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID to notarize);
# see packaging/macos/package.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${GITHUB_REPOSITORY:-Yuki-init/boorucraft}"

fail() { echo "error: $*" >&2; exit 1; }
[[ "$(uname -s)" = Darwin ]] || fail "run this on a Mac"
for tool in git gh cargo rustup lipo hdiutil codesign; do
  command -v "$tool" >/dev/null || fail "$tool not found"
done

VERSION="${1:-$(. "$ROOT/packaging/env.sh" >/dev/null && echo "$VERSION")}"
TAG="v$VERSION"

# The draft must exist: the Release workflow makes it.
draft="$(gh release view "$TAG" -R "$REPO" --json isDraft --jq .isDraft 2>/dev/null)" \
  || fail "no release $TAG in $REPO: run the Release workflow first"
[[ "$draft" = true ]] || fail "$TAG is already published; macOS files go on the draft before publishing"
COMMIT="$(gh release view "$TAG" -R "$REPO" --json targetCommitish --jq .targetCommitish)"
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || fail "draft $TAG points at '$COMMIT', not a commit"
echo "==> BooruCraft $VERSION for macOS, from $COMMIT"

git -C "$ROOT" fetch -q origin "$COMMIT" 2>/dev/null || git -C "$ROOT" fetch -q origin
WORK="$(mktemp -d "${TMPDIR:-/tmp}/boorucraft-macos.XXXXXX")"
cleanup() { git -C "$ROOT" worktree remove --force "$WORK/src" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT
git -C "$ROOT" worktree add -q --detach "$WORK/src" "$COMMIT"

cd "$WORK/src"
# A test run's version override (CI applies it the same way); then the build matches the draft.
if [[ "$(. packaging/env.sh >/dev/null && echo "$VERSION")" != "$VERSION" ]]; then
  cargo xtask version set "$VERSION"
fi
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  rustup target list --installed | grep -qx "$target" || rustup target add "$target"
done
# Keep the main checkout's build cache between releases.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/macos-release}"
export DIST="$WORK/dist"
packaging/macos/package.sh --arch universal

shopt -s nullglob
files=("$DIST"/*.dmg "$DIST"/*.zip)
[[ "${#files[@]}" -eq 2 ]] || fail "expected a DMG and a zip in $DIST, found: ${files[*]}"
gh release upload "$TAG" -R "$REPO" --clobber "${files[@]}"

# Add their checksums to SHA256SUMS.txt, replacing older lines for the same names.
sums="$WORK/SHA256SUMS.txt"
gh release download "$TAG" -R "$REPO" -p SHA256SUMS.txt -O "$sums" 2>/dev/null || : >"$sums"
for f in "${files[@]}"; do
  name="$(basename "$f")"
  grep -v "  $name\$" "$sums" >"$sums.new" || true
  echo "$(shasum -a 256 "$f" | cut -d' ' -f1)  $name" >>"$sums.new"
  mv "$sums.new" "$sums"
done
gh release upload "$TAG" -R "$REPO" --clobber "$sums"
echo "==> added to draft $TAG:"
for f in "${files[@]}"; do echo "    $(basename "$f")"; done
echo "Publish it on GitHub once you have checked it: https://github.com/$REPO/releases"
