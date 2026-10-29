// Where the CLI keeps its `History` between runs, and loading and saving it.
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use oxidex::history::History;

// The history file for this system, from the environment. `None` when no
// variable says where home is, so there's nowhere to keep it.
pub fn path() -> Option<PathBuf> {
    path_from(cfg!(windows), |name| std::env::var_os(name))
}

// `OXIDEX_HISTORY` names the file itself, so tests and scripts can keep
// clear of the user's real history. Otherwise it's the XDG data folder, or
// `%APPDATA%` on Windows. `var` looks up an environment variable; empty
// ones count as unset, as the XDG spec says.
fn path_from(windows: bool, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(file) = var("OXIDEX_HISTORY") {
        return Some(file);
    }
    let folder = if windows {
        var("APPDATA")?
    } else if let Some(data) = var("XDG_DATA_HOME") {
        data
    } else {
        var("HOME")?.join(".local").join("share")
    };
    Some(folder.join("oxidex").join("history.json"))
}

// Loads the history at `path`. A missing file is a first run. A file that
// can't be read is reported and left alone until the next save replaces
// it: losing frecency is better than not starting.
pub fn load(path: &Path) -> History {
    let result = File::open(path).and_then(History::read_from);
    match result {
        Ok(history) => history,
        Err(e) if e.kind() == io::ErrorKind::NotFound => History::new(),
        Err(e) => {
            eprintln!("{}: {e}; starting with an empty history", path.display());
            History::new()
        }
    }
}

// Saves `history` to `path`, atomically (see `write_atomically`).
pub fn save(history: &History, path: &Path) -> io::Result<()> {
    write_atomically(path, |file| history.write_to(file))
}

// Writes `path` through a temporary file in the same folder, renamed over
// the old one, so a crash mid-write can't leave half a file. Same folder,
// because a rename can't cross filesystems. Creates the folder if needed.
pub fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut temp = path.as_os_str().to_owned();
    // The process ID keeps two CLIs saving at once off each other's file.
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let result = File::create(&temp)
        .and_then(|mut file| {
            write(&mut file)?;
            // On disk before the rename, or a crash could leave an empty file.
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // `path_from` with only the variables in `vars` set.
    fn path_with(windows: bool, vars: &[(&str, &str)]) -> Option<PathBuf> {
        path_from(windows, |name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        })
    }

    #[test]
    fn override_wins() {
        let vars = [("OXIDEX_HISTORY", "/tmp/h.json"), ("XDG_DATA_HOME", "/x")];
        assert_eq!(path_with(false, &vars), Some("/tmp/h.json".into()));
        assert_eq!(path_with(true, &vars), Some("/tmp/h.json".into()));
    }

    #[test]
    fn xdg_data_home_comes_before_home() {
        let vars = [("XDG_DATA_HOME", "/data"), ("HOME", "/home/m")];
        let want = Path::new("/data").join("oxidex").join("history.json");
        assert_eq!(path_with(false, &vars), Some(want));
    }

    #[test]
    fn falls_back_to_local_share() {
        let vars = [("XDG_DATA_HOME", ""), ("HOME", "/home/m")];
        let want = Path::new("/home/m/.local/share/oxidex/history.json");
        assert_eq!(path_with(false, &vars), Some(want.into()));
    }

    #[test]
    fn windows_uses_appdata() {
        let vars = [("APPDATA", r"C:\Users\m\AppData\Roaming"), ("HOME", "/h")];
        let want = Path::new(r"C:\Users\m\AppData\Roaming")
            .join("oxidex")
            .join("history.json");
        assert_eq!(path_with(true, &vars), Some(want));
    }

    #[test]
    fn no_home_means_no_path() {
        assert_eq!(path_with(false, &[]), None);
        assert_eq!(path_with(true, &[("HOME", "/h")]), None);
    }

    #[test]
    fn saves_and_loads_through_a_new_folder() {
        let dir = std::env::temp_dir().join(format!("oxidex-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("sub").join("history.json");
        let mut history = History::new();
        history.record_open(Path::new("/a/b.txt"), 100).unwrap();
        save(&history, &path).unwrap();
        // Saving again replaces the file rather than failing on it.
        save(&history, &path).unwrap();
        let loaded = load(&path);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded.records(), history.records());
    }

    #[test]
    fn missing_or_corrupt_file_gives_empty_history() {
        let dir = std::env::temp_dir().join(format!("oxidex-corrupt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        assert!(load(&path).records().is_empty());
        fs::write(&path, "{not json").unwrap();
        let loaded = load(&path);
        fs::remove_dir_all(&dir).unwrap();
        assert!(loaded.records().is_empty());
    }
}
