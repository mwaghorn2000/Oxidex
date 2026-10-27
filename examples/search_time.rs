// Times name search on a walked folder:
// `cargo run --release --example search_time -- <path> <query>...`
// Each query runs several times and the median is reported, so one slow run
// (a page fault, a context switch) doesn't skew the result.
use std::num::NonZeroUsize;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use oxidex::index::Index;

const RUNS: usize = 9;
// Results a popup would show.
const LIMIT: usize = 50;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: search_time <path> <query>...");
        return;
    };
    let workers = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
    let walk = match oxidex::walk::walk(Path::new(&root), workers) {
        Ok(walk) => walk,
        Err(e) => {
            eprintln!("{root}: {e}");
            return;
        }
    };
    let index = walk.index;
    println!("{root}: {} entries", entry_count(&index));

    for query in args {
        let mut times = Vec::new();
        let mut matches = 0;
        for _ in 0..RUNS {
            let start = Instant::now();
            matches = naive_contains(&index, &query);
            times.push(start.elapsed());
        }
        let naive = median(&mut times);

        let mut hits = Vec::new();
        times.clear();
        for _ in 0..RUNS {
            let start = Instant::now();
            oxidex::search::search(&index, &query, LIMIT, &mut hits);
            times.push(start.elapsed());
        }
        println!(
            "{query:?}: {matches} matches, naive {naive:?}, search {:?}",
            median(&mut times)
        );
    }
}

fn entry_count(index: &Index) -> u32 {
    let mut entries = 0;
    while index.name(entries).is_some() {
        entries += 1;
    }
    entries
}

// The baseline: a plain substring check on every lowercased name.
fn naive_contains(index: &Index, query: &str) -> usize {
    let query = query.to_lowercase();
    let mut matches = 0;
    let mut id = 0;
    while let Some(name) = index.name(id) {
        if name.contains(&query) {
            matches += 1;
        }
        id += 1;
    }
    matches
}

fn median(times: &mut [Duration]) -> Duration {
    times.sort();
    times[times.len() / 2]
}
