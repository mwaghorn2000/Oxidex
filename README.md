# Oxidex

A fast local file search, written in Rust. Point it at some folders, and it
builds an index of every file and folder name in them. Then you type, and
the best matches appear as you go, in a few milliseconds.

The end goal is a background app with a popup search bar on a global
hotkey. For now there's a command-line version for trying it out.

It only searches file and folder **names** for now. Searching inside files
comes later.

## Try it

You'll need Rust installed (from [rustup.rs](https://rustup.rs)). Then, from
the repo:

```sh
cargo run --release -- ~/projects ~/Documents
```

Give it one or more folders. It walks each one once at startup, then shows
a prompt. Start typing and the results update under the query:

```
> oxdx/src/srch
  /home/you/projects/oxidex/src/search.rs
  ...
```

- **Backspace** deletes, and **Esc** or **Ctrl-C** quits.
- If you pipe input in instead of typing, it reads one query per line and
  prints the results. That's handy for scripts.
- Use `--release`. A debug build works, but it's much slower.

### Things to try

| Query | What it shows |
|---|---|
| `readme` | Exact matches, with or without the extension |
| `read` | Names that start with the query |
| `rdm` | Letters in order with gaps, like fzf |
| `raedme` | Typos: a swapped or wrong letter still matches |
| `docs/notes` | Path mode: folders above the file must match too |
| `oxdx/src/srch` | Path mode with every part abbreviated |

### On WSL

Your Windows drive is at `/mnt/c`, so this works:

```sh
cargo run --release -- /mnt/c/Users/<you>/Documents
```

Reading Windows folders from WSL is slow (about 0.3 ms per file), so stick
to a few folders rather than a whole drive. Searching is just as fast once
the walk is done. A native Windows build doesn't have this problem.

## How search ranks results

Every name gets sorted into a tier. A better tier always comes first:

1. **Exact:** the whole name, or the name without its extension (`readme`
   finds `README.md`).
2. **Prefix:** the name starts with the query.
3. **Typo:** within 1 or 2 edits of the name. Swapping two letters counts
   as one edit.
4. **Subsequence:** the query's letters appear in the name in order, with
   gaps allowed.

Within a tier, results are ranked by how good the match is, plus a small
boost for recently modified files. Short queries are kept simple:
1–2 letters only match exactly or as a prefix, subsequence matching starts
at 3, and typos at 4.

The full plan, including what's decided and what's still open, is in
[RANKING.md](RANKING.md).

## How it's put together

```
src/
├── main.rs     the command-line app
├── lib.rs      the library (uses only the standard library)
├── walk.rs     crawls a folder on several threads and builds an Index
├── index.rs    the Index: every name, its parent folder, size and time
├── search.rs   scanning the indexes and keeping the best results
├── matching.rs tiers and match quality for one name, and path mode
└── score.rs    ranking signals (match quality, modified time)
examples/
├── walk_time.rs     times a walk
└── search_time.rs   times searches against a naive baseline
```

A few ideas hold most of it together:

- **Names are packed.** All names live in one big string, and each entry
  just stores where its name starts and how long it is. There's no
  `String` per file.
- **Entries store their parent, not their path.** Full paths are only
  built for the results you actually see.
- **Each name has a character mask:** 64 bits saying which characters it
  contains. Most names can be ruled out with one bitwise check, without
  reading them. This filter is what makes search fast.
- **Ranking signals are pluggable.** Each one is a `Scorer` in `score.rs`,
  and they're all listed in `default_scorers()`. Adding one doesn't touch
  the search code.

If you're reading the code, start at `search()` in `src/search.rs`. The
comment at the top of that file explains where everything plugs in.

## Working on it

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo run --release --example search_time -- ~/projects readme rdm
```

Speed is the main goal, so changes that affect it should come with
numbers. [BENCHMARKS.md](BENCHMARKS.md) has the measurements so far.
[CLAUDE.md](CLAUDE.md) has the code style and the design decisions not to
change without discussion.

## What's next

- Skip folders like `node_modules`, `target` and `.git`, which currently
  flood the results.
- Find typos in partly typed names (`the binding of iscac`).
- Remember which files you open, and rank them higher.
- Save the index to disk, so it doesn't re-walk on every start.
- The real app: popup, global hotkey, tray.
