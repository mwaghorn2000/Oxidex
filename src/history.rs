// Files opened through Oxidex, for the frecency signal. See RANKING.md,
// "Frecency". The frontend owns a `History`: it calls `record_open` when the
// user opens a result, and decides whether and where to keep it with
// `write_to` and `read_from`. Core never picks a file location.
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;
// Opens closer together than this count once, so opening a file ten times
// in one sitting doesn't make it look ten times as popular.
const MIN_GAP: u64 = 12 * HOUR;
// A score halves every `HALF_LIFE`, so files you stopped using fade out.
const HALF_LIFE: u64 = 7 * DAY;

// Saved as JSON: `{"records": [{"path": ..., "score": ..., "counted_at": ...}]}`.
// An object rather than a bare list, so fields can be added later without
// breaking old files.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History {
    records: Vec<Record>,
    // (index number, entry ID, record number), filled in by `resolve`, which
    // isn't built yet. Worked out again after every walk, so it isn't saved.
    #[allow(dead_code)]
    #[serde(skip)]
    resolved: Vec<(u32, u32, usize)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    // Kept as a `String`: `record_open` only accepts UTF-8 paths, because
    // JSON strings must be UTF-8.
    path: String,
    // The score as of `counted_at`. Decay to now with `frecency`.
    score: f64,
    // Seconds since 1970 of the last open that counted.
    counted_at: u64,
}

impl Record {
    pub fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    // The score decayed to `now`, in seconds since 1970. A `now` before the
    // last count means a wrong clock, so it gets no decay.
    pub fn frecency(&self, now: u64) -> f64 {
        let age = now.saturating_sub(self.counted_at) as f64;
        self.score * 0.5_f64.powf(age / HALF_LIFE as f64)
    }
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    // Counts an open of `path` at `now`: +1 on the decayed score, unless the
    // last counted open was under 12 hours ago. Fails for empty paths and
    // paths that aren't UTF-8, which JSON can't hold.
    pub fn record_open(&mut self, path: &Path, now: u64) -> io::Result<()> {
        let Some(path) = path.to_str() else {
            return Err(invalid_input("path is not valid UTF-8"));
        };
        if path.is_empty() {
            return Err(invalid_input("path is empty"));
        }
        // A plain scan: history holds one record per file the user opened,
        // so it stays small.
        let Some(record) = self.records.iter_mut().find(|r| r.path == path) else {
            self.records.push(Record {
                path: path.to_string(),
                score: 1.0,
                counted_at: now,
            });
            return Ok(());
        };
        if now < record.counted_at.saturating_add(MIN_GAP) {
            return Ok(());
        }
        record.score = record.frecency(now) + 1.0;
        record.counted_at = now;
        Ok(())
    }

    // Writes the history as JSON.
    pub fn write_to(&self, out: impl Write) -> io::Result<()> {
        let mut out = BufWriter::new(out);
        serde_json::to_writer_pretty(&mut out, self)?;
        out.flush()
    }

    // Reads what `write_to` wrote. JSON that doesn't parse, or a record with
    // an empty path or a negative score, is an `InvalidData` error.
    pub fn read_from(input: impl Read) -> io::Result<History> {
        let history: History = serde_json::from_reader(BufReader::new(input))?;
        for r in &history.records {
            if r.path.is_empty() || r.score < 0.0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("bad history record: {r:?}"),
                ));
            }
        }
        Ok(history)
    }
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000 * DAY;

    fn opened(times: &[u64]) -> History {
        let mut history = History::new();
        for &t in times {
            history.record_open(Path::new("/a/b.txt"), t).unwrap();
        }
        history
    }

    fn assert_close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn first_open_scores_one() {
        let history = opened(&[NOW]);
        assert_eq!(history.records().len(), 1);
        assert_close(history.records()[0].frecency(NOW), 1.0);
    }

    #[test]
    fn opens_within_12_hours_count_once() {
        let history = opened(&[NOW, NOW + HOUR, NOW + 11 * HOUR]);
        assert_close(history.records()[0].frecency(NOW), 1.0);
    }

    // The old score decays to the new open's time before the +1.
    #[test]
    fn open_after_12_hours_adds_one() {
        let history = opened(&[NOW, NOW + DAY]);
        let expected = 0.5_f64.powf(1.0 / 7.0) + 1.0;
        assert_close(history.records()[0].frecency(NOW + DAY), expected);
    }

    #[test]
    fn frecency_halves_each_week() {
        let record = &opened(&[NOW]).records[0];
        assert_close(record.frecency(NOW + 7 * DAY), 0.5);
        assert_close(record.frecency(NOW + 14 * DAY), 0.25);
    }

    #[test]
    fn clock_before_last_count_gives_no_decay() {
        let record = &opened(&[NOW]).records[0];
        assert_close(record.frecency(NOW - DAY), 1.0);
    }

    #[test]
    fn different_paths_get_separate_records() {
        let mut history = History::new();
        history.record_open(Path::new("/a.txt"), NOW).unwrap();
        history.record_open(Path::new("/b.txt"), NOW).unwrap();
        assert_eq!(history.records().len(), 2);
        assert_eq!(history.records()[1].path(), Path::new("/b.txt"));
    }

    #[test]
    fn empty_path_is_refused() {
        let mut history = History::new();
        assert!(history.record_open(Path::new(""), NOW).is_err());
        assert!(history.records().is_empty());
    }

    #[test]
    fn write_then_read_gives_the_same_records() {
        let mut history = opened(&[NOW, NOW + 3 * DAY]);
        history
            .record_open(Path::new("/quote\"tab\tline\nbreak.txt"), NOW)
            .unwrap();
        let mut bytes = Vec::new();
        history.write_to(&mut bytes).unwrap();
        let read = History::read_from(bytes.as_slice()).unwrap();
        assert_eq!(read.records(), history.records());
    }

    #[test]
    fn bad_json_is_an_error() {
        // Empty or cut-off input is `UnexpectedEof` instead, from serde_json.
        assert!(History::read_from("".as_bytes()).is_err());
        for json in [
            "[]",
            r#"{"records": [{"path": "/a", "score": "x", "counted_at": 1}]}"#,
            r#"{"records": [{"path": "/a", "score": 1}]}"#,
            r#"{"records": [{"path": "", "score": 1, "counted_at": 1}]}"#,
            r#"{"records": [{"path": "/a", "score": -1, "counted_at": 1}]}"#,
        ] {
            let err = History::read_from(json.as_bytes()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{json}");
        }
    }

    #[test]
    fn reads_hand_written_json() {
        let json = r#"{"records": [{"path": "/a", "score": 2.5, "counted_at": 5}]}"#;
        let read = History::read_from(json.as_bytes()).unwrap();
        assert_eq!(read.records()[0].path(), Path::new("/a"));
        assert_close(read.records()[0].frecency(5), 2.5);
    }
}
