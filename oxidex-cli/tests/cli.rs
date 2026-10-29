// Runs the command-line binary with piped input, as a script would.
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

// Runs the CLI over a fresh folder holding `files`, feeding it `input`, and
// returns what it printed to stdout.
fn run_cli(test: &str, files: &[&str], input: &str) -> String {
    let dir = std::env::temp_dir().join(format!("oxidex-cli-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for file in files {
        fs::write(dir.join(file), "").unwrap();
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_oxidex"))
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn line_mode_prints_only_results() {
    let stdout = run_cli("results", &["readme.md"], "readme\n");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("  1. "), "{stdout}");
    assert!(lines[0].contains("readme.md"), "{stdout}");
    assert!(lines[1].starts_with("1 results in "), "{stdout}");
}

// Trimmed to "foo", the query would also find "foobar.txt".
#[test]
fn line_mode_keeps_spaces_in_the_query() {
    let stdout = run_cli("spaces", &["foo bar.txt", "foobar.txt"], "foo \n");
    assert!(stdout.contains("foo bar.txt"), "{stdout}");
    assert!(!stdout.contains("foobar.txt"), "{stdout}");
}
