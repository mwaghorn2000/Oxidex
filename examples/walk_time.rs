// Times `walk` on a folder: `cargo run --release --example walk_time -- <path>`
use std::path::Path;
use std::time::Instant;

fn main() {
    let Some(root) = std::env::args().nth(1) else {
        eprintln!("usage: walk_time <path>");
        return;
    };
    let start = Instant::now();
    let walk = match oxidex::walk::walk(Path::new(&root)) {
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
        "{root}: {entries} entries, {} skipped, {elapsed:?}",
        walk.skipped
    );
}
