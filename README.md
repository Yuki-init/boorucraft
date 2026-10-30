# BooruCraft

A desktop picture collection organiser and viewer that works like a booru: tag your pictures,
then find them with searches like `artist:foo cat -dog rating:s width:>1920 order:-date`.

- Pictures stay in your own folders. Each collection folder gets a `collection.json` holding its
  tags, so tags travel with the pictures. Files are tracked by content hash, so tags survive moves
  and renames.
- Tags are also written to XMP sidecar files, so other photo apps (digiKam, darktable,
  Lightroom) can see them.
- One central tag list keeps tags consistent across all your collections.
- Booru-style search with negation, OR, wildcards, namespaces and metatags (size, type, date,
  rating, tag count and more).
- Tag aliases (`nekomimi` → `cat_ears`) and implications (`cat_ears` → `animal_ears`).
- Finds exact and near-duplicate pictures.
- Watches your folders: files added, moved or tagged by other apps show up on their own.
- Slideshow and fullscreen viewer; saved searches.
- Videos get thumbnails (FilmCraft's pure-Rust decoders: H.264, HEVC, VP9, AV1, MPEG-2…) and
  play in your system's media player. Formats it can't show itself (AVIF, HEIC…) are tagged
  and searched too, and open in your default app.
- Rust libraries only, egui desktop UI.

Status: pre-alpha; the desktop app and the command-line tool work. See
[ROADMAP.md](ROADMAP.md) and [docs/design.md](docs/design.md).

## Download

Builds for Linux (AppImage, .deb, .rpm, Flatpak, Arch, tarball), Windows (MSI, portable zip),
macOS (DMG) and FreeBSD are attached to each [release](https://github.com/Yuki-init/boorucraft/releases).
They are made by `.github/workflows/release.yml` through the Craft suite's shared release
workflow; the scripts are in `packaging/`. Test builds are unsigned, so Windows SmartScreen and
macOS Gatekeeper will warn about them.

## Try it

With a Rust toolchain (the version in `rust-toolchain.toml` is picked up by rustup):

```sh
cargo xtask ci                         # the gate CI runs: fmt, clippy, tests, layering, licences
cargo run -p boorucraft -- --collection some-copy-of-a-folder
```

Or without installing anything, in Podman:

```sh
scripts/build-image.sh                 # once
scripts/check.sh                       # the same gate
scripts/dev.sh cargo run -p boorucraft-cli -- help

cp -r ~/Pictures/some-folder .cache/sample   # work on a copy while BooruCraft is young
scripts/run.sh --collection .cache/sample    # the desktop app, on your own display
scripts/screenshot.sh .cache/shot.png --collection .cache/sample --query "cat -dog"
```

In the app: `/` search, arrows move, `Enter` opens, `Esc` goes back, `t` edits tags, `1`/`2`/`3`
set the rating (`0` clears it), `o` opens the file in your default app, `s` starts a slideshow,
`F11` toggles fullscreen.

```text
boorucraft-cli scan DIR                         index a folder (creates DIR/collection.json)
boorucraft-cli tag DIR FILE cat -dog --create   add cat, remove dog; --create adds new tags to the list
boorucraft-cli search DIR "cat ~sky -dog rating:s"
boorucraft-cli tags rename kitty cat            every collection catches up when next opened
```
