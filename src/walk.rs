use std::io;
use std::path::Path;

use crate::index::Index;

pub struct Walk {
    pub index: Index,
    // Folders that couldn't be read plus names that aren't valid UTF-8.
    pub skipped: u64,
}

// Crawls `root` and returns an index of everything under it. The root entry's
// name is its full canonical path, so `full_path` gives absolute paths. Only
// fails if `root` itself can't be read.
pub fn walk(_root: &Path) -> io::Result<Walk> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

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
        let walk = walk(&dir).unwrap();
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

        let walk = walk(&dir).unwrap();
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
    fn relative_root_becomes_absolute() {
        let walk = walk(Path::new("src")).unwrap();
        let root = walk.index.full_path(0).unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("src"));
    }

    #[test]
    fn missing_root_is_error() {
        let dir = temp_dir("missing");
        assert!(walk(&dir.join("nope")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let dir = temp_dir("symlink");
        fs::create_dir(dir.join("a")).unwrap();
        fs::write(dir.join("a/f.txt"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();

        let walk = walk(&dir).unwrap();
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

        let result = walk(&dir);
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

        let walk = walk(&dir).unwrap();
        assert_eq!(
            all_paths(&walk.index),
            vec![dir.clone(), dir.join("good.txt")]
        );
        assert_eq!(walk.skipped, 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
