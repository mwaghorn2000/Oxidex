// Command-line search, for trying search on real folders:
// `cargo run --release -- <folder>...`
// Walks each folder once, then reads one query per line and prints the best
// matches. Ctrl-D quits.
use std::io::{self, BufRead, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use oxidex::index::Index;
use oxidex::score::default_scorers;
use oxidex::search::{Cancel, search};

// How many results to print per query.
const LIMIT: usize = 20;

fn main() {
    let folders: Vec<String> = std::env::args().skip(1).collect();
    if folders.is_empty() {
        eprintln!("usage: oxidex <folder>...");
        return;
    }
    let indexes = index_all(&folders);
    if indexes.is_empty() {
        return;
    }

    let scorers = default_scorers();
    let mut out = Vec::new();
    let mut line = String::new();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        line.clear();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let query = line.trim();
        if query.is_empty() {
            continue;
        }

        let start = Instant::now();
        let _ = search(
            &indexes,
            query,
            LIMIT,
            &scorers,
            now(),
            &Cancel::never(),
            &mut out,
        );
        let elapsed = start.elapsed();

        for (rank, hit) in out.iter().enumerate() {
            let index = &indexes[hit.index as usize];
            let path = index.full_path(hit.id).unwrap_or_default();
            println!(
                "{:>3}. {}  ({:?}, {})",
                rank + 1,
                path.display(),
                hit.tier,
                hit.score
            );
        }
        println!("{} results in {elapsed:.1?}", out.len());
    }
}

// Walks each folder into its own index. Folders that can't be read are
// reported and left out.
fn index_all(folders: &[String]) -> Vec<Index> {
    let workers = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
    let mut indexes = Vec::new();
    for folder in folders {
        let start = Instant::now();
        match oxidex::walk::walk(Path::new(folder), workers) {
            Ok(walk) => {
                let entries = walk.index.masks().len();
                eprintln!("{folder}: {entries} entries in {:.0?}", start.elapsed());
                indexes.push(walk.index);
            }
            Err(e) => eprintln!("{folder}: {e}"),
        }
    }
    indexes
}

// Seconds since 1970, for the modified-time scorer.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
