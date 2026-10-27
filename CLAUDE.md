# Oxidex

A fast local file search engine written in Rust. You point it at a folder or drive, and it builds an index you can search instantly. It runs in the background and shows a popup search bar on a global hotkey.

The current focus is **filename search only**. Content search (an inverted index plus text extractors) comes later. Don't add content-search code until asked.

## Layout

```
oxidex/
├── oxidex-core/   # library: all logic, no UI
│   └── src/
│       ├── walk.rs      # folder crawling
│       ├── index.rs     # Index, Entry, packed name buffer
│       ├── search.rs    # tiered matching and scoring
│       ├── extract/     # Extractor trait + implementations
│       └── store.rs     # save/load index to disk
└── oxidex-app/    # binary: egui popup, hotkey, tray
```

- `oxidex-core` never depends on UI crates.
- The indexer owns and mutates the `Index`. Search only takes `&Index`.

## Key design decisions

Don't change these without asking first.

- **Packed names.** All names live in one lowercased `String` buffer. Each `Entry` stores `name_start` and `name_len`. Never store a `String` per file.
- **Parent IDs, not full paths.** Each entry stores its parent folder's ID. Folders are entries too (`is_dir: true`). Rebuild full paths only for results that are actually shown.
- **Display names** are in a separate packed buffer with their own offsets, because lowercasing can change byte length.
- **Tiered matching.** The order is exact match, then prefix, then subsequence (fzf-style), then Levenshtein. The Levenshtein pass only runs when earlier tiers return few results. It's capped at 2 edits and skips names whose length difference rules them out.
- **Path mode.** A query containing `/` or `\` matches the last part against file names and the folder parts against folder names, by walking parent IDs.
- **Extractors** implement the `Extractor` trait (`name`, `extensions`, `sniff`, `extract`) and are `Send + Sync`. Detect file types by magic bytes first, then by extension.

## Dependencies

Keep dependencies minimal. Prefer the standard library. Well-known, focused crates (`serde`, `eframe`/`egui`) are fine. **Ask before adding any new dependency** and say why the standard library isn't enough.

## Code style

- Write simple, direct code. If something can be done in 50 lines instead of 200, do it in 50.
- Comment complex code.
- No abstractions, traits, or generics for things that only have one implementation. The `Extractor` trait and the scoring trait (one implementation per ranking signal, see `RANKING.md`) are the deliberate exceptions.
- Follow standard Rust naming: `snake_case` for functions and fields, `CamelCase` for types.
- Keep functions short and focused. Prefer plain loops and early returns over clever iterator chains when they're clearer.
- Library code returns `Result` and never panics on bad input or I/O errors. No `unwrap()` or `expect()` in `oxidex-core` outside tests.
- Comments explain *why*, not *what*. Don't comment obvious code.
- Code must pass `cargo fmt` and `cargo clippy -- -D warnings`.

## Making changes

- **Stay in scope.** Do exactly what was asked, nothing more. No bonus features, and no "while I was here" fixes. If you notice something else worth changing, mention it instead of doing it.
- **Surgical changes.** Touch only the lines the task needs. Don't reformat, rename, reorder, or refactor existing code that isn't part of the request.
- **Don't rewrite working code** to suit your own style. Match the conventions already in the file.
- If a request is unclear or would require changing a design decision above, ask before writing code.

## Commits

- Keep each commit to about 200 changed lines or fewer. Split bigger work into a sequence of small commits that each build and pass tests.
- One logical change per commit, with a short imperative message (e.g. `Add prefix tier to name matching`).

## Performance

Speed is the main goal of the project.

- Keep the search path allocation-free where possible. Return slices into the buffers rather than new `String`s.
- Don't guess about performance. Back claims with a benchmark or timing on a real directory.
- Indexing can be parallel and slower. Searching must stay instant.

## Commands

```sh
cargo build
cargo test
cargo fmt
cargo clippy -- -D warnings
cargo run -p oxidex-app
``` 