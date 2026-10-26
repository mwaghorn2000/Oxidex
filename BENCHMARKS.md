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
