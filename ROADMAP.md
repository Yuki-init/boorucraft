# BooruCraft roadmap

**Stage:** pre-alpha. The desktop app opens folders, shows a thumbnail grid and a viewer, searches
with the full booru syntax (tags, metatags, `order:`), edits tags (saved to `collection.json` and XMP sidecars),
finds and reviews near-duplicates, groups files into pools and families of versions, follows tag
aliases and implications, watches folders for outside changes, and shows video thumbnails. Design:
[docs/design.md](docs/design.md). Ranked work list: [docs/gaps.md](docs/gaps.md).

| Milestone | Goal | Status |
|---|---|---|
| M0 | Repo, Podman gate (copy PlayCraft's scripts), workspace, Rust-only dependency check, central `tags.json` and `collection.json` formats with atomic save, backup and lock, in-memory tag index, XMP sidecar read/update that keeps other apps' fields, scanner (hash, metadata, path tracking), `boorucraft-cli scan / info / tag / search` with plain tags | done |
| M1 | Desktop app: add and open collection folders, thumbnail cache and worker pool, virtualised grid, viewer (zoom, pan, next/prev, animation), open unsupported formats in the default app, tag editor with the central tag list and "Create tag", bulk tagging, ratings | done |
| M2 | Full search syntax (negation, OR, wildcards, every metatag, `order:`), error underline, autocomplete, tag sidebar with counts, namespaces and colours | done |
| M3 | Duplicates: dHash/pHash, BK-tree, `dupes:` metatag, review screen (keep, merge tags, trash, not-duplicates); grouping: parent/child links and pools, stacked in the grid | done |
| M4 | File watching, reload on outside changes, missing-file handling, Tags screen (rename/merge/delete across all collections, catch-up for closed ones), Unknown tags review, reading back sidecars edited by other apps, rebuild from sidecars, saved searches, slideshow | done |
| Polish | Scan progress, preloading in the viewer, video thumbnails (FilmCraft's decoders), tag aliases and implications | done |
| Later | Import tags from boorus by MD5; local ML auto-tagger | |

## Progress log

- 2026-10-29 — Plan written: desktop egui app, pictures indexed in place, booru search syntax and
  duplicate detection in the first version.
- 2026-10-29 — Design changed: Rust libraries only; each collection folder stores its data in
  `collection.json` instead of a SQLite database; formats the app can't decode open in the
  system's default app.
- 2026-10-29 — Design changed: tags are mirrored to XMP sidecars, and one central tag list
  (`~/.config/boorucraft/tags.json`) defines valid tags for every collection.
- 2026-10-29 — M0: workspace and Podman gate (`scripts/check.sh`: fmt, clippy, tests, layers,
  Rust-only check on Cargo.lock, `cargo deny`). `core` (tag names, ratings, query parser with
  error spans), `store` (central `tags.json` with rename/merge/delete history, `collection.json`,
  atomic saves with `.bak`, lock files, outside-change detection, roaring-bitmap search),
  `xmp` (sidecar read/update that keeps other apps' fields and keywords), `index` (parallel
  scanning, SHA-256 + MD5, header metadata, EXIF dates, moved/edited/missing files, sidecar
  read-back), `boorucraft-cli`. Tried on a copy of real darktable shoots: 59 files indexed in
  0.18 s, rescan 3 ms, darktable sidecars kept intact. Hash switched from BLAKE3 to SHA-256 (the
  `blake3` crate needs `cc`).
- 2026-10-29 — M1: desktop app (`boorucraft`, eframe + wgpu, Rust only). New `thumbs` crate
  (`image` Rust decoders + `jxl-oxide`, thumbnail cache keyed by SHA-256, capped sizes, panics
  caught); `ui-egui` with collections (add, remember, rescan, close; background scans), a worker
  pool for thumbnails (newest request first), a virtualised grid with click / Ctrl / Shift and
  keyboard selection, the search bar with error messages, a viewer (fit, 1:1, wheel zoom, drag
  pan, previous/next, GIF/WebP/APNG animation), "Open in default app" / "Show in folder" and
  external opening for formats BooruCraft can't decode, a tag editor for one file or many
  (suggestions from the central list, "Create and add", namespace colours, `-tag` removes),
  ratings (`1`/`2`/`3`/`0`), sources, and saves 2 s after the last edit (collection.json, then
  sidecars). A failed save keeps the edits, says so and retries; it never overwrites a version
  written by another program. `cargo xtask rust-only` now also checks what the build links:
  only `dl` (x11-dl). `scripts/run.sh` and `scripts/screenshot.sh` added; checked with Xvfb
  screenshots against a copy of the sample folder.
- 2026-10-29 — M2: every metatag in the parser (`width`, `height`, `ratio`, `mpixels`,
  `filesize`, `type`, `kind`, `animated`, `date`, `imported`, `tagcount`, `path`, `missing`,
  `md5`, `hash`, `source`) with comparisons, ranges, units, ages and quoted values, and `order:`
  (one comparator shared by the CLI and by results merged across collections). The search bar
  colours terms, underlines the bad one and explains it with an example; autocomplete (egui's
  completion popup) offers tags by use count in namespace colours, metatag names and values;
  the tag editor completes from the central list the same way. A sidebar lists the ratings and
  tags of the current results with counts, grouped by namespace; click adds or removes a term,
  Ctrl+click excludes. Grid tiles stretch to fill the row. Fixed: focusing a text box from the
  keyboard (`/`, `t`) didn't stick, and the cursor now lands at the end. `scripts/screenshot.sh`
  gives the Xvfb window focus (xdotool) so popups show in screenshots.
- 2026-10-30 — M3 planned with grouping added: M3.1 perceptual hashes and `dupes:`, M3.2 review
  screen, M3.3 parent/child links, M3.4 pools (names in the central tag list, positions in
  `collection.json`), M3.5 stacks in the grid.
- 2026-10-30 — M3.1: new `dedup` crate (L1, pure: dHash, pHash, BK-tree, union-find groups);
  `thumbs` hashes thumbnails; `index::find_groups` groups across collections (best copy first,
  "not duplicates" respected); a background job in the app hashes new pictures after scans;
  `dupes:yes/no` in searches; `boorucraft-cli dupes DIR… [--threshold N]`. Default threshold
  lowered from 8 to 6 after a false match between two flat wallpapers on the sample copy.
- 2026-10-30 — M3.2: Duplicates screen (top bar button, `--duplicates`): groups side by side,
  choose the copy to keep, "Keep this one, trash the others" with a confirmation dialog (tags,
  rating and source copied first; files and their sidecars go to the system trash via the
  `trash` crate), "Copy tags to this one", "Not duplicates", "Show in grid", threshold slider.
  Trashing goes through a replaceable function, so tests never touch the real trash.
- 2026-10-30 — M3.3: parent/child versions: `parent` in `collection.json` (may point into
  another collection, loops refused), `parent:`/`child:` searches, "Versions" in the tag panel,
  "Make variants" on the Duplicates screen, linked versions never grouped as duplicates,
  children follow the kept copy, `boorucraft-cli tag --parent`, parent and children in `info`.
- 2026-10-30 — M3.4: pools: names in the central `tags.json` (renames and deletes in
  `history`), positions in each file's entry, `pool:` searches in pool order (`order:pool`),
  sidebar list, Pools section in the tag panel (add with autocomplete, create, remove, move
  first/earlier/later/last, `[` `]` keys), page badges in the grid, `boorucraft-cli pools`.
- 2026-10-30 — M3.5: stacks: each pool and family shows as one tile (first page / top parent)
  with a count badge; double-click opens it, Backspace or "← Back" returns; toggle in the top
  bar. M3 done.
- 2026-10-30 — M3.6: fixes from a 100-post Safebooru test copy: tags may start with `:` (booru
  face tags like `:d`, `:3`; a trailing `:` is still refused), and `boorucraft-cli` no longer
  panics when its output pipe closes early (`info DIR | head`); it finishes quietly instead.
- 2026-10-30 — M4 planned: M4.1 outside changes, M4.2 file watching, M4.3 missing files, M4.4
  Tags screen, M4.5 Unknown tags review, M4.6 rebuild from sidecars, M4.7 saved searches, M4.8
  slideshow and fullscreen.
- 2026-10-30 — M4.1: `collection.json` and `tags.json` changed by other programs are noticed
  (one `stat` per file every 2 s; content compared only when size or time differ). Reloaded
  when there are no unsaved changes (sidecars follow; tag-list renames in `history` reach open
  collections); otherwise saving stops and the collection card asks "Keep mine" / "Load theirs".
  Deleted files are written again; damaged ones are never loaded.
- 2026-10-30 — M4.2: file watching (`notify`, in `index`): collection folders and the tag list's
  folder are watched; `index::classify` ignores BooruCraft's own writes (checked against the
  scan cache) and starts an incremental rescan 1 s after a real change: pictures added, edited,
  moved or deleted, folders, and sidecars edited by other apps (read back as before).
- 2026-10-30 — M4.3: missing files: "N missing files" link on each collection card; a note in
  the tag panel explaining that tags come back with the file, and "Forget…" with a
  confirmation (`Collection::forget_missing`: only missing entries, children unlinked);
  `boorucraft-cli forget-missing DIR [--yes]`. Open/Show in folder are disabled for them.
- 2026-10-30 — M4.4: Tags screen (`--tags`): filter, sort by name or use, create, describe,
  rename, merge, delete (with confirmations); namespaces (colour, add, remove) and pools
  (rename, delete). Fixed: pool renames and deletes from the tag list didn't reindex or save
  open collections (only tag changes did); scans that were running catch up when they finish.
- 2026-10-30 — M4.5: Unknown tags review: "Unknown (n)" tab on the Tags screen listing tags in
  the open collections that aren't in the tag list, from the usage counts already kept; add
  (one or all), use a listed tag instead (with suggestions), or remove from the files.
- 2026-10-30 — M4.6: rebuild from sidecars. Fixed: with `collection.json` lost, a rescan
  skipped every sidecar (the scan cache still knew them) and the tags were silently gone; files
  new to a collection now always have their sidecars read. A damaged `collection.json` can be
  recovered from its `.bak` or rebuilt from the sidecars (collection card, `boorucraft-cli
  recover`); a copy of the damaged file is kept in the config folder first.
- 2026-10-30 — M4.7: saved searches in the sidebar (`config.json`): save the current search
  under a name, run, rename, replace, reorder, delete. Searches with errors can't be saved.
- 2026-10-30 — M4.8: slideshow (`s`, viewer bar, `--slideshow`: interval, loop, pause; timing
  starts once a picture has loaded and lets animations finish) and fullscreen (`F11`,
  `--fullscreen`: picture only, with a fading hint). M4 done.
- 2026-10-30 — P1: scans show their progress ("12 files found", then "hashing 40 / 120") on the
  collection card, in the status bar and in the empty grid. `--during-scan` lets `--screenshot`
  capture a running scan.
- 2026-10-30 — P2: the viewer and slideshow decode the next picture (in the direction of travel,
  wrapping in a looping slideshow) once the current one has loaded, so stepping on is instant.
- 2026-10-30 — P3: video thumbnails (a frame near the start, with a play badge) and video sizes
  through PlayCraft's pure-Rust MP4/Matroska demuxer and H.264 decoder (`../playcraft`, a path
  dependency). Fixed MP4/MOV files being stored as HEIC pictures; scans correct old entries.
- 2026-10-30 — L1: tag aliases (`neko` → `cat`: tagging, searching and sidecar keywords use the
  tag) and implications (`cat` → `animal`, followed through chains, loops refused; new ones
  reach files already tagged through the history), in tags.json version 2 and the CLI
  (`tags alias|unalias|imply|unimply|aliases|implications`).
- 2026-10-30 — L2: aliases and implications on the Tags screen (the tag editor lists and edits
  them; the filter matches aliases); the side panel's tag box takes aliases. A tag gained through
  a rename or merge now brings what it implies.
- 2026-10-30 — P4: videos through FilmCraft's `filmcraft-codecs` (git dependency pinned by
  commit, as the Craft apps share code) instead of PlayCraft's crates: thumbnails for H.264,
  HEVC, VP8/VP9, AV1, MPEG-2, ProRes and more, in MP4/MOV, MKV/WebM, AVI, MPEG-TS and MXF. Its
  `symphonia` dependency (MPL-2.0) is allowed by name. Dropped the compatibility code for older
  tags.json and collection.json files (nothing is published yet).
- 2026-10-30 — R2: CI on GitHub Actions through the Craft suite's shared gate
  (`storytold/craft-common-actions` `gate.yml`, as FilmCraft calls it), plus Dependabot;
  `cargo xtask ci` now ends with `cargo deny`, and `scripts/check.sh` runs that same gate in
  Podman for local work.
- 2026-10-30 — R3.1: app icon (`assets/app-icon/boorucraft.svg`, rendered to hicolor PNGs,
  `.ico` and `.icns` by `packaging/icons.sh`), window icon, app id
  `io.github.yuki_init.boorucraft`, `--version` on both binaries, Windows GUI subsystem and exe
  icon, and `cargo xtask version|ico|icns` for release packaging.
- 2026-10-30 — R3.2: release packaging. `.github/workflows/release.yml` calls the Craft suite's
  shared release workflow (as FilmCraft does): Linux tarball/deb/rpm/AppImage (`packaging/linux`),
  Flatpak bundle, Arch package, Windows MSI and portable zip (`packaging/windows`), macOS DMG
  (`packaging/macos`) and FreeBSD (the shared fallback), drafted as a GitHub Release. Unsigned
  until signing secrets are set.
- 2026-10-30 — R3.3: releases without the suite's paid macOS runner. `craft-release.yml` is
  BooruCraft's copy of the shared release workflow minus macOS (stable toolchain, unsigned
  Windows allowed, FreeBSD x86_64 only); `scripts/release-macos.sh` builds the DMG on a Mac and
  adds it to the draft. Fixed the Windows build (trash needs a COM threading feature).
- 2026-10-30 — R3.4: releases skip FreeBSD, web and 32-bit Windows (no users to justify the
  build minutes), nor riscv64: Linux x86_64/aarch64, Windows x64/arm64, macOS universal.
- 2026-10-30 — Published v0.1.0-rc.1 as a pre-release for testing (Linux and Windows; macOS to be
  added from a Mac). R3.8: pre-release Arch packages get a version the suite's arch-package
  action can read (0.1.0rc1) instead of 0.0.0; rc.1's were rebuilt the same way by hand.
- 2026-10-30 — B1: `crates/booru` looks a picture up by MD5 on Safebooru, Gelbooru (API key)
  or Danbooru and reads its tags, rating, source and post URL. HTTPS through ureq + rustls with
  the RustCrypto provider (no C/asm crypto) and webpki-roots; honest user agent, ≥1 s between
  requests, 30 s timeout, 4 MiB response cap. Tests use saved real responses; a live test
  (`-- --ignored`) checks the TLS stack against the sites.
