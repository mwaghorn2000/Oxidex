// Files opened through Oxidex, for the frecency signal. See RANKING.md,
// "Frecency". The frontend owns a `History` and core never picks a file
// location. The frontend's part:
//
// 1. At startup, load it with `read_from`, or start with `History::new`.
// 2. After every walk, call `resolve`: entry IDs change with each walk.
// 3. When the user opens a result, call `record_open`, then `resolve`, so
//    a newly opened file gets its boost straight away.
// 4. Save it with `write_to` whenever it likes.
use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::index::{Index, fold_case};

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
    // (index number, entry ID, record number), sorted, filled in by
    // `resolve`. Worked out again after every walk, so it isn't saved.
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

    // Links each record to the entry with its path in `indexes`, for `find`.
    // Search works in entry IDs, and `Index` can't look a path up, so this
    // does one pass per index: a hash lookup rules out almost every name,
    // and only entries named like a record build a full path to compare.
    // Records whose file isn't in any index stay unresolved. They're kept,
    // and resolve again if the file comes back.
    pub fn resolve(&mut self, indexes: &[Arc<Index>]) {
        self.resolved.clear();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (number, record) in self.records.iter().enumerate() {
            let Some(name) = record.path().file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            by_name.entry(fold_case(name)).or_default().push(number);
        }
        for (number, index) in indexes.iter().enumerate() {
            for id in 0..index.len() as u32 {
                let Some(records) = index.name(id).and_then(|n| by_name.get(n)) else {
                    continue;
                };
                let Some(path) = index.full_path(id) else {
                    continue;
                };
                for &r in records {
                    if path == self.records[r].path() {
                        self.resolved.push((number as u32, id, r));
                    }
                }
            }
        }
        self.resolved.sort_unstable();
        // `record_open` never makes two records for one path, but a file
        // edited by hand might. One link per entry, so it's one hit.
        self.resolved
            .dedup_by_key(|&mut (index, id, _)| (index, id));
    }

    // Every resolved entry and its record, as (index number, entry ID,
    // record), for search's history phase.
    pub(crate) fn resolved(&self) -> impl Iterator<Item = (u32, u32, &Record)> {
        self.resolved
            .iter()
            .map(|&(index, id, r)| (index, id, &self.records[r]))
    }

    // The record for entry `id` of index number `index`, as of the last
    // `resolve`.
    pub fn find(&self, index: u32, id: u32) -> Option<&Record> {
        let i = self
            .resolved
            .binary_search_by(|&(x, y, _)| (x, y).cmp(&(index, id)))
            .ok()?;
        self.records.get(self.resolved[i].2)
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
        // A last count in the future means the clock was wrong then (or is
        // now). Left alone, it would block every later open until the real
        // clock caught up, so move it to now. This open doesn't count, as if
        // it fell within the gap.
        if record.counted_at > now {
            record.counted_at = now;
            return Ok(());
        }
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

    // root/a.txt, root/Docs/a.txt and root/Docs/ReadMe.md, with root at
    // `/r`. Returns the index and the IDs of the three files.
    fn tree() -> (Arc<Index>, [u32; 3]) {
        let mut index = Index::new();
        let root = index.push_root("/r", true, 0, 0).unwrap();
        let a = index.push("a.txt", root, false, 0, 0).unwrap();
        let docs = index.push("Docs", root, true, 0, 0).unwrap();
        let docs_a = index.push("a.txt", docs, false, 0, 0).unwrap();
        let readme = index.push("ReadMe.md", docs, false, 0, 0).unwrap();
        (Arc::new(index), [a, docs_a, readme])
    }

    fn history_of(paths: &[&str]) -> History {
        let mut history = History::new();
        for path in paths {
            history.record_open(Path::new(path), NOW).unwrap();
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

    // A record counted with a clock a year fast must not freeze: later opens
    // at the real time count again.
    #[test]
    fn open_after_a_future_count_unfreezes_the_record() {
        let mut times = vec![NOW + 365 * DAY];
        times.extend((0..30).map(|day| NOW + day * DAY));
        let history = opened(&times);
        assert!(history.records()[0].frecency(NOW + 29 * DAY) > 3.0);
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

    #[test]
    fn resolve_finds_the_entry_with_the_same_path() {
        let (index, [a, docs_a, _]) = tree();
        let mut history = history_of(&["/r/Docs/a.txt"]);
        history.resolve(&[index]);
        let record = history.find(0, docs_a).unwrap();
        assert_eq!(record.path(), Path::new("/r/Docs/a.txt"));
        // Same name, different folder.
        assert!(history.find(0, a).is_none());
    }

    // Index names are lowercased, but the comparison uses the real path.
    #[test]
    fn resolve_matches_names_with_capitals() {
        let (index, [_, _, readme]) = tree();
        let mut history = history_of(&["/r/Docs/ReadMe.md"]);
        history.resolve(&[index]);
        assert!(history.find(0, readme).is_some());
    }

    #[test]
    fn missing_file_stays_unresolved_but_kept() {
        let (index, ids) = tree();
        let mut history = history_of(&["/r/gone.txt"]);
        history.resolve(&[index]);
        assert!(ids.iter().all(|&id| history.find(0, id).is_none()));
        assert_eq!(history.records().len(), 1);
    }

    #[test]
    fn resolve_uses_each_index_number() {
        let mut other = Index::new();
        let root = other.push_root("/s", true, 0, 0).unwrap();
        // Gives "b.txt" a different ID from "a.txt" in the first index.
        other.push("x.txt", root, false, 0, 0).unwrap();
        let b = other.push("b.txt", root, false, 0, 0).unwrap();
        let (index, [a, ..]) = tree();
        let mut history = history_of(&["/s/b.txt", "/r/a.txt"]);
        history.resolve(&[index, Arc::new(other)]);
        assert_eq!(history.find(0, a).unwrap().path(), Path::new("/r/a.txt"));
        assert_eq!(history.find(1, b).unwrap().path(), Path::new("/s/b.txt"));
        assert!(history.find(1, a).is_none());
    }

    // A new walk gives new IDs, so old links must not survive it.
    #[test]
    fn resolve_replaces_old_links() {
        let (index, [a, ..]) = tree();
        let mut history = history_of(&["/r/a.txt"]);
        history.resolve(&[index]);
        history.resolve(&[]);
        assert!(history.find(0, a).is_none());
    }

    #[test]
    fn duplicate_records_resolve_once() {
        let (index, [a, ..]) = tree();
        let json = r#"{"records": [
            {"path": "/r/a.txt", "score": 1, "counted_at": 0},
            {"path": "/r/a.txt", "score": 2, "counted_at": 0}
        ]}"#;
        let mut history = History::read_from(json.as_bytes()).unwrap();
        history.resolve(&[index]);
        assert_eq!(history.resolved().count(), 1);
        assert!(history.find(0, a).is_some());
    }

    #[test]
    fn find_before_resolve_finds_nothing() {
        let history = history_of(&["/r/a.txt"]);
        assert!(history.find(0, 1).is_none());
    }
}
