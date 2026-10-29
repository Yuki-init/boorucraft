# Gaps: ranked work list

Pick from the top. Each item names its milestone.

1. **M1.1 Desktop shell**: `ui-egui` crate and `boorucraft` app (eframe, wgpu); open and
   remember collection folders (`~/.config/boorucraft/config.json`); a scan with progress on a
   background thread.
2. **M1.2 Thumbnails**: `image` crate (Rust decoders only) + `jxl-oxide`; cache at
   `~/.cache/boorucraft/thumbs/ab/cdef….jpg`; worker pool; placeholder icons with a format label
   for files the app can't decode.
3. **M1.3 Grid and search bar**: virtualised grid, multi-select, the M0 query language with error
   underline, result count.
4. **M1.4 Viewer**: fit / 1:1 / zoom / pan, next/previous in results, GIF/WebP animation; "Open in
   default app" (`open` crate) and "Show in file manager"; unsupported formats open externally.
5. **M1.5 Tag editor**: per file and bulk, autocomplete from the central list, "Create tag",
   ratings with `1`/`2`/`3`; sidecars written after each change (debounced).
6. **M2**: metatags (`width:`, `date:`, `type:`, `tagcount:`, `path:"…"`, `order:`…), quoted
   values, autocomplete in the search bar, tag sidebar with counts and namespace colours.
7. **M3**: perceptual hashes (dHash, pHash), BK-tree, `dupes:` metatag, review screen.
8. **M4**: `notify` file watching, reload on outside changes to `collection.json` / `tags.json`,
   Tags screen, Unknown tags screen, rebuild from sidecars, saved searches, slideshow.

## Known limitations

- Sidecars in `Stem` mode (`cat.xmp`): two files with the same stem in one folder (`cat.jpg`,
  `cat.png`) share one sidecar, and the last write wins.
- One-level `lr:hierarchicalSubject` entries written by other apps are kept as they are, even
  after the matching tag is removed in BooruCraft.
- Stale-lock detection only works on Linux; elsewhere the error message says which lock file to
  delete.
- Camera raw files get no width/height (their headers describe a preview).
- Animated GIF detection reads only the first 64 KiB.
