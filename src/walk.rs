use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::UNIX_EPOCH;

use crate::index::Index;

pub struct Walk {
    pub index: Index,
    // Folders that couldn't be read, names that aren't valid UTF-8, and
    // entries the OS failed to list.
    pub skipped: u64,
}

// Crawls `root` and returns an index of everything under it. The root entry's
// name is its full canonical path, so `full_path` gives absolute paths. Only
// fails if `root` itself can't be read. `workers` is how many threads read
// folders.
pub fn walk(root: &Path, workers: NonZeroUsize) -> io::Result<Walk> {
    let root = fs::canonicalize(root)?;
    let meta = fs::metadata(&root)?;
    let Some(name) = root.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "root path is not valid UTF-8",
        ));
    };
    let is_dir = meta.is_dir();
    let size = if is_dir { 0 } else { meta.len() };
    let mut walk = Walk {
        index: Index::new(),
        skipped: 0,
    };
    let Ok(root_id) = walk
        .index
        .push(name, u32::MAX, is_dir, size, modified_secs(&meta))
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "root path is too long",
        ));
    };
    if !is_dir {
        return Ok(walk);
    }

    // Worker threads read folders and send back batches; this thread is the
    // only one that touches the index, so IDs need no locking or renumbering.
    let first = read_batch(fs::read_dir(&root)?, root_id);
    let queue = Mutex::new(Queue {
        dirs: Vec::new(),
        done: false,
    });
    let ready = Condvar::new();
    let (tx, rx) = mpsc::channel();
    thread::scope(|s| {
        let (queue, ready) = (&queue, &ready);
        for _ in 0..workers.get() {
            let tx = tx.clone();
            s.spawn(move || worker(queue, ready, tx));
        }
        drop(tx);

        // Folders queued or being read. Each one sends exactly one batch,
        // so at zero every folder has been pushed.
        let mut outstanding = push_batch(&mut walk, first, queue, ready);
        while outstanding > 0 {
            let Ok(batch) = rx.recv() else { break };
            outstanding -= 1;
            outstanding += push_batch(&mut walk, batch, queue, ready);
        }
        lock(queue).done = true;
        ready.notify_all();
    });
    Ok(walk)
}

// Folders waiting to be read. `done` tells idle workers to exit.
struct Queue {
    dirs: Vec<(PathBuf, u32)>,
    done: bool,
}

// One folder's contents, read by a worker and waiting to be pushed.
struct Batch {
    parent: u32,
    found: Vec<Found>,
    skipped: u64,
}

struct Found {
    name: String,
    size: u64,
    modified: u64,
    // Set only for folders, which still need to be read.
    path: Option<PathBuf>,
}

// A worker only panics on a bug, and the queue stays valid if one does, so
// a poisoned lock is safe to keep using.
fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

fn worker(queue: &Mutex<Queue>, ready: &Condvar, tx: mpsc::Sender<Batch>) {
    loop {
        let (path, id) = {
            let mut q = lock(queue);
            loop {
                if q.done {
                    return;
                }
                if let Some(dir) = q.dirs.pop() {
                    break dir;
                }
                q = ready.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let batch = match fs::read_dir(&path) {
            Ok(entries) => read_batch(entries, id),
            Err(_) => Batch {
                parent: id,
                found: Vec::new(),
                skipped: 1,
            },
        };
        if tx.send(batch).is_err() {
            return;
        }
    }
}

fn read_batch(entries: fs::ReadDir, parent: u32) -> Batch {
    let mut batch = Batch {
        parent,
        found: Vec::new(),
        skipped: 0,
    };
    for entry in entries {
        let Ok(entry) = entry else {
            batch.skipped += 1;
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            batch.skipped += 1;
            continue;
        };
        // `file_type` doesn't follow symlinks, so a link to a folder is
        // stored as a plain entry and never entered.
        let Ok(file_type) = entry.file_type() else {
            batch.skipped += 1;
            continue;
        };
        let is_dir = file_type.is_dir();
        let (size, modified) = match entry.metadata() {
            Ok(meta) if is_dir => (0, modified_secs(&meta)),
            Ok(meta) => (meta.len(), modified_secs(&meta)),
            Err(_) => (0, 0),
        };
        let path = if is_dir { Some(entry.path()) } else { None };
        batch.found.push(Found {
            name,
            size,
            modified,
            path,
        });
    }
    batch
}

// Pushes a batch into the index and queues its subfolders. Returns how many
// folders it queued.
fn push_batch(walk: &mut Walk, batch: Batch, queue: &Mutex<Queue>, ready: &Condvar) -> usize {
    walk.skipped += batch.skipped;
    let mut dirs = Vec::new();
    for f in batch.found {
        let is_dir = f.path.is_some();
        let Ok(id) = walk
            .index
            .push(&f.name, batch.parent, is_dir, f.size, f.modified)
        else {
            walk.skipped += 1;
            continue;
        };
        if let Some(path) = f.path {
            dirs.push((path, id));
        }
    }
    let queued = dirs.len();
    if queued > 0 {
        lock(queue).dirs.extend(dirs);
        // Wake one worker per folder rather than all of them, so idle
        // workers don't all fight over the lock for a single folder.
        for _ in 0..queued {
            ready.notify_one();
        }
    }
    queued
}

// Seconds since 1970, or 0 if the OS can't say.
fn modified_secs(meta: &fs::Metadata) -> u64 {
    match meta.modified().map(|t| t.duration_since(UNIX_EPOCH)) {
        Ok(Ok(d)) => d.as_secs(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const WORKERS: NonZeroUsize = NonZeroUsize::new(4).unwrap();

    // A fresh, empty folder per test so tests can run in parallel.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxidex-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    fn all_paths(index: &Index) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut id = 0;
        while index.name(id).is_some() {
            paths.push(index.full_path(id).unwrap());
            id += 1;
        }
        paths.sort();
        paths
    }

    #[test]
    fn root_name_is_full_path() {
        let dir = temp_dir("root_name");
        let walk = walk(&dir, WORKERS).unwrap();
        assert_eq!(walk.index.display_name(0), dir.to_str());
        assert_eq!(walk.index.full_path(0), Some(dir.clone()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_nested_entries() {
        let dir = temp_dir("nested");
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("a/b/c.txt"), "hi").unwrap();
        fs::write(dir.join("d.txt"), "").unwrap();

        let walk = walk(&dir, WORKERS).unwrap();
        let expected = vec![
            dir.clone(),
            dir.join("a"),
            dir.join("a/b"),
            dir.join("a/b/c.txt"),
            dir.join("d.txt"),
        ];
        assert_eq!(all_paths(&walk.index), expected);
        assert_eq!(walk.skipped, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_worker_finds_everything() {
        let dir = temp_dir("one_worker");
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("a/b/c.txt"), "").unwrap();

        let walk = walk(&dir, NonZeroUsize::MIN).unwrap();
        let expected = vec![
            dir.clone(),
            dir.join("a"),
            dir.join("a/b"),
            dir.join("a/b/c.txt"),
        ];
        assert_eq!(all_paths(&walk.index), expected);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relative_root_becomes_absolute() {
        let walk = walk(Path::new("src"), WORKERS).unwrap();
        let root = walk.index.full_path(0).unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("src"));
    }

    #[test]
    fn missing_root_is_error() {
        let dir = temp_dir("missing");
        assert!(walk(&dir.join("nope"), WORKERS).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let dir = temp_dir("symlink");
        fs::create_dir(dir.join("a")).unwrap();
        fs::write(dir.join("a/f.txt"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();

        let walk = walk(&dir, WORKERS).unwrap();
        let expected = vec![
            dir.clone(),
            dir.join("a"),
            dir.join("a/f.txt"),
            dir.join("link"),
        ];
        assert_eq!(all_paths(&walk.index), expected);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folder_is_skipped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("unreadable");
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden.txt"), "").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let result = walk(&dir, WORKERS);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let walk = result.unwrap();
        // The folder itself is listed; only its contents are missing.
        assert_eq!(all_paths(&walk.index), vec![dir.clone(), locked]);
        assert_eq!(walk.skipped, 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_name_is_skipped() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = temp_dir("non_utf8");
        fs::write(dir.join(OsStr::from_bytes(b"bad\xff.txt")), "").unwrap();
        fs::write(dir.join("good.txt"), "").unwrap();

        let walk = walk(&dir, WORKERS).unwrap();
        assert_eq!(
            all_paths(&walk.index),
            vec![dir.clone(), dir.join("good.txt")]
        );
        assert_eq!(walk.skipped, 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
