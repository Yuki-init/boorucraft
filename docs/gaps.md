# Gaps: ranked work list

Pick from the top. Each item names its milestone.

1. **UI polish**: thumbnails for videos (via PlayCraft's Rust decoders, see Later).
2. **Later** (ROADMAP): tag aliases and implications; importing tags from boorus by MD5; a local
   auto-tagger.

## Known limitations

- Network drives often send no change events; there, changes show up after a Rescan (outside
  changes to `collection.json` are still noticed within 10 s).
- Closing the window while a "changed by another program" choice is open keeps the version on
  disk; that window's unsaved changes are dropped.
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
- While a search has an error, the grid keeps showing the last good results (the count says how
  many); the bad term is underlined.
- Clicking a sidebar tag rebuilds the query from whitespace-separated words, so runs of spaces
  inside a quoted `path:"…"` value collapse to one.
- Parent/child links, pool positions and "not duplicates" decisions live only in
  `collection.json`, not in XMP sidecars, so "Rebuild from sidecars" can't bring them back.
- A pool search holds only the files in open collections; pages in a closed collection keep
  their numbers and slot back in when it opens.
- With Stacks on, the sidebar's tag and rating counts cover the visible tiles, not the files
  hidden inside stacks.
- Perceptual hashes come from thumbnails; pictures with large flat areas (wallpapers with one
  object in the middle) can still match falsely at higher thresholds.
