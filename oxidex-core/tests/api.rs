// Uses only the public API, the way a frontend would, so a change that
// breaks downstream code fails here.
use std::error::Error;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use oxidex::index::{NO_PARENT, NameTooLong};
use oxidex::{Cancel, Cancelled, History, Hit, Index, Sources, default_scorers, search, walk};

// root/a.txt and root/docs/a.txt: equal matches for "a.txt", where the
// shallower one wins the tiebreak until the deeper one is opened.
fn walked() -> (PathBuf, Vec<Arc<Index>>) {
    let dir = std::env::temp_dir().join(format!("oxidex-api-{}", std::process::id()));
    fs::create_dir_all(dir.join("docs")).unwrap();
    fs::write(dir.join("a.txt"), "").unwrap();
    fs::write(dir.join("docs").join("a.txt"), "").unwrap();
    let index = walk(&dir, NonZeroUsize::MIN).unwrap().index;
    (dir.canonicalize().unwrap(), vec![Arc::new(index)])
}

fn top_path(indexes: &[Arc<Index>], history: &History, query: &str) -> PathBuf {
    let sources = Sources { indexes, history };
    let mut hits: Vec<Hit> = Vec::new();
    let scorers = default_scorers();
    search(
        &sources,
        query,
        10,
        &scorers,
        0,
        &Cancel::never(),
        &mut hits,
    )
    .unwrap();
    indexes[hits[0].index as usize]
        .full_path(hits[0].id)
        .unwrap()
}

#[test]
fn walk_search_open_and_save() {
    let (dir, indexes) = walked();
    let mut history = History::new();
    assert_eq!(top_path(&indexes, &history, "a.txt"), dir.join("a.txt"));

    let deep = dir.join("docs").join("a.txt");
    history.record_open(&deep, 0).unwrap();
    history.resolve(&indexes);
    assert_eq!(top_path(&indexes, &history, "a.txt"), deep);

    let mut saved = Vec::new();
    history.write_to(&mut saved).unwrap();
    let read = History::read_from(saved.as_slice()).unwrap();
    assert_eq!(read.records(), history.records());
    fs::remove_dir_all(&dir).unwrap();
}

// Pushes `name`, then searches with a cancel that has already fired, so
// it fails with `NameTooLong` for a long name and `Cancelled` otherwise.
// Both must work with `?` into `Box<dyn Error>`.
fn push_and_search(name: &str) -> Result<(), Box<dyn Error>> {
    let mut index = Index::new();
    index.push(name, NO_PARENT, false, 0, 0)?;
    let indexes = [Arc::new(index)];
    let history = History::new();
    let sources = Sources {
        indexes: &indexes,
        history: &history,
    };
    let latest = AtomicU64::new(1);
    let mut hits = Vec::new();
    search(
        &sources,
        "x",
        10,
        &default_scorers(),
        0,
        &Cancel::new(&latest, 0),
        &mut hits,
    )?;
    Ok(())
}

#[test]
fn errors_work_with_question_mark() {
    let long = "x".repeat(usize::from(u16::MAX) + 1);
    let err = push_and_search(&long).unwrap_err();
    assert!(err.downcast_ref::<NameTooLong>().is_some(), "{err}");
    let err = push_and_search("x.txt").unwrap_err();
    assert!(err.downcast_ref::<Cancelled>().is_some(), "{err}");
}
