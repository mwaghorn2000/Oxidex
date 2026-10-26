// Times `walk` on a folder:
// `cargo run --release --example walk_time -- <path> [workers]`
// Workers defaults to one per CPU thread.
use std::num::NonZeroUsize;
use std::path::Path;
use std::thread;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: walk_time <path> [workers]");
        return;
    };
    let workers = match args.next() {
        Some(arg) => match arg.parse::<NonZeroUsize>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("workers must be a whole number above 0");
                return;
            }
        },
        None => thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
    };
    let start = Instant::now();
    let walk = match oxidex::walk::walk(Path::new(&root), workers) {
        Ok(walk) => walk,
        Err(e) => {
            eprintln!("{root}: {e}");
            return;
        }
    };
    let elapsed = start.elapsed();

    let mut entries = 0;
    while walk.index.name(entries).is_some() {
        entries += 1;
    }
    println!(
        "{root}: {entries} entries, {} skipped, {workers} workers, {elapsed:?}",
        walk.skipped
    );
}
