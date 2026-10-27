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

### Typo pass

Step 6 adds the Levenshtein tier. It's a second full scan that only runs when
the first pass finds fewer than 50 hits.

| Query | Typo pass runs? | Step 5 | Step 6 |
|---|---|---|---|
| `a` | no | 13.1 ms | 14.1 ms |
| `readme` | no | 17.7 ms | 19.2 ms |
| `rdm` | no | 17.9 ms | 19.4 ms |
| `zzqxj` | yes, 1 edit | 14.8 ms | 29.2 ms |
| `café` | yes, 1 edit, non-ASCII | 27.0 ms | 50.0 ms |
| `docuemntation` | yes, 2 edits | — | 48.5 ms |

Queries that skip the pass are within noise of step 5. When the pass runs it
costs 14–35 ms on top of the first pass. Most of that is the second visit to
all 545k names plus an edit-distance table for every name within `max`
characters of the query's length. Long queries with 2 edits and non-ASCII
queries (decoding every name) are the worst cases.

### Typos in the first pass

Typos now rank above subsequence matches, and are checked in the same pass
for queries of 4+ characters. The second pass is gone.

| Query | Typo check? | Step 6 | Now |
|---|---|---|---|
| `a` | no (too short) | 14.1 ms | 13.9 ms |
| `rdm` | no (too short) | 19.4 ms | 18.7 ms |
| `readme` | yes | 19.2 ms | 28.0 ms |
| `main.rs` | yes | 17.1 ms | 27.4 ms |
| `zzqxj` | yes | 29.2 ms | 23.3 ms |
| `café` | yes | 50.0 ms | 42.3 ms |
| `docuemntation` | yes | 48.5 ms | 41.1 ms |

Worst cases got faster, because there's no second scan. Typical queries of
4+ characters got about 9 ms slower, because every name that isn't an exact
or prefix match now gets a length check and, if its length is close to the
query's, an edit-distance table. This is the biggest single cost to look at
when optimising.

## Optimisation

### Missing-character filter

Each entry stores a 64-bit mask of the characters in its name (`char_mask`).
A name missing more distinct query characters than `max_edits` can't match
in any tier, so search skips it without reading the name.

| Query | Before | After |
|---|---|---|
| `a` | 13.9 ms | 11.4 ms |
| `e` | — | 12.5 ms |
| `rdm` | 18.7 ms | 5.6 ms |
| `readme` | 28.0 ms | 10.4 ms |
| `main.rs` | 27.4 ms | 8.1 ms |
| `cargo` | 27.1 ms | 7.0 ms |
| `zzqxj` | 23.3 ms | 1.0 ms |
| `raedme` | 27.4 ms | 10.9 ms |
| `café` | 42.3 ms | 29.4 ms |
| `docuemntation` | 41.1 ms | 6.4 ms |

Several queries are now faster than the naive scan, because most names
are never read. Building the masks didn't change walk time measurably
(median 320 ms with, 313 ms without; warm walks vary from 0.22 to 0.46 s).

Checked against the unfiltered search on `/usr` with no result limit: all
25,103 results for 10 queries had the same paths, tiers and scores.

What's left:

- **`a` and `e`** match most names, so the filter can't skip them. Their
  time goes on scoring and on keeping the top 50.
- **Non-ASCII queries** (`café`): the mask doesn't track `é`, so more names
  pass, and each of those is decoded into `char`s.

### Prefix only for 1–2 character queries

Queries of 1–2 characters now only match exactly or as a prefix.

| Query | Before | After |
|---|---|---|
| `a` | 11.4 ms | 6.1 ms |
| `e` | 12.5 ms | 6.4 ms |
| `re` | — | 4.5 ms |
| `rdm` (3 characters, unchanged) | 5.6 ms | 5.4 ms |
| `readme` (unchanged) | 10.4 ms | 9.9 ms |

Every query measured so far is now at or near the 10 ms target (`readme`
9.9 ms, `raedme` 10.9 ms at the last measurement), except non-ASCII ones
(`café`, 29 ms).
