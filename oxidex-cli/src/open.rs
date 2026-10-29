// Opens a file with the system's default app, as double-clicking it would.
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// How long to watch an opener before taking it as a success. Most openers
// hand the file over and exit at once, but some stay running as long as the
// app does, and waiting for those would hang the CLI.
const WAIT: Duration = Duration::from_secs(2);

// Opens `path`, or says why it couldn't.
pub fn open(path: &Path) -> io::Result<()> {
    let (program, args) = opener(std::env::consts::OS, path);
    let err = match run(program, &args) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    // Many WSL setups have no `xdg-open`, or one with no handlers, but can
    // still hand files to Windows.
    if program == "xdg-open" && is_wsl() {
        return open_wsl(path).map_err(|_| err);
    }
    Err(err)
}

// The program and arguments that open `path` on operating system `os`, as
// `std::env::consts::OS` names it.
fn opener(os: &str, path: &Path) -> (&'static str, Vec<OsString>) {
    let path = path.as_os_str().to_owned();
    match os {
        "macos" => ("open", vec![path]),
        // `start` is built into `cmd`. Its first quoted argument is the
        // window title, so an empty one keeps a quoted path from taking it.
        "windows" => ("cmd", vec!["/C".into(), "start".into(), "".into(), path]),
        _ => ("xdg-open", vec![path]),
    }
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version").is_ok_and(|v| is_wsl_version(&v))
}

// WSL kernels name themselves in `/proc/version`, as "Microsoft" in WSL 1
// and "microsoft" in WSL 2.
fn is_wsl_version(version: &str) -> bool {
    version.to_lowercase().contains("microsoft")
}

// `wslview` (from wslu) first, then Windows' own `explorer.exe`, which needs
// a Windows path.
fn open_wsl(path: &Path) -> io::Result<()> {
    if run("wslview", &[path.as_os_str().to_owned()]).is_ok() {
        return Ok(());
    }
    let output = Command::new("wslpath").arg("-w").arg(path).output()?;
    if !output.status.success() {
        return Err(io::Error::other("wslpath failed"));
    }
    let windows_path = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    // `explorer.exe` exits with status 1 even when it opened the file, so
    // only a failure to start it counts.
    spawn("explorer.exe", &[windows_path.into()]).map(|_| ())
}

// Starts `program` and gives it up to `WAIT` to fail. One still running by
// then is left to carry on.
fn run(program: &str, args: &[OsString]) -> io::Result<()> {
    let mut child = spawn(program, args)?;
    let start = Instant::now();
    while start.elapsed() < WAIT {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(io::Error::other(format!("{program} failed: {status}")));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

// Without its own input and output, an opener can't read keys meant for the
// CLI or print over the results.
fn spawn(program: &str, args: &[OsString]) -> io::Result<Child> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("{program}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn macos_uses_open() {
        let (program, a) = opener("macos", Path::new("/a b/c.txt"));
        assert_eq!(program, "open");
        assert_eq!(a, args(&["/a b/c.txt"]));
    }

    #[test]
    fn windows_uses_start_with_an_empty_title() {
        let (program, a) = opener("windows", Path::new(r"C:\a b\c.txt"));
        assert_eq!(program, "cmd");
        assert_eq!(a, args(&["/C", "start", "", r"C:\a b\c.txt"]));
    }

    #[test]
    fn others_use_xdg_open() {
        for os in ["linux", "freebsd", "openbsd"] {
            let (program, a) = opener(os, Path::new("/c.txt"));
            assert_eq!(program, "xdg-open");
            assert_eq!(a, args(&["/c.txt"]));
        }
    }

    #[test]
    fn detects_wsl_from_proc_version() {
        assert!(is_wsl_version(
            "Linux version 6.6.87.2-microsoft-standard-WSL2 (gcc) #1 SMP"
        ));
        assert!(is_wsl_version("Linux version 4.4.0-19041-Microsoft"));
        assert!(!is_wsl_version(
            "Linux version 6.8.0-45-generic (buildd@ubuntu)"
        ));
    }
}
