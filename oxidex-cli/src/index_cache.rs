// Saved indexes, one file per search folder, so live mode can show results
// at once and re-walk in the background. They live in an `indexes` folder
// beside the history file, so `OXIDEX_HISTORY` moves both.
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use oxidex::index::Index;

use crate::history_file;

// The folder for saved indexes, beside the history file at `history`.
pub fn folder(history: &Path) -> Option<PathBuf> {
    Some(history.parent()?.join("indexes"))
}

// The file for `root`, named by a hash of its path so any folder gives a
// valid file name. FNV-1a, because std's hasher isn't promised to stay the
// same between Rust versions, and a new name would orphan every saved file.
fn file(folder: &Path, root: &Path) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in root.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    folder.join(format!("{hash:016x}.index"))
}

// The saved index for `root`, if there's a usable one. Any problem (none
// saved yet, a corrupt or older file) just means walking instead.
pub fn load(folder: &Path, root: &Path) -> Option<Index> {
    let saved = File::open(file(folder, root)).ok()?;
    let index = oxidex::store::read_from(saved).ok()?;
    // Guards against a hash collision: the root entry shows its folder's
    // path. Compared by name only, since Windows roots differ in prefix.
    let path = index.full_path(0)?;
    (path.file_name() == root.file_name()).then_some(index)
}

pub fn save(folder: &Path, root: &Path, index: &Index) -> io::Result<()> {
    history_file::write_atomically(&file(folder, root), |out| {
        oxidex::store::write_to(index, out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxidex-cache-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A saved name must never change, or every saved index is lost. The
    // expected value is FNV-1a of "/home/me", worked out separately.
    #[test]
    fn file_name_is_stable() {
        let name = file(Path::new("c"), Path::new("/home/me"));
        assert_eq!(name, Path::new("c").join("8a93216f4a4854b0.index"));
        assert_ne!(name, file(Path::new("c"), Path::new("/home/mf")));
    }

    #[test]
    fn saved_index_loads_back() {
        let dir = temp_dir("round_trip");
        let root = dir.join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.txt"), "").unwrap();
        let index = oxidex::walk::walk(&root, std::num::NonZeroUsize::MIN)
            .unwrap()
            .index;
        let root = index.full_path(0).unwrap();
        save(&dir, &root, &index).unwrap();
        let loaded = load(&dir, &root).unwrap();
        assert_eq!(loaded.len(), index.len());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_or_corrupt_file_means_walking() {
        let dir = temp_dir("corrupt");
        let root = Path::new("/nowhere/root");
        assert!(load(&dir, root).is_none());
        fs::write(file(&dir, root), b"not an index").unwrap();
        assert!(load(&dir, root).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }
}
