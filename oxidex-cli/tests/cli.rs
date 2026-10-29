// Runs the command-line binary with piped input, as a script would.
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

// Runs the CLI over a fresh folder holding `files`, feeding it `input`, and
// returns what it printed to stdout. `history` is the history file's
// contents, or `None` for no file; `{dir}` in it becomes the folder's path.
fn run_cli(test: &str, files: &[&str], history: Option<&str>, input: &str) -> String {
    let dir = std::env::temp_dir().join(format!("oxidex-cli-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for file in files {
        fs::write(dir.join(file), "").unwrap();
    }
    // Beside the folder, not in it, so it isn't indexed. Always set, so a
    // test never reads the real user's history.
    let history_path = dir.with_extension("history.json");
    let _ = fs::remove_file(&history_path);
    if let Some(history) = history {
        let folder = fs::canonicalize(&dir).unwrap();
        fs::write(&history_path, history.replace("{dir}", &json_path(&folder))).unwrap();
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_oxidex"))
        .arg(&dir)
        .env("OXIDEX_HISTORY", &history_path)
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
    let _ = fs::remove_file(&history_path);
    String::from_utf8(output.stdout).unwrap()
}

// `path` escaped for a JSON string, for Windows' backslashes.
fn json_path(path: &std::path::Path) -> String {
    path.to_str().unwrap().replace('\\', "\\\\")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn first_line(stdout: &str) -> &str {
    stdout.lines().next().unwrap_or_default()
}

#[test]
fn line_mode_prints_only_results() {
    let stdout = run_cli("results", &["readme.md"], None, "readme\n");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("  1. "), "{stdout}");
    assert!(lines[0].contains("readme.md"), "{stdout}");
    assert!(lines[1].starts_with("1 results in "), "{stdout}");
}

// Trimmed to "foo", the query would also find "foobar.txt".
#[test]
fn line_mode_keeps_spaces_in_the_query() {
    let stdout = run_cli("spaces", &["foo bar.txt", "foobar.txt"], None, "foo \n");
    assert!(stdout.contains("foo bar.txt"), "{stdout}");
    assert!(!stdout.contains("foobar.txt"), "{stdout}");
}

#[test]
fn line_mode_ranks_opened_files_first() {
    // The two names tie without history, so whichever one was opened must
    // come first, not whichever the walk happened to find first.
    let files = ["notes-a.txt", "notes-b.txt"];
    for opened in files {
        let path = json_path(&std::path::Path::new("{dir}").join(opened));
        let history = format!(
            r#"{{"records": [{{"path": "{path}", "score": 5.0, "counted_at": {}}}]}}"#,
            now()
        );
        let stdout = run_cli(opened, &files, Some(&history), "notes\n");
        assert!(first_line(&stdout).contains(opened), "{stdout}");
    }
}

// A history file that doesn't parse costs its ranking, not the search.
#[test]
fn line_mode_survives_a_corrupt_history() {
    let stdout = run_cli("corrupt", &["readme.md"], Some("{not json"), "readme\n");
    assert!(first_line(&stdout).contains("readme.md"), "{stdout}");
}
