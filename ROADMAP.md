# BooruCraft roadmap

**Stage:** pre-alpha. The desktop app opens folders, shows a thumbnail grid and a viewer, searches
with tags and ratings, and edits tags (saved to `collection.json` and XMP sidecars). Design:
[docs/design.md](docs/design.md). Ranked work list: [docs/gaps.md](docs/gaps.md).

| Milestone | Goal | Status |
|---|---|---|
| M0 | Repo, Podman gate (copy PlayCraft's scripts), workspace, Rust-only dependency check, central `tags.json` and `collection.json` formats with atomic save, backup and lock, in-memory tag index, XMP sidecar read/update that keeps other apps' fields, scanner (hash, metadata, path tracking), `boorucraft-cli scan / info / tag / search` with plain tags | done |
| M1 | Desktop app: add and open collection folders, thumbnail cache and worker pool, virtualised grid, viewer (zoom, pan, next/prev, animation), open unsupported formats in the default app, tag editor with the central tag list and "Create tag", bulk tagging, ratings | done |
| M2 | Full search syntax (negation, OR, wildcards, every metatag, `order:`), error underline, autocomplete, tag sidebar with counts, namespaces and colours | |
| M3 | Duplicates: dHash/pHash, BK-tree, `dupes:` metatag, review screen (keep, merge tags, trash, not-duplicates) | |
| M4 | File watching, reload on outside changes, missing-file handling, Tags screen (rename/merge/delete across all collections, catch-up for closed ones), Unknown tags review, reading back sidecars edited by other apps, rebuild from sidecars, saved searches, slideshow | |
| Later | Tag aliases and implications; import tags from boorus by MD5; local ML auto-tagger; pools/collections; video thumbnails via PlayCraft's Rust decoders | |

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
