// Filename search over a list of indexes, one per search directory, ranked
// together. See RANKING.md for the full plan.
//
// Results are ranked in three layers, each with its own place to extend:
//
// 1. Tier (`Tier`): what kind of match it is. A new kind of match is a new
//    `Tier` variant plus a check in `match_name`, both in matching.rs, and
//    `Query::may_match` must still let its names through.
// 2. Score (`Hit::score`): how good the match is within its tier. It's the
//    total of the `Scorer`s passed to `search`: match quality from
//    `match_name`, plus signals like modified time. A new signal is a new
//    `Scorer` in score.rs, added to `default_scorers`. Don't put signals in
//    `match_name` or `compare`.
// 3. Tiebreaks (`compare`): only for hits with equal tier and score, so the
//    order never shuffles between searches. Not a place for ranking signals.
use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{self, AtomicU64};

use crate::history::{History, Record};
use crate::index::Index;
pub use crate::matching::Tier;
use crate::matching::{Query, Scratch, folders_match, match_history, match_name};
use crate::score::{Candidate, Scorer, total};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    // Position of the hit's index in the slice passed to `search`.
    pub index: u32,
    // Entry ID within that index.
    pub id: u32,
    pub tier: Tier,
    // How good the hit is within its tier: the total of every scorer's
    // points. Higher is better.
    pub score: i32,
}

// What search looks through. `history` must have been resolved against
// exactly these `indexes` (see `History::resolve`): its links are entry IDs,
// which only mean something for the walk that made them.
pub struct Sources<'a> {
    pub indexes: &'a [Arc<Index>],
    pub history: &'a History,
}

// Lets a newer query stop an older search. The UI bumps `latest` on every
// keystroke and starts a search for the new generation; any search still
// running for an older generation sees the change and stops.
pub struct Cancel<'a> {
    latest: &'a AtomicU64,
    generation: u64,
}

// Returned by `search` when it stopped because a newer query started.
#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

// Never bumped, so `Cancel::never` is never cancelled.
static NEVER: AtomicU64 = AtomicU64::new(0);

impl<'a> Cancel<'a> {
    pub fn new(latest: &'a AtomicU64, generation: u64) -> Cancel<'a> {
        Cancel { latest, generation }
    }

    // For searches nothing will cancel: tests, benchmarks, one-off searches.
    pub fn never() -> Cancel<'static> {
        Cancel {
            latest: &NEVER,
            generation: 0,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        // Relaxed is enough: the counter is only a stop flag and no other data
        // is passed through it. Seeing the bump a little late just means a
        // few more entries get scanned.
        self.latest.load(atomic::Ordering::Relaxed) != self.generation
    }
}

// How many entries to scan between cancellation checks. Rare enough that the
// check costs nothing measurable; often enough that a stale search stops
// within microseconds.
const CANCEL_CHECK_EVERY: u32 = 4096;

// Fills `out` with up to `limit` hits for `query` from all of `sources`,
// best first. Matching is case-insensitive. Hits within a tier are ranked by
// `scorers` (usually `score::default_scorers()`), with `now` in seconds since
// 1970. If `cancel` fires, stops early, clears `out` so a partial list is
// never shown, and returns `Cancelled`.
//
// Path mode: if `query` contains `/` or `\`, the part after the last one is
// matched against names as usual, and each part before it must match a
// folder above the hit (see `folders_match`).
pub fn search(
    sources: &Sources,
    query: &str,
    limit: usize,
    scorers: &[Box<dyn Scorer>],
    now: u64,
    cancel: &Cancel,
    out: &mut Vec<Hit>,
) -> Result<(), Cancelled> {
    out.clear();
    if query.is_empty() || limit == 0 {
        return Ok(());
    }
    let query = Query::new(query);
    if query.text.is_empty() {
        return Ok(());
    }
    let Sources { indexes, history } = *sources;
    let mut scratch = Scratch::default();

    // The hit for entry `id` of index `number`, if its name matches. Both
    // phases below use it, so history hits are matched and scored exactly
    // like the rest. A closure because a function would need every argument
    // of `search` again. Callers pass `index` in rather than it being looked
    // up here: going through the `Arc` again for every matching name made
    // one-letter queries about 5% slower.
    let mut hit_for = |number: u32, index: &Index, id, record: Option<&Record>| -> Option<Hit> {
        let name = index.name(id)?;
        let (tier, quality) = match record {
            Some(record) => match_history(name, &query, record.frecency(now), &mut scratch)?,
            None => match_name(name, &query, &mut scratch)?,
        };
        // Checked after the name matches, so path mode costs nothing for
        // the names that don't.
        if !query.folders.is_empty() && !folders_match(index, id, &query.folders) {
            return None;
        }
        // Scored before the hit competes for a place in `out`. New signals
        // go in score.rs: see the top of this file.
        let candidate = Candidate {
            index,
            id,
            tier,
            quality,
            history: record,
            now,
        };
        Some(Hit {
            index: number,
            id,
            tier,
            score: total(scorers, &candidate),
        })
    };

    // Keep only the best `limit` hits instead of collecting and sorting every
    // match: a one-letter query matches most of the index. `worst` is the
    // position of the worst kept hit, once `out` is full. It's shared across
    // indexes, so they compete for the same `limit` places.
    let mut worst = None;

    // Phase 1, history (RANKING.md, "Streaming"): the few files the user has
    // opened, matched with `match_history`. They're small in number, so no
    // cancellation check is needed.
    for (number, id, record) in history.resolved() {
        let Some(index) = indexes.get(number as usize) else {
            continue;
        };
        let Some(&mask) = index.masks().get(id as usize) else {
            continue;
        };
        if !query.may_match_history(mask) {
            continue;
        }
        if let Some(hit) = hit_for(number, index, id, Some(record)) {
            keep(indexes, limit, out, &mut worst, hit);
        }
    }

    // Phase 2, every entry of every index. `opened` walks phase 1's entries
    // in the order this loop reaches them (`resolved` is sorted), so skipping
    // them costs one comparison per entry rather than a lookup.
    let mut opened = history.resolved().map(|(i, id, _)| (i, id)).peekable();
    for (number, index) in indexes.iter().enumerate() {
        let number = number as u32;
        for (id, &mask) in index.masks().iter().enumerate() {
            let id = id as u32;
            // Checked at entry 0 too, so an already-stale search does no work.
            if id.is_multiple_of(CANCEL_CHECK_EVERY) && cancel.is_cancelled() {
                out.clear();
                return Err(Cancelled);
            }
            // Rules out most names without reading them.
            if !query.may_match(mask) {
                continue;
            }
            // Already scored in phase 1.
            while opened.next_if(|&e| e < (number, id)).is_some() {}
            if opened.peek() == Some(&(number, id)) {
                continue;
            }
            if let Some(hit) = hit_for(number, index, id, None) {
                keep(indexes, limit, out, &mut worst, hit);
            }
        }
    }
    out.sort_by(|a, b| compare(indexes, a, b));
    Ok(())
}

// Adds `hit` to `out` if there's room or it beats the worst kept hit.
fn keep(
    indexes: &[Arc<Index>],
    limit: usize,
    out: &mut Vec<Hit>,
    worst: &mut Option<usize>,
    hit: Hit,
) {
    if out.len() < limit {
        out.push(hit);
        if out.len() == limit {
            *worst = Some(find_worst(indexes, out));
        }
    } else if let Some(w) = *worst
        && compare(indexes, &hit, &out[w]) == Ordering::Less
    {
        out[w] = hit;
        *worst = Some(find_worst(indexes, out));
    }
}

// The full result order. Best first: tier, then score, then shorter name,
// then shallower path, then earlier index, then earlier entry. Name length
// and depth are only looked up on a tie, so the common case stays cheap.
// Ranking signals belong in `Hit::score`, not here: see the top of this file.
fn compare(indexes: &[Arc<Index>], a: &Hit, b: &Hit) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then(b.score.cmp(&a.score))
        .then_with(|| name_len(indexes, a).cmp(&name_len(indexes, b)))
        .then_with(|| depth(indexes, a).cmp(&depth(indexes, b)))
        .then(a.index.cmp(&b.index))
        .then(a.id.cmp(&b.id))
}

// A missing entry sorts last rather than panicking.
fn name_len(indexes: &[Arc<Index>], hit: &Hit) -> usize {
    let name = indexes.get(hit.index as usize).and_then(|i| i.name(hit.id));
    name.map_or(usize::MAX, str::len)
}

// An entry whose parents form a cycle has no depth, so it sorts last.
fn depth(indexes: &[Arc<Index>], hit: &Hit) -> u32 {
    let depth = indexes
        .get(hit.index as usize)
        .and_then(|i| i.depth(hit.id));
    depth.unwrap_or(u32::MAX)
}

// Position of the worst hit in `out`. A linear scan is fine: `out` holds
// only `limit` hits, and this only runs when a better hit replaces one.
fn find_worst(indexes: &[Arc<Index>], out: &[Hit]) -> usize {
    let mut worst = 0;
    for i in 1..out.len() {
        if compare(indexes, &out[i], &out[worst]) == Ordering::Greater {
            worst = i;
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::{Quality, default_scorers};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicU64;

    // A root folder with each name as a direct child file.
    fn index_of(names: &[&str]) -> Arc<Index> {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        for name in names {
            index.push(name, root, false, 0, 0).unwrap();
        }
        Arc::new(index)
    }

    // Hits for `query` with the default scorers, as the frontend would get
    // them. The one place tests call `search`, apart from tests of its other
    // parameters (scorers, cancellation).
    fn run(indexes: &[Arc<Index>], query: &str, limit: usize, now: u64) -> Vec<Hit> {
        run_with(indexes, &History::new(), query, limit, now)
    }

    fn run_with(
        indexes: &[Arc<Index>],
        history: &History,
        query: &str,
        limit: usize,
        now: u64,
    ) -> Vec<Hit> {
        let mut out = Vec::new();
        search(
            &Sources { indexes, history },
            query,
            limit,
            &default_scorers(),
            now,
            &Cancel::never(),
            &mut out,
        )
        .unwrap();
        out
    }

    // Display name and tier of each hit, in result order.
    fn results(index: &Arc<Index>, query: &str, limit: usize) -> Vec<(String, Tier)> {
        let out = run(std::slice::from_ref(index), query, limit, 0);
        out.iter()
            .map(|hit| (index.display_name(hit.id).unwrap().to_string(), hit.tier))
            .collect()
    }

    fn tier_of(names: &[&str], query: &str) -> Option<Tier> {
        let index = index_of(names);
        let hits = results(&index, query, 10);
        hits.into_iter()
            .find(|(n, _)| n == names[0])
            .map(|(_, t)| t)
    }

    #[test]
    fn empty_query_returns_nothing() {
        let index = index_of(&["readme.md"]);
        assert!(results(&index, "", 10).is_empty());
    }

    #[test]
    fn no_match_returns_nothing() {
        let index = index_of(&["readme.md"]);
        assert!(results(&index, "xyz", 10).is_empty());
    }

    #[test]
    fn query_is_case_insensitive() {
        assert_eq!(tier_of(&["ReadMe.md"], "README"), Some(Tier::Exact));
    }

    #[test]
    fn folder_entries_match_by_name() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        index.push("docs", root, true, 0, 0).unwrap();
        let hits = results(&Arc::new(index), "docs", 10);
        assert_eq!(hits, vec![("docs".to_string(), Tier::Exact)]);
    }

    #[test]
    fn results_are_ordered_by_tier() {
        let index = index_of(&["xrxexaxd", "readme", "read"]);
        let tiers: Vec<Tier> = results(&index, "read", 10).iter().map(|h| h.1).collect();
        assert_eq!(tiers, vec![Tier::Exact, Tier::Prefix, Tier::Subsequence]);
    }

    // The exact match is pushed last, so a scan that stops at `limit` would
    // miss it.
    #[test]
    fn limit_keeps_best_tiers() {
        let index = index_of(&["r_e_a_d_1", "r_e_a_d_2", "reader", "read"]);
        let hits = results(&index, "read", 2);
        assert_eq!(
            hits,
            vec![
                ("read".to_string(), Tier::Exact),
                ("reader".to_string(), Tier::Prefix),
            ]
        );
    }

    #[test]
    fn non_ascii_query_matches() {
        assert_eq!(tier_of(&["Café Menu.pdf"], "CAFÉ"), Some(Tier::Prefix));
    }

    #[test]
    fn one_letter_query_matches_exact_and_prefix_only() {
        let index = index_of(&["banana", "apple", "a.txt"]);
        assert_eq!(names(&index, "a", 10), ["a.txt", "apple"]);
    }

    #[test]
    fn two_letter_query_matches_exact_and_prefix_only() {
        let index = index_of(&["xaxb", "abc", "ab"]);
        assert_eq!(names(&index, "ab", 10), ["ab", "abc"]);
    }

    fn names(index: &Arc<Index>, query: &str, limit: usize) -> Vec<String> {
        results(index, query, limit)
            .into_iter()
            .map(|h| h.0)
            .collect()
    }

    #[test]
    fn ties_go_to_shorter_name() {
        let index = index_of(&["readme.txt", "readme.md"]);
        assert_eq!(names(&index, "readme", 10), ["readme.md", "readme.txt"]);
    }

    #[test]
    fn ties_go_to_shallower_path() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        let docs = index.push("docs", root, true, 0, 0).unwrap();
        let deep = index.push("a.txt", docs, false, 0, 0).unwrap();
        let shallow = index.push("a.txt", root, false, 0, 0).unwrap();
        let out = run(&[Arc::new(index)], "a.txt", 10, 0);
        let ids: Vec<u32> = out.iter().map(|h| h.id).collect();
        assert_eq!(ids, [shallow, deep]);
    }

    #[test]
    fn ties_go_to_earlier_entry() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        let first = index.push("a.txt", root, false, 0, 0).unwrap();
        let second = index.push("a.txt", root, false, 0, 0).unwrap();
        let out = run(&[Arc::new(index)], "a.txt", 10, 0);
        let ids: Vec<u32> = out.iter().map(|h| h.id).collect();
        assert_eq!(ids, [first, second]);
    }

    // A full set of exact matches must not stop the scan: a later exact match
    // can still score higher.
    #[test]
    fn limit_keeps_best_quality() {
        let index = index_of(&["readme.txt", "readme"]);
        assert_eq!(names(&index, "readme", 1), ["readme"]);
    }

    // Index number and display name of each hit, in result order.
    fn hits_across(indexes: &[Arc<Index>], query: &str, limit: usize) -> Vec<(u32, String)> {
        let out = run(indexes, query, limit, 0);
        out.iter()
            .map(|hit| {
                let name = indexes[hit.index as usize].display_name(hit.id).unwrap();
                (hit.index, name.to_string())
            })
            .collect()
    }

    #[test]
    fn no_indexes_returns_nothing() {
        assert!(hits_across(&[], "readme", 10).is_empty());
    }

    #[test]
    fn searches_every_index() {
        let indexes = [index_of(&["a.txt"]), index_of(&["b.txt"])];
        assert_eq!(
            hits_across(&indexes, "txt", 10),
            [(0, "a.txt".to_string()), (1, "b.txt".to_string())]
        );
    }

    // The better match is in the later index, so ranking must happen across
    // indexes, not one index after another.
    #[test]
    fn indexes_are_ranked_together() {
        let indexes = [index_of(&["xrxexaxd"]), index_of(&["read"])];
        assert_eq!(
            hits_across(&indexes, "read", 10),
            [(1, "read".to_string()), (0, "xrxexaxd".to_string())]
        );
    }

    #[test]
    fn limit_applies_across_indexes() {
        let indexes = [index_of(&["readme_old_backup"]), index_of(&["readme"])];
        assert_eq!(
            hits_across(&indexes, "readme", 1),
            [(1, "readme".to_string())]
        );
    }

    // Both entries have the same ID in their own index, so the tiebreak must
    // use the index number and not just the ID.
    #[test]
    fn ties_go_to_earlier_index() {
        let indexes = [index_of(&["a.txt"]), index_of(&["a.txt"])];
        assert_eq!(
            hits_across(&indexes, "a.txt", 10),
            [(0, "a.txt".to_string()), (1, "a.txt".to_string())]
        );
    }

    #[test]
    fn typos_rank_between_prefix_and_subsequence() {
        let index = index_of(&["s_l_a_c_k", "slick", "slack_notes"]);
        let hits = results(&index, "slack", 10);
        assert_eq!(
            hits,
            [
                ("slack_notes".to_string(), Tier::Prefix),
                ("slick".to_string(), Tier::Levenshtein),
                ("s_l_a_c_k".to_string(), Tier::Subsequence),
            ]
        );
    }

    // Scattered subsequence matches fill `limit`, but the typo match must
    // still be found and shown first.
    #[test]
    fn typo_beats_a_full_page_of_subsequence_matches() {
        let index = index_of(&["xrxaxexdxmxex1", "xrxaxexdxmxex2", "readme.md"]);
        assert_eq!(names(&index, "raedme", 2), ["readme.md", "xrxaxexdxmxex1"]);
    }

    // The exact match is also 0 edits from the query; it must appear once.
    #[test]
    fn earlier_tier_match_is_not_repeated() {
        let index = index_of(&["slack.exe", "slick"]);
        assert_eq!(names(&index, "slack", 10), ["slack.exe", "slick"]);
    }

    // root/
    //   docs/readme.md, docs/old/notes.txt
    //   src/readme.md, src/docs/
    fn tree() -> Arc<Index> {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        let docs = index.push("docs", root, true, 0, 0).unwrap();
        index.push("readme.md", docs, false, 0, 0).unwrap();
        let old = index.push("old", docs, true, 0, 0).unwrap();
        index.push("notes.txt", old, false, 0, 0).unwrap();
        let src = index.push("src", root, true, 0, 0).unwrap();
        index.push("readme.md", src, false, 0, 0).unwrap();
        index.push("docs", src, true, 0, 0).unwrap();
        Arc::new(index)
    }

    // Full path of each hit, in result order.
    fn paths(index: Arc<Index>, query: &str) -> Vec<String> {
        let indexes = [index];
        let out = run(&indexes, query, 10, 0);
        out.iter()
            .map(|h| {
                indexes[0]
                    .full_path(h.id)
                    .unwrap()
                    .display()
                    .to_string()
                    .replace('\\', "/")
            })
            .collect()
    }

    #[test]
    fn path_query_matches_file_under_folder() {
        assert_eq!(paths(tree(), "docs/readme"), ["root/docs/readme.md"]);
    }

    #[test]
    fn backslash_works_like_slash() {
        assert_eq!(paths(tree(), "docs\\readme"), ["root/docs/readme.md"]);
    }

    #[test]
    fn folder_part_is_subsequence() {
        assert_eq!(paths(tree(), "dcs/readme"), ["root/docs/readme.md"]);
    }

    #[test]
    fn folder_need_not_be_direct_parent() {
        assert_eq!(paths(tree(), "docs/notes"), ["root/docs/old/notes.txt"]);
    }

    #[test]
    fn folder_parts_match_in_order() {
        assert_eq!(paths(tree(), "docs/old/notes"), ["root/docs/old/notes.txt"]);
        assert!(paths(tree(), "old/docs/notes").is_empty());
    }

    // The match's own name doesn't count as one of its folders.
    #[test]
    fn file_name_is_not_its_own_folder() {
        assert!(paths(tree(), "readme/readme").is_empty());
    }

    #[test]
    fn folders_can_match_the_file_part() {
        assert_eq!(paths(tree(), "src/docs"), ["root/src/docs"]);
    }

    // The tier comes from the file part alone, as if there were no folders.
    #[test]
    fn path_hit_is_ranked_by_file_part() {
        let indexes = [tree()];
        let out = run(&indexes, "docs/readme", 10, 0);
        assert_eq!(out[0].tier, Tier::Exact);
        assert_eq!(out[0].score, 900);
    }

    #[test]
    fn empty_folder_parts_are_ignored() {
        assert_eq!(paths(tree(), "/notes"), ["root/docs/old/notes.txt"]);
        assert_eq!(paths(tree(), "docs//notes"), ["root/docs/old/notes.txt"]);
    }

    #[test]
    fn trailing_slash_returns_nothing() {
        assert!(paths(tree(), "docs/").is_empty());
    }

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_000 * DAY;

    // Two entries with the same name, depth and match: only the modified
    // time tells them apart. The older is pushed first, so without the
    // modified scorer the tiebreak would put it first.
    #[test]
    fn newer_file_ranks_higher_on_equal_match() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        let old = index
            .push("a.txt", root, false, 0, NOW - 365 * DAY)
            .unwrap();
        let new = index.push("a.txt", root, false, 0, NOW).unwrap();
        let out = run(&[Arc::new(index)], "a.txt", 10, NOW);
        let ids: Vec<u32> = out.iter().map(|h| h.id).collect();
        assert_eq!(ids, [new, old]);
    }

    // The modified boost only adjusts within a tier's quality range: a much
    // better prefix match still beats a brand new one.
    #[test]
    fn better_match_beats_newer_file() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        index
            .push("readme", root, false, 0, NOW - 365 * DAY)
            .unwrap();
        index
            .push("readme_old_backup", root, false, 0, NOW)
            .unwrap();
        let out = run(&[Arc::new(index)], "read", 10, NOW);
        assert_eq!(out[0].id, 1);
    }

    // The score comes from the scorers passed in, not a fixed formula.
    #[test]
    fn hit_score_is_total_of_scorers() {
        let index = index_of(&["a.txt"]);
        let scorers: Vec<Box<dyn Scorer>> = vec![Box::new(Quality { weight: 2 })];
        let history = History::new();
        let mut out = Vec::new();
        search(
            &Sources {
                indexes: &[index],
                history: &history,
            },
            "a.txt",
            10,
            &scorers,
            NOW,
            &Cancel::never(),
            &mut out,
        )
        .unwrap();
        assert_eq!(out[0].score, 2000);
    }

    #[test]
    fn cancel_follows_latest_generation() {
        let latest = AtomicU64::new(3);
        let cancel = Cancel::new(&latest, 3);
        assert!(!cancel.is_cancelled());
        latest.store(4, std::sync::atomic::Ordering::Relaxed);
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn never_is_never_cancelled() {
        assert!(!Cancel::never().is_cancelled());
    }

    #[test]
    fn search_that_is_not_cancelled_returns_ok() {
        let latest = AtomicU64::new(1);
        let mut out = Vec::new();
        let history = History::new();
        let result = search(
            &Sources {
                indexes: &[index_of(&["a.txt"])],
                history: &history,
            },
            "a",
            10,
            &default_scorers(),
            0,
            &Cancel::new(&latest, 1),
            &mut out,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(out.len(), 1);
    }

    // A newer query has already started, so this search must stop before
    // returning any hits, even ones that match.
    #[test]
    fn stale_search_is_cancelled_and_leaves_out_empty() {
        let latest = AtomicU64::new(2);
        let mut out = vec![Hit {
            index: 9,
            id: 9,
            tier: Tier::Exact,
            score: 0,
        }];
        let history = History::new();
        let result = search(
            &Sources {
                indexes: &[index_of(&["a.txt"])],
                history: &history,
            },
            "a",
            10,
            &default_scorers(),
            0,
            &Cancel::new(&latest, 1),
            &mut out,
        );
        assert_eq!(result, Err(Cancelled));
        assert!(out.is_empty());
    }

    // The root shows its full path, but must still be found by its own name.
    #[test]
    fn root_is_exact_match_for_its_last_part() {
        let mut index = Index::new();
        let root = index.push_root("/home/me/myproj", true, 0, 0).unwrap();
        index.push("myproj.txt", root, false, 0, 0).unwrap();
        let hits = results(&Arc::new(index), "myproj", 10);
        assert_eq!(hits[0], ("/home/me/myproj".to_string(), Tier::Exact));
    }

    // A query ending in 'Σ' lowercases to final 'ς', but the same letter in
    // the middle of a name lowercases to 'σ'.
    #[test]
    fn final_sigma_query_matches_exact_and_prefix() {
        let index = index_of(&["ΟΔΟΣ.txt", "ΟΔΟΣΤΡΩΜΑ.txt"]);
        let hits = results(&index, "ΟΔΟΣ", 10);
        assert_eq!(
            hits,
            [
                ("ΟΔΟΣ.txt".to_string(), Tier::Exact),
                ("ΟΔΟΣΤΡΩΜΑ.txt".to_string(), Tier::Prefix),
            ]
        );
    }

    // A history with `paths` opened once each, resolved against `indexes`.
    fn history_of(indexes: &[Arc<Index>], paths: &[&str]) -> History {
        let mut history = History::new();
        for path in paths {
            history.record_open(Path::new(path), 0).unwrap();
        }
        history.resolve(indexes);
        history
    }

    // Phase 1 finds it and phase 2 must skip it.
    #[test]
    fn history_hit_appears_once() {
        let indexes = [index_of(&["a.txt", "notes.md"])];
        let history = history_of(&indexes, &["root/a.txt"]);
        let hits = run_with(&indexes, &history, "a.txt", 10, 0);
        let ids: Vec<u32> = hits.iter().map(|h| h.id).collect();
        assert_eq!(ids, [1]);
    }

    // Files opened once each are well below the tier-jump score, and these
    // queries aren't typos of a name being typed, so history only changes
    // scores here: the same hits, in the same tiers.
    #[test]
    fn history_keeps_hits_and_tiers() {
        let indexes = [tree()];
        let opened = [
            "root/docs",
            "root/docs/readme.md",
            "root/docs/old/notes.txt",
            "root/src/readme.md",
            "root/src/docs",
        ];
        let history = history_of(&indexes, &opened);
        assert_eq!(history.resolved().count(), opened.len());
        let tiers = |hits: Vec<Hit>| {
            let mut tiers: Vec<(u32, Tier)> = hits.iter().map(|h| (h.id, h.tier)).collect();
            tiers.sort();
            tiers
        };
        for query in ["readme", "docs", "rdm", "raedme", "docs/readme", "r"] {
            let with = run_with(&indexes, &history, query, 10, 0);
            assert_eq!(tiers(with), tiers(run(&indexes, query, 10, 0)), "{query}");
        }
    }

    // Both are exact matches. Without history the shallower one would win
    // the tiebreak.
    #[test]
    fn opened_file_ranks_higher_on_equal_match() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        let docs = index.push("docs", root, true, 0, 0).unwrap();
        let deep = index.push("a.txt", docs, false, 0, 0).unwrap();
        let shallow = index.push("a.txt", root, false, 0, 0).unwrap();
        let indexes = [Arc::new(index)];
        let ids = |hits: Vec<Hit>| hits.iter().map(|h| h.id).collect::<Vec<u32>>();
        assert_eq!(ids(run(&indexes, "a.txt", 10, 0)), [shallow, deep]);
        let history = history_of(&indexes, &["root/docs/a.txt"]);
        assert_eq!(
            ids(run_with(&indexes, &history, "a.txt", 10, 0)),
            [deep, shallow]
        );
    }

    #[test]
    fn history_hits_obey_path_mode() {
        let indexes = [tree()];
        let history = history_of(&indexes, &["root/docs/readme.md", "root/src/readme.md"]);
        let hits = run_with(&indexes, &history, "docs/readme", 10, 0);
        let paths: Vec<_> = hits
            .iter()
            .map(|h| indexes[0].full_path(h.id).unwrap())
            .collect();
        assert_eq!(paths, [Path::new("root").join("docs").join("readme.md")]);
    }

    // Resolved as index 1, which doesn't exist here: no hit, and no panic.
    #[test]
    fn history_resolved_against_other_indexes_is_ignored() {
        let other = [index_of(&["x.txt"]), index_of(&["a.txt"])];
        let history = history_of(&other, &["root/a.txt"]);
        let indexes = [index_of(&["notes.md"])];
        assert!(run_with(&indexes, &history, "a.txt", 10, 0).is_empty());
    }

    // `path` opened with history score `score` as of time 0, resolved
    // against `indexes`.
    fn history_scored(indexes: &[Arc<Index>], path: &str, score: f64) -> History {
        let json =
            format!(r#"{{"records": [{{"path": "{path}", "score": {score}, "counted_at": 0}}]}}"#);
        let mut history = History::read_from(json.as_bytes()).unwrap();
        history.resolve(indexes);
        history
    }

    fn named(indexes: &[Arc<Index>], hits: &[Hit]) -> Vec<(String, Tier)> {
        hits.iter()
            .map(|h| (indexes[0].display_name(h.id).unwrap().to_string(), h.tier))
            .collect()
    }

    // RANKING.md's example: a typo of a daily-opened app beats an exact
    // match nobody opens.
    #[test]
    fn daily_file_jumps_above_exact_match() {
        let indexes = [index_of(&["slark.txt", "slack.exe"])];
        let before = named(&indexes, &run(&indexes, "slark", 10, 0));
        assert_eq!(
            before,
            [
                ("slark.txt".to_string(), Tier::Exact),
                ("slack.exe".to_string(), Tier::Levenshtein),
            ]
        );
        let history = history_scored(&indexes, "root/slack.exe", 5.0);
        let after = named(&indexes, &run_with(&indexes, &history, "slark", 10, 0));
        assert_eq!(
            after,
            [
                ("slack.exe".to_string(), Tier::Exact),
                ("slark.txt".to_string(), Tier::Exact),
            ]
        );
    }

    // Hits for a pasted path, against a root that has folders above it.
    fn pasted(query: &str) -> Vec<PathBuf> {
        let mut index = Index::new();
        let root = index.push_root("/home/Me/Projects", true, 0, 0).unwrap();
        let src = index.push("src", root, true, 0, 0).unwrap();
        index.push("search.rs", src, false, 0, 0).unwrap();
        let indexes = [Arc::new(index)];
        run(&indexes, query, 10, 0)
            .iter()
            .map(|h| indexes[0].full_path(h.id).unwrap())
            .collect()
    }

    #[test]
    fn pasted_full_path_matches_folders_above_the_root() {
        let file = Path::new("/home/Me/Projects").join("src").join("search.rs");
        for query in [
            "/home/me/projects/src/search.rs",
            "me/projects/src/search.rs",
            "home/src/search.rs",
            "./src/search.rs",
            "~/projects/src/search.rs",
        ] {
            assert_eq!(pasted(query), std::slice::from_ref(&file), "{query}");
        }
    }

    #[test]
    fn folders_above_the_root_must_still_match() {
        assert!(pasted("/home/other/projects/src/search.rs").is_empty());
        assert!(pasted("projects/home/src/search.rs").is_empty());
    }

    #[test]
    fn rarely_opened_file_does_not_jump() {
        let indexes = [index_of(&["slark.txt", "slack.exe"])];
        let history = history_scored(&indexes, "root/slack.exe", 1.0);
        let hits = named(&indexes, &run_with(&indexes, &history, "slark", 10, 0));
        assert_eq!(hits[0], ("slark.txt".to_string(), Tier::Exact));
    }

    // Unopened names get no typos while typing; opened ones do.
    #[test]
    fn only_opened_files_match_typos_while_typing() {
        let indexes = [index_of(&["slack.exe", "slick.txt"])];
        assert!(run(&indexes, "slar", 10, 0).is_empty());
        let history = history_scored(&indexes, "root/slack.exe", 1.0);
        let hits = named(&indexes, &run_with(&indexes, &history, "slar", 10, 0));
        assert_eq!(hits, [("slack.exe".to_string(), Tier::Levenshtein)]);
    }

    // The opened one was pushed second, so it only wins if opening it raised
    // its score.
    #[test]
    fn opened_recent_file_ranks_above_unopened_recent_file() {
        const NOW: u64 = 1_000 * 86_400;
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        index.push("a.txt", root, false, 0, NOW).unwrap();
        let opened = index.push("b.txt", root, false, 0, NOW).unwrap();
        let indexes = [Arc::new(index)];
        let mut history = History::new();
        history.record_open(Path::new("root/b.txt"), NOW).unwrap();
        history.resolve(&indexes);
        let hits = run_with(&indexes, &history, "txt", 10, NOW);
        assert_eq!(hits[0].id, opened);
    }
}
