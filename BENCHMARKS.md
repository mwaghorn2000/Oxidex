# Benchmarks

Measured with:

```sh
cargo run --release --example walk_time -- <path>
```

Machine: AMD Ryzen 5 7500X3D (6 cores, 12 threads), WSL2 (kernel 6.18.33.2),
ext4, rustc 1.99.1.

"Warm" is the median of three runs after a first run filled the OS cache.
"Cold" is that first run.

## walk

### Single-threaded (`fa809fb`)

| Folder | Entries | Cold | Warm |
|---|---|---|---|
| `/home/mitch` (ext4) | 545,154 | 12.66 s | 1.23 s |
| `/usr/lib/wsl` (Windows drive through WSL) | 3,272 | — | 3.08 s |

Building the index is a small part of this: a crawl that only reads folders
and file details, with no `push`, took 1.17 s warm on `/home/mitch`.
Folders on a Windows drive cost about 1 ms per entry through WSL, against
about 2 µs on ext4.

### Parallel, 12 workers

Workers read folders; one thread pushes into the index.

| Folder | Entries | Warm | vs single-threaded |
|---|---|---|---|
| `/home/mitch` (ext4) | 545,305 | 0.28 s | 4.4× faster |
| `/usr/lib/wsl` (Windows drive through WSL) | 3,272 | 1.13 s | 2.7× faster |

Warm runs on `/home/mitch` ranged from 0.22 s to 0.40 s. Waking all
workers for every queued folder, instead of one per folder, gave a 0.39 s
median. A crawl-only prototype with no index reached 0.17 s, so the single
pushing thread is likely the next limit.

### Worker count

`walk_time <path> <workers>`, warm, measured with `/usr/bin/time`. CPU is
user + sys across all threads.

| Workers | `/home/mitch` wall | `/home/mitch` CPU | `/usr/lib/wsl` wall | `/usr/lib/wsl` CPU |
|---|---|---|---|---|
| 1 (direct, no threads) | 1.23 s | 1.22 s | 2.96 s | 0.25 s |
| 1 (one worker thread) | 2.35 s | 2.70 s | — | — |
| 2 | 1.74 s | 3.0 s | — | — |
| 4 | 1.40 s | 3.1 s | — | — |
| 6 | 1.26 s | 3.0 s | — | — |
| 12 | 0.18–0.33 s | 1.6–1.9 s | 1.05 s | 0.42 s |

A single worker thread made about 53,000 context switches, roughly one per
folder (55,468), as the main thread and the worker took turns sleeping and
waking. With 12 workers the queue rarely empties and there were about 6,800.
So one worker now crawls on the calling thread with no queue or channel.
Between 2 and 6 workers, the walk is slower than 12 and uses more CPU than 1.

## search

```sh
cargo run --release --example search_time -- <path> <query>...
```

Each query is the median of 9 runs on an index that's already in memory.
The figures below are the median of three such runs.

### Naive baseline

A plain `contains` check on every lowercased name, with no tiers, scoring or
top-`limit` selection. This is the reference the real search is measured
against.

| Query | Matches | Time |
|---|---|---|
| `a` | 343,726 | 8.2 ms |
| `e` | 413,567 | 7.7 ms |
| `readme` | 4,959 | 9.3 ms |
| `main.rs` | 40 | 8.4 ms |
| `cargo` | 374 | 8.9 ms |
| `zzqxj` | 0 | 8.2 ms |

`/home/mitch` (ext4), 545,552 entries. The time barely depends on the query
or the number of matches. It's the cost of visiting all 545k names, about
15 ns each, so that visit is the floor for a full scan. That's already close
to the 10 ms target, so the real search can't add much per name. The
previous-query cache matters for this reason: while typing forward, it skips
the full scan after the first character.

### Tiers, one index

`search` with exact, prefix and subsequence tiers, keeping the top 50.
There's no quality scoring yet.

| Query | Matches | Naive | Search |
|---|---|---|---|
| `a` | 343,805 | 8.0 ms | 13.4 ms |
| `e` | 413,641 | 7.5 ms | 13.5 ms |
| `readme` | 4,959 | 8.7 ms | 0.15 ms |
| `main.rs` | 40 | 8.3 ms | 13.7 ms |
| `cargo` | 374 | 8.6 ms | 0.6–11 ms |
| `zzqxj` | 0 | 8.3 ms | 12.0 ms |
| `rdm` | 202 | 8.4 ms | 14.2 ms |

Most queries take 12–14 ms, about 22–26 ns per name, which is over the
10 ms target. `readme` stops early, because 50 exact matches fill the
results and nothing can beat them. `cargo` sometimes stops early too,
depending on where the walk put the exact matches. That early stop goes
away once step 3 ranks matches within a tier.

### Match quality, one index

Step 3 adds per-tier quality scoring, fzf-style subsequence scoring and the
full tiebreak order. It also removes the early stop.

| Query | Matches | Naive | Search |
|---|---|---|---|
| `a` | 343,867 | 7.9 ms | 12.7 ms |
| `e` | 413,694 | 7.5 ms | 12.6 ms |
| `readme` | 4,959 | 8.5 ms | 17.5 ms |
| `main.rs` | 40 | 8.2 ms | 16.3 ms |
| `cargo` | 374 | 8.4 ms | 17.7 ms |
| `zzqxj` | 0 | 8.1 ms | 14.7 ms |
| `rdm` | 202 | 8.2 ms | 17.6 ms |
| `café` | 0 | 8.0 ms | 27.0 ms |

Queries with no early stop are 1–3 ms slower than step 2, so 15–18 ms. The
`readme` and `cargo` speedups are gone, as expected now that a full set of
exact matches no longer stops the scan. Non-ASCII queries are the slowest,
because every name is decoded into `char`s before the subsequence check.
