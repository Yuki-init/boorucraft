# Gaps: ranked work list

Pick from the top. Each item names its milestone.

1. **M2**: metatags (`width:`, `date:`, `type:`, `tagcount:`, `path:"…"`, `order:`…), quoted
   values, autocomplete in the search bar, tag sidebar with counts and namespace colours.
2. **M3**: perceptual hashes (dHash, pHash), BK-tree, `dupes:` metatag, review screen.
3. **M4**: `notify` file watching, reload on outside changes to `collection.json` / `tags.json`,
   Tags screen, Unknown tags screen, rebuild from sidecars, saved searches, slideshow.
4. **UI polish found in M1**: Tab to complete in the tag
   input; fullscreen viewer; a progress count during scans (now only a spinner); thumbnails
   for videos (via PlayCraft's Rust decoders, see Later).

## Known limitations

- Sidecars in `Stem` mode (`cat.xmp`): two files with the same stem in one folder (`cat.jpg`,
  `cat.png`) share one sidecar, and the last write wins.
- One-level `lr:hierarchicalSubject` entries written by other apps are kept as they are, even
  after the matching tag is removed in BooruCraft.
- Stale-lock detection only works on Linux; elsewhere the error message says which lock file to
  delete.
- Camera raw files get no width/height (their headers describe a preview).
- Animated GIF detection reads only the first 64 KiB.
- After an edit the whole search index of that collection is rebuilt (fast up to tens of
  thousands of files; incremental updates later if needed).
- A collection can't be closed while any scan is running (scan threads report back by position).
- `scripts/run.sh` runs the app in the dev container: "Open in default app" and "Show in folder"
  do nothing there. Outside the container they use the system's default apps.
- No CJK font is bundled: Japanese, Chinese and Korean text needs a system font (Noto CJK,
  Source Han, WenQuanYi, …); the app says so on start-up when none is found.
