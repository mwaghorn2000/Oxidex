// Runs the command-line binary with piped input, as a script would.
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn line_mode_prints_only_results() {
    let dir = std::env::temp_dir().join(format!("oxidex-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("readme.md"), "").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_oxidex"))
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"readme\n").unwrap();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("  1. "), "{stdout}");
    assert!(lines[0].contains("readme.md"), "{stdout}");
    assert!(lines[1].starts_with("1 results in "), "{stdout}");
}
