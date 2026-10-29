// Times saving and loading a walked index:
// `cargo run --release --example store_time -- <path> <file>`
// Also times the two ways to get masks on load, rebuilding them from the
// names or reading them from disk, to back the choice in `store`.
use std::fs::{self, File};
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const RUNS: usize = 9;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(root), Some(file)) = (args.next(), args.next()) else {
        eprintln!("usage: store_time <path> <file>");
        return Ok(());
    };
    let workers = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
    let start = Instant::now();
    let index = oxidex::walk::walk(Path::new(&root), workers)?.index;
    println!(
        "{root}: {} entries, walk {:?}",
        index.len(),
        start.elapsed()
    );

    let save = median(|| oxidex::store::write_to(&index, File::create(&file)?))?;
    let size = fs::metadata(&file)?.len();
    let load = median(|| oxidex::store::read_from(File::open(&file)?).map(drop))?;
    println!("save {save:?}, load {load:?}, {size} bytes");

    let rebuild = median(|| {
        let mut masks = Vec::with_capacity(index.len());
        for id in 0..index.len() as u32 {
            masks.push(oxidex::index::char_mask(index.name(id).unwrap_or("")));
        }
        std::hint::black_box(masks);
        Ok(())
    })?;
    // What loading saved masks would cost: reading and decoding 8 bytes each.
    let mask_file = format!("{file}.masks");
    let bytes: Vec<u8> = index.masks().iter().flat_map(|m| m.to_le_bytes()).collect();
    fs::write(&mask_file, &bytes)?;
    let read = median(|| {
        let bytes = fs::read(&mask_file)?;
        let (chunks, _) = bytes.as_chunks::<8>();
        let masks: Vec<u64> = chunks.iter().map(|c| u64::from_le_bytes(*c)).collect();
        std::hint::black_box(masks);
        Ok(())
    })?;
    fs::remove_file(&mask_file)?;
    println!(
        "masks: rebuild {rebuild:?}, read {read:?}, {} bytes",
        bytes.len()
    );
    Ok(())
}

fn median(mut f: impl FnMut() -> io::Result<()>) -> io::Result<Duration> {
    let mut times = Vec::new();
    for _ in 0..RUNS {
        let start = Instant::now();
        f()?;
        times.push(start.elapsed());
    }
    times.sort();
    Ok(times[RUNS / 2])
}
