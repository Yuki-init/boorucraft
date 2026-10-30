# BooruCraft: working notes for Claude

Read [AGENTS.md](AGENTS.md) first: its rules (Rust crates only, never damage the user's files,
never crash) bind you and win over anything here.

@AGENTS.md

## Start every session

1. Read `ROADMAP.md` and the top of `docs/gaps.md`.
2. Run `scripts/check.sh` before you change anything, so you know the tree is green.

## Habits

- **Build and test only through Podman**: `scripts/dev.sh <cmd>` (offline), `scripts/fetch.sh`
  (network, after changing dependencies), `scripts/check.sh` (the full gate). Nothing is installed
  on the host. Rebuild the image with `scripts/build-image.sh` after changing
  `containers/dev.Containerfile` or `rust-toolchain.toml`.
- **Never point BooruCraft at the user's real picture folders while developing**: it writes
  `collection.json` and sidecars there. Copy a sample into `.cache/` (gitignored) and run against
  that, with `BOORUCRAFT_CONFIG_DIR` and `BOORUCRAFT_CACHE_DIR` also under `.cache/`:
  `scripts/dev.sh env BOORUCRAFT_CONFIG_DIR=/work/.cache/cfg BOORUCRAFT_CACHE_DIR=/work/.cache/cache cargo run -p boorucraft-cli -- scan .cache/sample`.
  A real folder can be mounted read-only at `/pictures` via `scripts/local.env`
  (`BOORUCRAFT_PICTURES_HOST_DIR=…`) for reading only.
- Video thumbnails use FilmCraft's decoder crates and craft-media as git dependencies pinned by
  commit; update them by bumping the revisions in the root `Cargo.toml` (keep craft-media at the
  revision FilmCraft uses).
- Test pictures are generated in code (see `png()` in the index and CLI tests); never commit
  real pictures.
- Before adding a dependency, check `cargo xtask rust-only` and `cargo deny` still pass.
- Keep `ROADMAP.md` and `docs/gaps.md` current.

## Memory

Memories for this project live in `.claude/memory/` (gitignored), one fact per file with an index
in `.claude/memory/MEMORY.md`.
