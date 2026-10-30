# BooruCraft design

A desktop app for organising and viewing a local picture collection the way a booru site does:
every file has tags, and you find files by searching tags and other properties.

## Decisions

| # | Decision | Why |
|---|---|---|
| D1 | Desktop app with egui/eframe (wgpu), same stack as PlayCraft | One binary, fast thumbnail grid, shared know-how and UI pieces |
| D2 | Rust libraries only (see "Rust-only rule") | The user's rule; also keeps builds simple and portable |
| D3 | A collection is a folder; its data lives in `collection.json` inside that folder | Tags travel with the pictures, human-readable, no database needed |
| D4 | Never move, rename or change the user's files; the only files written to the folder are `collection.json` (with its `.bak` and lock file) and XMP sidecars | The folder stays usable without the app |
| D4a | Tags and rating are also written to an XMP sidecar next to each file (`cat.png.xmp`) | Other photo apps (digiKam, darktable, Lightroom…) can read the tags; tags survive even without `collection.json` |
| D4b | One central tag list (`~/.config/boorucraft/tags.json`) defines the valid tags and namespaces for every collection | The same tag means the same thing everywhere; no typo variants; renames apply to all collections |
| D5 | Identify a file by its content hash (SHA-256), not its path | Tags survive moves and renames; exact duplicates fall out for free |
| D6 | Store MD5 as well | Danbooru, Gelbooru and others look posts up by MD5, so tags can be imported later |
| D7 | All data is held in memory; search runs on in-memory indexes (roaring bitmaps) | Instant search; JSON is only read at open and written on save |
| D8 | Booru search syntax, parsed by our own parser | Familiar to booru users |
| D9 | Near-duplicate detection with 64-bit perceptual hashes and Hamming distance | Cheap to compute and store; finds resizes and re-encodes |
| D10 | Formats the app can't decode itself are still indexed, tagged and searched, but open in the system's default app | Videos, AVIF, HEIC etc. stay part of the collection without C decoders |
| D11 | Build and test only in Podman | Same habits as PlayCraft |

## Rust-only rule (D2)

- Every dependency is a Rust crate. No C or C++ is compiled into the binary: no `-sys` crates that
  build bundled C code, no `cc`/`cmake` build steps.
- Allowed: talking to libraries the operating system already provides, the way every desktop app
  does (graphics drivers through wgpu, X11/Wayland, the desktop portal). Nothing from these is
  bundled; they are opened at run time (`dlopen`).
- Crates are configured to stay pure: `image` with only its Rust decoders, `rfd` with the
  `xdg-portal` backend (not GTK), `eframe` with wgpu and without its bundled fonts.
- `cargo xtask rust-only` checks two things:
  1. Cargo.lock has no crate whose only job is building or finding C code (`cmake`, `bindgen`,
     `vcpkg`, `system-deps`, `cxx`, …). `cc` and `pkg-config` are allowed in the lock file
     because the windowing crates list them as optional build steps that are never switched on
     here.
  2. It builds the workspace and reads what every build script told the compiler to link. The
     only native library allowed is `dl` (x11-dl uses it to open X11 at run time); anything
     else, or any static library, fails the check.
- This is why the hash is SHA-256 (`sha2`): the `blake3` crate always compiles C with `cc`.
- Licences: permissive only (checked by `cargo deny`). This is why config and cache folders are
  found by our own code instead of the `dirs` crate, which pulls in an MPL-2.0 dependency.

Crates: `serde` + `serde_json`, `sha2`, `md-5`, `quick-xml`, `roaring`, `walkdir`, `rayon`,
`imagesize`, `kamadak-exif`, `chrono`, `thiserror` (since M0); `eframe`/`egui` (wgpu), `image`
(JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI), `jxl-oxide`, `open`, `rfd` (since M1); `trash` (since M3; on Linux it is plain Rust over the
freedesktop.org trash folders); planned: `notify`.

## Workspace layout

```
crates/
  core/      data types, query parser and AST (no IO, heavily unit-tested)
  store/     central tag list, collection.json format, load/save, locking, in-memory indexes, search
  xmp/       reading and updating XMP sidecars without losing other apps' data
  index/     folder scanning, hashing, metadata, sidecar sync
  thumbs/    decoding pictures (Rust decoders only), thumbnail cache
  dedup/     perceptual hashes, BK-tree, grouping (pure functions, no IO)
  ui-egui/   search bar, grid, viewer, tag editor, duplicates view
apps/
  boorucraft/      the desktop app
  boorucraft-cli/  headless scan / search / tag / dupes, for tests and scripting
```

Dependencies only point downwards: `core` <- `store`, `xmp`, `dedup` <- `index`, `thumbs` <- `ui-egui` <- apps.

## Central tag list (D4b)

`~/.config/boorucraft/tags.json` is the one list of valid tags, shared by every collection. Its
location can be changed in settings, e.g. to a synced folder so several computers share it.

```json
{
  "format": "boorucraft-tags",
  "version": 1,
  "revision": 42,
  "namespaces": {
    "artist":    { "color": "#d04040" },
    "character": { "color": "#40a040" },
    "series":    { "color": "#a040c0" },
    "meta":      { "color": "#e09030" }
  },
  "tags": {
    "artist:foo": { "description": "" },
    "cat":        { "description": "Domestic cats, any breed" },
    "sky":        {}
  },
  "history": [
    { "revision": 41, "op": "rename", "from": "kitty", "to": "cat" },
    { "revision": 42, "op": "merge", "from": ["feline"], "into": "cat" }
  ]
}
```

- **Only listed tags can be applied.** Typing a tag that isn't in the list offers the closest
  existing tags first, then "Create tag `xyz`", which adds it to the central list.
- **Collections store tag names, not IDs**, so `collection.json` and the sidecars stay readable and
  the folder stays meaningful on its own.
- **Rename, merge and delete** go through the central list and are recorded in `history` with a
  new `revision`. Open collections are updated straight away (JSON and sidecars). Each
  `collection.json` records the `tags_revision` it was last synced to, so a collection that wasn't
  open at the time (e.g. on an unplugged drive) catches up from `history` the next time it opens.
- **Unknown tags**: tags in a collection that aren't in the central list, e.g. from a folder
  copied from another computer or tags added by another app in a sidecar. They are listed on an
  "Unknown tags" screen with three choices for each: add to the central list, map to an existing
  tag, or remove. Until then they are kept and searchable, but shown in grey.
- Saved with the same safety rules as `collection.json`: debounced, atomic, `.bak`, lock.
- Later: aliases (`kitty` → `cat`) and implications (`cat` → `animal`) live here too.

Counts per tag are not stored; they are computed in memory for the open collections.

## Collections and `collection.json` (D3, D4)

A collection is any folder you add, e.g. `~/Pictures/Anime`. The app's own settings
(`~/.config/boorucraft/config.json`) only list which collections are open. Searches can cover
one collection or all open ones.

```json
{
  "format": "boorucraft-collection",
  "version": 1,
  "files": {
    "9f2c…b71a": {
      "md5": "5d41…2a7c",
      "paths": ["2024/cat.png", "favourites/cat copy.png"],
      "kind": "image",
      "format": "png",
      "size": 1834211,
      "width": 2560,
      "height": 1440,
      "animated": false,
      "taken": "2024-03-02T14:11:09Z",
      "imported": "2026-10-29T21:30:00Z",
      "rating": "s",
      "source": "https://example.org/post/123",
      "tags": ["artist:foo", "cat", "sky"],
      "phash": "c3a1f00e9b7d2244",
      "dhash": "0f0f3c3c7e7e1818"
    }
  },
  "not_duplicates": [["9f2c…b71a", "41be…07d3"]],
  "tags_revision": 42
}
```

- Entries are keyed by SHA-256 hash. Paths are relative to the collection folder, use `/`, and a
  file can have several (exact copies).
- Saved pretty-printed, with keys, tags and paths sorted, so diffs are small and the file works
  well in git or backups.
- `phash`/`dhash` are stored here because recomputing them means decoding every picture again.
- `version` allows migrations; an older app refuses a newer file instead of damaging it.
- A subfolder that has its own `collection.json` is a separate collection and is skipped by the
  parent's scan.
- Missing files (no path found on disk) keep their entry, marked missing in the UI, so tags come
  back if the file reappears (matched by hash). The user can purge them.

### Saving safely

- **Debounced**: saves about 2 seconds after the last change, and on close.
- **Atomic**: write `collection.json.tmp`, flush it to disk, then rename it over `collection.json`.
  The previous version is kept as `collection.json.bak`.
- **Lock**: `collection.json.lock` (holding the process ID and host name) is created when a
  collection is opened. A second app instance opens it read-only; a stale lock from a crashed
  process on the same machine is detected and replaced (Linux; elsewhere the message says which
  file to delete).
- **Changed by someone else**: if `collection.json` changed on disk since the last load or save
  (another tool, a sync program), the app reloads it and asks before saving over it.
- **Damaged file**: if it doesn't parse, the app offers to load `.bak` and never overwrites
  the damaged file.

Expected size: about 300–400 bytes per file, so 100k files ≈ 35 MB of JSON, loaded and indexed
in well under a second. If saving ever becomes slow at very large sizes, the fallback is an
append-only change log (`collection.log`, JSON lines) compacted into `collection.json` now and
then. This isn't needed for v1.

## XMP sidecars (D4a)

Every tagged file gets a sidecar named `<file name>.xmp` next to it (`2024/cat.png` →
`2024/cat.png.xmp`), the convention digiKam and darktable use. A setting switches to the
Lightroom style (`cat.xmp`) per collection. A file with several paths gets a sidecar at each path.

What BooruCraft writes:

| XMP field | Content |
|---|---|
| `dc:subject` | all tags as plain keywords: `cat`, `sky`, `artist:foo` |
| `lr:hierarchicalSubject` | namespaced tags as hierarchies: `artist|foo`, so Lightroom and digiKam show them as a tree |
| `boorucraft:Rating` | `s`, `q` or `e` (own namespace; the star rating `xmp:Rating` is left alone) |
| `boorucraft:Source` | source URL |
| `boorucraft:Hash` | SHA-256, to link the sidecar back to its file |

**Other apps' data is kept.** darktable stores its edit history in the same file, and other apps
add their own fields. The keyword fields are shared, so BooruCraft also keeps other apps'
keywords in them: darktable's own tree (`darktable|format|jpg`) and the parent levels of keyword
hierarchies (`Places` and `Europe` for `Places|Europe|France`) stay in the file but are not read as
tags. BooruCraft reads an existing sidecar with `quick-xml`, replaces only the
fields above, and writes everything else back untouched. Sidecars are written atomically
(temp file + rename) by a background writer, debounced like `collection.json`.

**Source of truth**: `collection.json`. Sidecars are a mirror, with two exceptions:

- **Edited by another app**: during a scan, a sidecar whose modification time differs from the
  one recorded after BooruCraft's last write or read is read back. If BooruCraft wrote it before
  (it has `boorucraft:Hash`), its tags replace the file's tags; if not (a sidecar from another
  app), its tags are added and nothing is removed. Tags not in the central list go to the
  "Unknown tags" screen. A setting turns this off.
- **Rebuild**: if `collection.json` is lost, "Rebuild from sidecars" recreates tags, ratings and
  sources from the XMP files.

Sidecars are never counted as collection files, and a sidecar without a matching file is left
alone. A setting turns sidecars off for a collection, and "Remove sidecars" deletes only the
sidecars BooruCraft created that hold no data from other apps.

### Cache (outside the collection)

`~/.cache/boorucraft/thumbs/ab/cdef….jpg` (PNG when the picture has transparency), keyed by
SHA-256 and shared by all collections. Safe to delete; thumbnails are regenerated on demand.

## In-memory index (D7)

On load, each file gets a dense number (0..n). The store builds:

- a roaring bitmap per tag (which files have it), plus bitmaps per rating, kind and format;
- columns for size, width, height, dates and tag count, for range filters and sorting;
- a sorted tag list with counts, for autocomplete and the sidebar;
- a path → file map, used by the scanner.

Edits update the indexes and mark the collection and the affected sidecars dirty for the next save.

## Indexing

1. The scanner walks the collection folder (`walkdir`), skipping hidden folders, symlinks and
   nested collections, and hashes new or changed files in parallel (`rayon`).
2. A path whose size and mtime haven't changed since the last scan is skipped. Size and mtime are
   kept in memory and in a small scan cache in `~/.cache`. They aren't stored in `collection.json`,
   since copying a folder changes mtimes.
3. Otherwise the file is hashed (SHA-256 + MD5). A known hash just gets the new path attached,
   so moved files keep their tags. A picture edited in place (same path, new content) passes
   its tags, rating and source on to the new content.
4. Metadata: dimensions and format from `imagesize` (headers only, so this works for AVIF, HEIC
   etc. too; not for camera raw files, whose header describes a small preview), EXIF date from
   `kamadak-exif`, otherwise mtime; animation from the GIF/PNG/WebP headers.
5. For decodable images: thumbnail and perceptual hashes on a worker pool. The grid shows
   placeholders until each is ready.
6. Sidecars: a newer-than-expected `.xmp` is read back (see XMP sidecars); a tagged file without
   one gets it written.
7. While the app runs, `notify` watches the folder and queues changed paths for steps 2–6.

Which files are included: known picture and video extensions by default (jpg, png, gif, webp,
bmp, tiff, jxl, avif, heic, mp4, webm, mkv, mov…). The list is configurable per collection.
`.xmp` files and BooruCraft's own files are always excluded.

## Formats (D10)

| | Decoded by the app | Opened externally |
|---|---|---|
| Formats | JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI, JPEG XL | AVIF, HEIC/HEIF, videos (MP4, WebM, MKV, MOV…), RAW photos, anything else |
| Thumbnail | yes | format icon with a label (e.g. "MP4", "HEIC") |
| In-app viewer | yes (zoom, pan, animation) | the viewer shows the format icon and an "Open in default app" button; Enter / double-click opens it directly |
| Tags, rating, search | yes | yes |
| Dimensions | yes | from headers where `imagesize` supports it (AVIF, HEIC); otherwise unknown |
| Exact duplicates | yes | yes (hash) |
| Near duplicates | yes | no (needs decoded pixels) |

Opening externally uses the `open` crate, which hands the file to the desktop's default app
(`xdg-open` on Linux). Every file, decodable or not, also has "Open in default app" and
"Show in file manager" actions.

Later option: video thumbnails and dimensions via PlayCraft's pure-Rust H.264 decoder and
container crates. Playback would still go to the system player.

## Search syntax (D8)

| Query | Meaning |
|---|---|
| `cat sky` | has both tags |
| `-dog` | doesn't have the tag |
| `~cat ~dog` | has at least one of them (works with metatags too: `~kind:video ~animated:yes`) |
| `cat*` | wildcard on the tag name |
| `artist:foo` | namespaced tag |
| `rating:s` / `rating:q,e` | rating (also `-rating:e`; unrated files: `-rating:s,q,e`) |
| `width:>1920` `height:<=1080` `width:1000..2000` `width:..500` | size in pixels |
| `ratio:16:9` `ratio:>1` `mpixels:>2` | aspect ratio (a plain value matches ±0.01), megapixels (±5%) |
| `filesize:>2MB` `filesize:500kb` | size in bytes; units k/m/g are powers of 1024; a plain value matches ±5% |
| `type:png,gif` `kind:video` `animated:yes` | format (`jpg` = `jpeg`, `tif` = `tiff`), kind (image, video, other) |
| `date:2024` `date:2024-01..2024-06` `date:>2023-05-01` | taken date (EXIF, else mtime, else import date) |
| `imported:<7d` `imported:>1y` `date:30d..90d` | ages (`h`, `d`, `w`, `mo`, `y`) compare by age: `<7d` is "less than 7 days old" |
| `tagcount:0` `tagcount:>10` | `tagcount:0` finds untagged files |
| `path:"holiday 2023"` `path:pets/*` | substring of any path, or a wildcard over the whole path (case-insensitive) |
| `source:none` `source:*pixiv*` | no source / source matches |
| `md5:…` `hash:3fa9` | exact MD5 (32 hex digits) / SHA-256 prefix (4+ digits) |
| `missing:yes` | the file wasn't found in the last scan |
| `dupes:yes` `dupes:no` | has a near-duplicate in the open collections |
| `order:date` `order:-date` `order:filesize` `order:name` `order:random` | sort order, one per search |

Values with spaces go in double quotes. Calendar dates are in UTC, the same way EXIF times are
stored. Files without a value (videos have no width) never match a numeric filter and sort last.

`order:` keys: `imported` (the default), `date`, `filesize`, `width`, `height`, `mpixels`,
`ratio`, `tagcount`, `name`, `path`, `random`. Without a sign they sort largest or newest first,
except `name` and `path`, which sort A→Z; `-` reverses (`_asc` / `_desc` also work). Results from
several collections are sorted together. `order:random` keeps its shuffle for the session.

The parser lives in `core` and returns an AST with error positions; the search bar underlines
the bad term and says what's wrong, with an example. The store evaluates it with bitmap
operations: AND for plain tags, OR for `~` groups, subtracting for `-`, and wildcards as the union
of matching tag bitmaps. Metatags scan per-file columns kept in the index. The result is then
sorted.

Autocomplete suggests tags by prefix (also of the name after a namespace), ordered by count, in
their namespace colour; then metatag names with an example, and values after `rating:`,
`kind:`, `order:` and the like. Arrow keys choose, Tab or Enter accepts, Escape hides it.

## Duplicate detection (D9)

- **Exact**: same SHA-256, including the same picture in two open collections. One entry with
  several paths inside a collection.
- **Near**: dHash (9×8 grid) and pHash (low 8×8 DCT frequencies of a 32×32 grid), 64 bits each,
  computed from the thumbnail (so a cached thumbnail needs no decoding; transparent parts count
  as mid-grey) and stored in `collection.json`. Two pictures are joined if both distances are at
  most a threshold: default 6, adjustable. Tried on a copy of the sample photos: resized and
  re-encoded copies differ by 0–3 bits, while two unrelated wallpapers with flat backgrounds
  and one object in the middle differed by 6 and 8, so 8 was too loose. Candidates come from a
  BK-tree over the dHashes; groups are joined transitively (union-find) and list the best copy
  first (most pixels, then biggest file, then imported first).
- **When**: after scans finish, a background job hashes every decodable picture that has no
  hashes yet (status bar: "Looking for duplicates: n / total"). A picture that fails isn't
  retried until the next start. `boorucraft-cli dupes DIR…` does the same and prints the groups.
- **Review screen** ("Duplicates" in the top bar, or `--duplicates`): each group side by side
  with resolution, file size, format, paths, tag count, rating and source; the best resolution
  and size are bold. The best copy is chosen to keep; a click chooses another. Actions:
  - **Keep this one, trash the others…**: a dialog lists what goes and, by default, first copies
    their tags, rating and source to the kept file. Every copy of each file goes to the system
    trash (`trash` crate) with its sidecar (unless another file shares it, as `cat.xmp` can in
    Lightroom naming), and its entry leaves `collection.json`.
  - **Copy tags to this one**: tags of the others are added; rating and source only if it has
    none (the strictest rating wins).
  - **Not duplicates**: every pair is saved in `not_duplicates` of each collection involved.
  - **Show in grid**: searches `~hash:… ~hash:…` for the group.
  - A slider sets the threshold (0–12 bits, saved in `config.json`).
  At most 200 groups are drawn at once.

## UI

- **Top**: search bar with autocomplete and error underline; result count; collection picker.
- **Left**: tag sidebar for the current results (tags with counts, grouped by namespace; click to
  add, ctrl-click to exclude).
- **Centre**: virtualised thumbnail grid, adjustable size; multi-select with shift and ctrl.
- **Viewer**: fit, 1:1, zoom and pan, next/previous within the results, GIF/WebP animation,
  slideshow, fullscreen. Unsupported formats open externally (D10).
- **Tag editor**: on the viewer and on a multi-selection (bulk add/remove), with
  autocomplete from the central tag list; type `artist:` to set a namespace. New tags need an
  explicit "Create tag".
- **Tags** screen: the central list with counts in the open collections; create, describe,
  rename, merge, delete, namespace colours; the "Unknown tags" review.
- **Duplicates** tab and **Collections** settings (add/remove folders, rescan, included file
  types, sidecar on/off and naming, thumbnail cache size, tag list location, rebuild from
  sidecars).

Keyboard first: `/` focuses search, arrows move through the grid, `Enter` opens, `t` edits tags,
`1`/`2`/`3` set the rating, `o` opens in the default app.

## Open questions

None at the moment.
