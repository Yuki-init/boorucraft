# AGENTS.md: rules for every agent and contributor

BooruCraft is a desktop picture organiser and viewer that works like a booru: files are tagged
and found with booru-style searches. It is written in Rust, built to the same conventions as
PlayCraft. The design is in [docs/design.md](docs/design.md).

These rules bind every AI agent and every human contributor. `CLAUDE.md` holds day-to-day working
notes; if the two disagree, **this file wins**.

## 1. Rust crates only

- Every dependency is a Rust crate. No C or C++ is compiled into or linked by the program: no
  `cmake`, `bindgen` or `-sys` crates that build or find C libraries, and `cc` / `pkg-config`
  only as optional build steps that stay switched off. `cargo xtask rust-only` enforces this on
  Cargo.lock and on what the build actually links (only `dl` is allowed). Talking to libraries
  the operating system provides at run time (graphics drivers through wgpu, X11/Wayland, the
  desktop portal) is fine.
- Formats the app can't decode in Rust are still indexed, tagged and searched, and open in the
  system's default app (`open` crate). Never add a C decoder to show them.
- Permissive licences only (`cargo deny`): no GPL, LGPL or MPL dependencies.

## 2. Never damage the user's files

People trust an organiser with their only copy of their pictures. **This outranks feature work.**

- Never move, rename, modify or delete a picture. The only exception is the duplicates screen's
  "move to trash", after the user confirms, through the system trash.
- The only files BooruCraft writes into a collection folder are `collection.json` (with `.bak`,
  `.tmp` and `.lock`) and XMP sidecars.
- Every save is atomic (temp file, flush, rename). JSON files keep a `.bak` and are never saved
  over a version another program wrote since it was loaded.
- Sidecars: change only BooruCraft's fields and keep every other byte. A sidecar that can't be
  parsed is reported and left untouched, never overwritten.

## 3. Never crash

- No `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!` in non-test code;
  no `unsafe` (`unsafe_code = "forbid"`). Every production crate root carries
  `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]`.
- Errors are `Result<T, E>` with the crate's error type and `?`. The UI reports them and carries on.
- **Files are hostile**: pictures, sidecars and even `collection.json` (it can be edited by hand).
  Cap reads, use `get()` not `[i]`, checked arithmetic, bounded recursion, and never trust a
  stored path (`Collection::abs_path` refuses `..` and absolute paths). Third-party parsers run
  inside `catch_unwind`.
- Every crash fix lands with a regression test.

## 4. Architecture

| Layer | Crates |
|---|---|
| L0 | `core`: tag names, ratings, the query language (no IO) |
| L1 | `store`: tag list, collection.json, saving and locking, search index; `xmp`: sidecars; `dedup`: perceptual hashes, BK-tree, grouping (pure functions) |
| L2 | `index`: scanning, hashing, metadata, sidecar sync, duplicate groups across collections; `thumbs`: decoding, thumbnail cache, perceptual hashes of thumbnails |
| L3 | `ui-egui`: the window (search, grid, viewer, tag editor) |
| L4 | apps: `boorucraft-cli`, `boorucraft` desktop |

- `cargo xtask layers` enforces it: a crate depends only on lower layers, and nothing below L3
  uses egui, eframe, winit, wgpu or rfd.
- The UI is thin: it reads the store and acts through the same functions the CLI uses. egui only.
- Time goes through `chrono`.

## 5. Quality gates

Before every commit: `scripts/check.sh`, which runs in Podman: `cargo xtask ci` (fmt, clippy
`-D warnings`, tests, layers, rust-only) and `cargo deny`. Commit one landed task at a time with
its task id (`M1.2: thumbnail grid`).

## 6. Progress docs

`ROADMAP.md` (stage, progress log), `docs/gaps.md` (ranked work list: pick from the top),
`docs/design.md` (decisions and formats). When work lands, update the affected docs.
