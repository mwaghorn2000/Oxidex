// Command-line search, for trying search on real folders:
// `cargo run --release -- <folder>...`
// Walks each folder once. In a terminal, results update live as you type;
// Esc or Ctrl-C quits. With piped input, reads one query per line instead,
// so it can be scripted.
use std::io::{self, BufRead, IsTerminal, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

use oxidex::index::Index;
use oxidex::score::{Scorer, default_scorers};
use oxidex::search::{Cancel, Hit, search};

// How many results to print per query in line mode.
const LIMIT: usize = 20;

fn main() {
    let folders: Vec<String> = std::env::args().skip(1).collect();
    if folders.is_empty() {
        eprintln!("usage: oxidex <folder>...");
        return;
    }
    let indexes = index_all(&folders);
    if indexes.is_empty() {
        return;
    }
    if io::stdin().is_terminal() {
        if let Err(e) = live(&indexes) {
            eprintln!("terminal error: {e}");
        }
    } else {
        lines(&indexes);
    }
}

// Walks each folder into its own index. Folders that can't be read are
// reported and left out.
fn index_all(folders: &[String]) -> Vec<Index> {
    let workers = thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
    let mut indexes = Vec::new();
    for folder in folders {
        let start = Instant::now();
        match oxidex::walk::walk(Path::new(folder), workers) {
            Ok(walk) => {
                let entries = walk.index.masks().len();
                eprintln!("{folder}: {entries} entries in {:.0?}", start.elapsed());
                indexes.push(walk.index);
            }
            Err(e) => eprintln!("{folder}: {e}"),
        }
    }
    indexes
}

// Line mode: one query per line, results printed below it.
fn lines(indexes: &[Index]) {
    let scorers = default_scorers();
    let mut out = Vec::new();
    let mut line = String::new();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        line.clear();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let query = line.trim();
        if query.is_empty() {
            continue;
        }
        let elapsed = timed_search(indexes, query, LIMIT, &scorers, &mut out);
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
// key press.
fn live(indexes: &[Index]) -> io::Result<()> {
    let _screen = Screen::enter()?;
    let scorers = default_scorers();
    let mut query = String::new();
    let mut out = Vec::new();
    let mut elapsed = Duration::ZERO;
    loop {
        draw(indexes, &query, &out, elapsed)?;
        match event::read()? {
            // Windows also reports key releases; only act on presses.
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char(c) => query.push(c),
                KeyCode::Backspace => {
                    query.pop();
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
        elapsed = timed_search(indexes, &query, limit, &scorers, &mut out);
    }
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

fn draw(indexes: &[Index], query: &str, hits: &[Hit], elapsed: Duration) -> io::Result<()> {
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
    for (row, hit) in (1..rows.saturating_sub(1)).zip(hits) {
        let index = &indexes[hit.index as usize];
        let path = index.full_path(hit.id).unwrap_or_default();
        let path = path.display().to_string();
        // Split so the file name can be drawn in bold and the folder dimmed.
        let (folder, name) = match path.rfind(['/', '\\']) {
            Some(i) => path.split_at(i + 1),
            None => ("", path.as_str()),
        };
        let (folder, name) = fit(folder, name, width.saturating_sub(2));
        queue!(
            stdout,
            MoveTo(2, row),
            SetAttribute(Attribute::Dim),
            Print(folder),
            SetAttribute(Attribute::Reset),
            SetAttribute(Attribute::Bold),
            Print(name),
            SetAttribute(Attribute::Reset),
        )?;
    }

    let status = if query.is_empty() {
        "Type to search. Esc quits.".to_string()
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
    indexes: &[Index],
    query: &str,
    limit: usize,
    scorers: &[Box<dyn Scorer>],
    out: &mut Vec<Hit>,
) -> Duration {
    let start = Instant::now();
    // Nothing cancels these searches, so `search` can't fail.
    let _ = search(indexes, query, limit, scorers, now(), &Cancel::never(), out);
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
}
