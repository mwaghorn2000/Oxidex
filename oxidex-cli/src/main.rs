// Command-line search, for trying search on real folders:
// `cargo run --release -- <folder>...`
// Walks each folder once. In a terminal, results update live as you type,
// Up and Down move the highlight, Enter opens it and quits, and Esc or
// Ctrl-C quits. With piped input, reads one query per line instead,
// so it can be scripted. Opened files are kept in a history file that
// ranks them higher next time (see `history_file`).
use std::io::{self, BufRead, IsTerminal, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

mod history_file;
mod open;

use oxidex::history::History;
use oxidex::index::Index;
use oxidex::score::{Scorer, default_scorers};
use oxidex::search::{Cancel, Hit, Sources, search};

// How many results to print per query in line mode.
const LIMIT: usize = 20;

fn main() {
    let folders: Vec<String> = std::env::args().skip(1).collect();
    if folders.is_empty() {
        eprintln!("usage: oxidex <folder>...");
        return;
    }
    let history_path = history_file::path();
    let mut history = match &history_path {
        Some(path) => history_file::load(path),
        None => History::new(),
    };
    let indexes = index_all(&folders);
    if indexes.is_empty() {
        return;
    }
    history.resolve(&indexes);
    if io::stdin().is_terminal() {
        match live(&indexes, &history) {
            Ok(Some(opened)) => remember(&mut history, history_path.as_deref(), &opened),
            Ok(None) => {}
            Err(e) => eprintln!("terminal error: {e}"),
        }
    } else {
        lines(&indexes, &history);
    }
}

// Counts an open of `opened` and saves the history. There's no `resolve`
// after `record_open`, because the CLI quits once a file is open.
fn remember(history: &mut History, path: Option<&Path>, opened: &Path) {
    if let Err(e) = history.record_open(opened, now()) {
        eprintln!("{}: not added to history: {e}", opened.display());
        return;
    }
    let Some(path) = path else {
        eprintln!("history not saved: no HOME or APPDATA to keep it in");
        return;
    };
    if let Err(e) = history_file::save(history, path) {
        eprintln!("{}: history not saved: {e}", path.display());
    }
}

// Walks each folder into its own index. Folders inside another one are left
// out, so no file shows up twice, and so are folders that can't be read.
fn index_all(folders: &[String]) -> Vec<Arc<Index>> {
    let workers = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
    let roots: Vec<PathBuf> = folders.iter().map(PathBuf::from).collect();
    let mut indexes = Vec::new();
    for root in oxidex::walk::distinct_roots(&roots) {
        let folder = root.display();
        let start = Instant::now();
        match oxidex::walk::walk(&root, workers) {
            Ok(walk) => {
                let entries = walk.index.len();
                eprintln!("{folder}: {entries} entries in {:.0?}", start.elapsed());
                indexes.push(Arc::new(walk.index));
            }
            Err(e) => eprintln!("{folder}: {e}"),
        }
    }
    indexes
}

// Line mode: one query per line, results printed below it. Line mode only
// runs with piped input, so there's no prompt: nobody is there to see it,
// and it would end up mixed into the output.
fn lines(indexes: &[Arc<Index>], history: &History) {
    let scorers = default_scorers();
    let mut out = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // Only the line ending: spaces are part of the query (RANKING.md).
        let query = line.trim_end_matches(['\n', '\r']);
        if query.is_empty() {
            continue;
        }
        let elapsed = timed_search(indexes, history, query, LIMIT, &scorers, &mut out);
        for (rank, hit) in out.iter().enumerate() {
            let index = &indexes[hit.index as usize];
            let path = index.full_path(hit.id).unwrap_or_default();
            println!(
                "{:>3}. {}  ({:?}, {})",
                rank + 1,
                path.display(),
                hit.tier,
                hit.score
            );
        }
        println!("{} results in {elapsed:.1?}", out.len());
    }
}

// Live mode: the query on the top line, results redrawn under it on every
// key press. Returns the path opened, if any.
fn live(indexes: &[Arc<Index>], history: &History) -> io::Result<Option<PathBuf>> {
    let _screen = Screen::enter()?;
    let scorers = default_scorers();
    let mut query = String::new();
    let mut out = Vec::new();
    let mut elapsed = Duration::ZERO;
    // Row of the highlighted result in `out`.
    let mut selected = 0;
    // Why the last open failed, shown until the next key.
    let mut error = None;
    loop {
        draw(indexes, &query, &out, selected, elapsed, error.take())?;
        match event::read()? {
            // Windows also reports key releases; only act on presses.
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Enter => {
                    let Some(hit) = out.get(selected) else {
                        continue;
                    };
                    let Some(path) = indexes[hit.index as usize].full_path(hit.id) else {
                        continue;
                    };
                    // Like a launcher popup: done once something is open.
                    match open::open(&path) {
                        Ok(()) => return Ok(Some(path)),
                        Err(e) => error = Some(format!("Couldn't open {}: {e}", path.display())),
                    }
                    continue;
                }
                KeyCode::Up => {
                    selected = move_selection(selected, false, out.len());
                    continue;
                }
                KeyCode::Down => {
                    selected = move_selection(selected, true, out.len());
                    continue;
                }
                // A new query brings new results, so start again at the top.
                KeyCode::Char(c) => {
                    query.push(c);
                    selected = 0;
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                }
                _ => continue,
            },
            // A resize changes how many results fit, so search again.
            Event::Resize(..) => {}
            _ => continue,
        }
        let (_, rows) = terminal::size()?;
        // One row for the query, one for the status line.
        let limit = usize::from(rows).saturating_sub(2).max(1);
        elapsed = timed_search(indexes, history, &query, limit, &scorers, &mut out);
        // A resize can leave fewer results than the highlighted row.
        selected = clamp_selection(selected, out.len());
    }
}

// One row down or up from `selected`, stopping at the first and last of
// `len` results rather than wrapping.
fn move_selection(selected: usize, down: bool, len: usize) -> usize {
    if down {
        clamp_selection(selected + 1, len)
    } else {
        selected.saturating_sub(1)
    }
}

// Keeps a highlighted row inside `len` results; row 0 when there are none.
fn clamp_selection(selected: usize, len: usize) -> usize {
    selected.min(len.saturating_sub(1))
}

// Raw mode and the alternate screen, undone on drop so the terminal is
// restored however `live` exits, including by `?` on an error.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Screen> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn draw(
    indexes: &[Arc<Index>],
    query: &str,
    hits: &[Hit],
    selected: usize,
    elapsed: Duration,
    error: Option<String>,
) -> io::Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = usize::from(cols);
    let mut stdout = io::stdout();
    queue!(
        stdout,
        Clear(ClearType::All),
        MoveTo(0, 0),
        Print("> "),
        Print(query)
    )?;

    // Results go on rows 1 up to the status line on the last row.
    for (i, (row, hit)) in (1..rows.saturating_sub(1)).zip(hits).enumerate() {
        let index = &indexes[hit.index as usize];
        let path = index.full_path(hit.id).unwrap_or_default();
        let path = path.display().to_string();
        // Split so the file name can be drawn in bold and the folder dimmed.
        let (folder, name) = match path.rfind(['/', '\\']) {
            Some(i) => path.split_at(i + 1),
            None => ("", path.as_str()),
        };
        let (folder, name) = fit(folder, name, width.saturating_sub(2));
        let highlight = if i == selected {
            Attribute::Reverse
        } else {
            Attribute::NoReverse
        };
        queue!(
            stdout,
            MoveTo(2, row),
            SetAttribute(highlight),
            SetAttribute(Attribute::Dim),
            Print(folder),
            // Not `Reset`, which would also end the highlight.
            SetAttribute(Attribute::NormalIntensity),
            SetAttribute(Attribute::Bold),
            Print(name),
            SetAttribute(Attribute::Reset),
        )?;
    }

    let status = if let Some(error) = error {
        error
    } else if query.is_empty() {
        "Type to search. Up and Down select, Enter opens. Esc quits.".to_string()
    } else {
        format!("{} results in {elapsed:.1?}", hits.len())
    };
    queue!(
        stdout,
        MoveTo(0, rows.saturating_sub(1)),
        SetAttribute(Attribute::Dim),
        Print(status),
        SetAttribute(Attribute::Reset),
    )?;
    // Leave the cursor at the end of the query, where typing goes.
    let cursor = 2 + query.chars().count();
    queue!(stdout, MoveTo(cursor.min(width) as u16, 0))?;
    stdout.flush()
}

// Shortens a path to fit `width` characters, keeping the file name: the
// folder loses characters from the left, behind a "…". Only a name too long
// for the line on its own is cut, from the right.
fn fit(folder: &str, name: &str, width: usize) -> (String, String) {
    let folder_len = folder.chars().count();
    let name_len = name.chars().count();
    if folder_len + name_len <= width {
        return (folder.to_string(), name.to_string());
    }
    if name_len >= width {
        if name_len == width || width == 0 {
            let kept: String = name.chars().take(width).collect();
            return (String::new(), kept);
        }
        let kept: String = name.chars().take(width - 1).collect();
        return (String::new(), format!("{kept}…"));
    }
    // The name fits, so the folder gets the rest of the line, one character
    // of it spent on the "…". The folder is longer than that room, or the
    // whole path would have fitted above.
    let room = width - name_len;
    let kept: String = folder.chars().skip(folder_len - (room - 1)).collect();
    (format!("…{kept}"), name.to_string())
}

fn timed_search(
    indexes: &[Arc<Index>],
    history: &History,
    query: &str,
    limit: usize,
    scorers: &[Box<dyn Scorer>],
    out: &mut Vec<Hit>,
) -> Duration {
    let start = Instant::now();
    // Nothing cancels these searches, so `search` can't fail.
    let sources = Sources { indexes, history };
    let _ = search(
        &sources,
        query,
        limit,
        scorers,
        now(),
        &Cancel::never(),
        out,
    );
    start.elapsed()
}

// Seconds since 1970, for the modified-time scorer.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fitted(folder: &str, name: &str, width: usize) -> String {
        let (folder, name) = fit(folder, name, width);
        format!("{folder}{name}")
    }

    #[test]
    fn short_path_is_unchanged() {
        assert_eq!(fitted("/home/", "a.txt", 20), "/home/a.txt");
    }

    #[test]
    fn long_folder_is_cut_from_the_left() {
        assert_eq!(fitted("/home/mitch/projects/", "a.txt", 12), "…jects/a.txt");
    }

    #[test]
    fn name_is_kept_whole_when_it_fits() {
        assert_eq!(fitted("/home/mitch/", "readme.md", 10), "…readme.md");
    }

    #[test]
    fn name_too_long_for_the_line_is_cut_from_the_right() {
        assert_eq!(fitted("/home/", "abcdefghij", 5), "abcd…");
    }

    // Widths count characters, not bytes, so a non-ASCII path isn't cut
    // inside a character.
    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(fitted("/café/", "é.txt", 8), "…é/é.txt");
    }

    #[test]
    fn zero_width_gives_nothing() {
        assert_eq!(fitted("/home/", "a.txt", 0), "");
    }

    #[test]
    fn selection_moves_and_stops_at_the_ends() {
        assert_eq!(move_selection(0, true, 3), 1);
        assert_eq!(move_selection(2, true, 3), 2);
        assert_eq!(move_selection(1, false, 3), 0);
        assert_eq!(move_selection(0, false, 3), 0);
        assert_eq!(move_selection(0, true, 0), 0);
    }

    #[test]
    fn selection_stays_inside_the_results() {
        assert_eq!(clamp_selection(2, 5), 2);
        assert_eq!(clamp_selection(5, 5), 4);
        assert_eq!(clamp_selection(3, 1), 0);
        assert_eq!(clamp_selection(3, 0), 0);
    }
}
