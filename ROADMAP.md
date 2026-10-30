# BooruCraft roadmap

**Stage:** pre-alpha. The desktop app opens folders, shows a thumbnail grid and a viewer, searches
with the full booru syntax (tags, metatags, `order:`), edits tags (saved to `collection.json` and XMP sidecars),
finds and reviews near-duplicates, and groups files into pools and families of versions. Design:
[docs/design.md](docs/design.md). Ranked work list: [docs/gaps.md](docs/gaps.md).

| Milestone | Goal | Status |
|---|---|---|
| M0 | Repo, Podman gate (copy PlayCraft's scripts), workspace, Rust-only dependency check, central `tags.json` and `collection.json` formats with atomic save, backup and lock, in-memory tag index, XMP sidecar read/update that keeps other apps' fields, scanner (hash, metadata, path tracking), `boorucraft-cli scan / info / tag / search` with plain tags | done |
| M1 | Desktop app: add and open collection folders, thumbnail cache and worker pool, virtualised grid, viewer (zoom, pan, next/prev, animation), open unsupported formats in the default app, tag editor with the central tag list and "Create tag", bulk tagging, ratings | done |
| M2 | Full search syntax (negation, OR, wildcards, every metatag, `order:`), error underline, autocomplete, tag sidebar with counts, namespaces and colours | done |
| M3 | Duplicates: dHash/pHash, BK-tree, `dupes:` metatag, review screen (keep, merge tags, trash, not-duplicates); grouping: parent/child links and pools, stacked in the grid | done |
| M4 | File watching, reload on outside changes, missing-file handling, Tags screen (rename/merge/delete across all collections, catch-up for closed ones), Unknown tags review, reading back sidecars edited by other apps, rebuild from sidecars, saved searches, slideshow | |
| Later | Tag aliases and implications; import tags from boorus by MD5; local ML auto-tagger; video thumbnails via PlayCraft's Rust decoders | |

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
