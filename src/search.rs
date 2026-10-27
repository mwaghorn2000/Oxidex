// Filename search over a list of indexes, one per search directory, ranked
// together. See RANKING.md for the full plan.
//
// Results are ranked in three layers, each with its own place to extend:
//
// 1. Tier (`Tier`): what kind of match it is. A new kind of match is a new
//    `Tier` variant plus a check in `match_name`, or, if it should only run
//    sometimes, a separate pass in `search` like the typo pass.
// 2. Score (`Hit::score`): how good the match is within its tier. Match
//    quality comes from `match_name`. New ranking signals (modified time,
//    frecency, ...) are added on top where the `Hit` is built in `search`,
//    marked "RANKING SIGNALS". They will be `Scorer` implementations
//    (RANKING.md, build step 7). Don't put them in `match_name` or `compare`.
// 3. Tiebreaks (`compare`): only for hits with equal tier and score, so the
//    order never shuffles between searches. Not a place for ranking signals.
use std::cmp::Ordering;
use std::sync::atomic::{self, AtomicU64};

use crate::index::Index;

// Ordered best first, so sorting by tier puts exact matches on top. Tiers
// are strict: a hit in a better tier always ranks above one in a worse tier,
// whatever their scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    // The whole name, or the name without its extension.
    Exact,
    // The start of the name.
    Prefix,
    // The query's characters appear in the name in order, with gaps allowed.
    Subsequence,
    // Within a few typos (edits) of the name, with or without its extension.
    // A swap of two neighbouring letters counts as one edit.
    Levenshtein,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    // Position of the hit's index in the slice passed to `search`.
    pub index: u32,
    // Entry ID within that index.
    pub id: u32,
    pub tier: Tier,
    // How good the hit is within its tier. Higher is better. For now this
    // is match quality alone, 0 to 1000; ranking signals will add to it.
    pub score: i32,
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

// Subsequence scoring, after fzf: every matched character earns `MATCH`, plus
// a bonus at the start of a word or for following the previous match. Gaps
// between matches cost a fixed amount plus a little per skipped character.
const MATCH: i32 = 16;
const WORD_START: i32 = 8;
const CONSECUTIVE: i32 = 8;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;

// Fills `out` with up to `limit` hits for `query` from all of `indexes`,
// best first. An exact match is the whole name or the name without its
// extension. Matching is case-insensitive. If `cancel` fires, stops early,
// clears `out` so a partial list is never shown, and returns `Cancelled`.
pub fn search(
    indexes: &[Index],
    query: &str,
    limit: usize,
    cancel: &Cancel,
    out: &mut Vec<Hit>,
) -> Result<(), Cancelled> {
    out.clear();
    if query.is_empty() || limit == 0 {
        return Ok(());
    }
    let query = query.to_lowercase();
    // ASCII bytes never occur inside a multi-byte UTF-8 character, so an
    // ASCII query can be matched byte by byte without splitting characters.
    // Other queries are matched by `char`, decoding each name into a reused
    // buffer.
    let ascii = query.is_ascii();
    let query_chars: Vec<char> = query.chars().collect();
    let mut name_chars = Vec::new();

    // Keep only the best `limit` hits instead of collecting and sorting every
    // match: a one-letter query matches most of the index. `worst` is the
    // position of the worst kept hit, once `out` is full. It's shared across
    // indexes, so they compete for the same `limit` places.
    let mut worst = None;
    for (number, index) in indexes.iter().enumerate() {
        let mut id = 0;
        while let Some(name) = index.name(id) {
            // Checked at entry 0 too, so an already-stale search does no work.
            if id % CANCEL_CHECK_EVERY == 0 && cancel.is_cancelled() {
                out.clear();
                return Err(Cancelled);
            }
            if let Some((tier, score)) =
                match_name(name, &query, ascii, &query_chars, &mut name_chars)
            {
                // RANKING SIGNALS: new signals (modified time, frecency, ...)
                // add to `score` here, before the hit competes for a place in
                // `out`. See the top of this file.
                let hit = Hit {
                    index: number as u32,
                    id,
                    tier,
                    score,
                };
                keep(indexes, limit, out, &mut worst, hit);
            }
            id += 1;
        }
    }

    // Typo pass. Edit distance is far slower than the checks above, so it
    // only runs when they found too few hits to fill `out`. In that case
    // `out` holds every earlier-tier match, which is how names that already
    // matched are skipped.
    let max_edits = max_edits(query_chars.len());
    if out.len() < limit && max_edits > 0 {
        let mut rows = (Vec::new(), Vec::new(), Vec::new());
        for (number, index) in indexes.iter().enumerate() {
            let number = number as u32;
            let mut id = 0;
            while let Some(name) = index.name(id) {
                if id % CANCEL_CHECK_EVERY == 0 && cancel.is_cancelled() {
                    out.clear();
                    return Err(Cancelled);
                }
                let edits = typo_edits(
                    name,
                    &query,
                    ascii,
                    &query_chars,
                    max_edits,
                    &mut name_chars,
                    &mut rows,
                );
                if let Some(edits) = edits
                    && !out.iter().any(|h| h.index == number && h.id == id)
                {
                    // One edit scores 1000, two score 500. Zero edits would
                    // be an exact match, already in `out`.
                    let score = 1000 / edits.max(1) as i32;
                    // RANKING SIGNALS: add to `score` here too, as above.
                    let hit = Hit {
                        index: number,
                        id,
                        tier: Tier::Levenshtein,
                        score,
                    };
                    keep(indexes, limit, out, &mut worst, hit);
                }
                id += 1;
            }
        }
    }

    out.sort_by(|a, b| compare(indexes, a, b));
    Ok(())
}

// Typo allowance by query length in characters. Short queries get none, or
// "ab" would match every name of four characters or fewer. Capped at 2.
fn max_edits(query_len: usize) -> usize {
    match query_len {
        0..=2 => 0,
        3..=7 => 1,
        _ => 2,
    }
}

// Fewest edits between `query` and either the whole name or its stem, if no
// more than `max`. The stem is what people usually mistype ("slark" for
// "slack.exe"); the whole name covers typos in queries with an extension.
// `rows` is a reused buffer for `edit_distance`.
fn typo_edits(
    name: &str,
    query: &str,
    ascii: bool,
    query_chars: &[char],
    max: usize,
    name_chars: &mut Vec<char>,
    rows: &mut (Vec<usize>, Vec<usize>, Vec<usize>),
) -> Option<usize> {
    let stem = stem_of(name);
    let has_ext = stem.len() < name.len();
    // Unlike the subsequence check, an ASCII query needs `char`s here when
    // the name isn't ASCII: 'é' is two bytes, so bytes would count it twice.
    if ascii && name.is_ascii() {
        let whole = edit_distance(name.as_bytes(), query.as_bytes(), max, rows);
        if !has_ext {
            return whole;
        }
        let stem = edit_distance(stem.as_bytes(), query.as_bytes(), max, rows);
        return whole.into_iter().chain(stem).min();
    }
    name_chars.clear();
    name_chars.extend(name.chars());
    let whole = edit_distance(name_chars, query_chars, max, rows);
    if !has_ext {
        return whole;
    }
    let stem_len = stem.chars().count();
    let stem = edit_distance(&name_chars[..stem_len], query_chars, max, rows);
    whole.into_iter().chain(stem).min()
}

// Edit distance between `a` and `b` if it's at most `max`, else `None`.
// Levenshtein, plus a swap of two neighbouring letters counts as one edit
// (optimal string alignment), since swaps are the most common typo. Gives up
// early: when the length difference alone is over `max`, or when every cell
// in a row is, since later rows can't be lower. Generic for the same
// bytes-or-`char`s reason as `subsequence_score`.
fn edit_distance<T: Copy + PartialEq>(
    a: &[T],
    b: &[T],
    max: usize,
    rows: &mut (Vec<usize>, Vec<usize>, Vec<usize>),
) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    // Three rows of the usual table: `curr` for this character of `a`, `prev`
    // for the one before, and `prev2` before that, which swaps look back to.
    let (prev2, prev, curr) = rows;
    prev2.clear();
    prev.clear();
    prev.extend(0..=b.len());
    for (i, &x) in a.iter().enumerate() {
        curr.clear();
        curr.push(i + 1);
        let mut row_min = i + 1;
        for (j, &y) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(x != y);
            let mut cell = substitute.min(prev[j + 1] + 1).min(curr[j] + 1);
            if i > 0 && j > 0 && x == b[j - 1] && a[i - 1] == y {
                cell = cell.min(prev2[j - 1] + 1);
            }
            curr.push(cell);
            row_min = row_min.min(cell);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(prev2, prev);
        std::mem::swap(prev, curr);
    }
    let distance = prev[b.len()];
    (distance <= max).then_some(distance)
}

// Adds `hit` to `out` if there's room or it beats the worst kept hit.
fn keep(indexes: &[Index], limit: usize, out: &mut Vec<Hit>, worst: &mut Option<usize>, hit: Hit) {
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

// Decides the tier and match quality (0 to 1000) of `name`, or `None` if it
// doesn't match. Checks run best tier first, so a name gets its best tier.
// `query` is lowercased; `query_chars` is the same query as `char`s, and
// `name_chars` is a reused buffer, both only used for non-ASCII queries.
fn match_name(
    name: &str,
    query: &str,
    ascii: bool,
    query_chars: &[char],
    name_chars: &mut Vec<char>,
) -> Option<(Tier, i32)> {
    let stem = stem_of(name);
    // A whole-name match ranks just above a match without the extension.
    if name == query {
        return Some((Tier::Exact, 1000));
    }
    if stem == query {
        return Some((Tier::Exact, 900));
    }
    if name.starts_with(query) {
        // The less of the name left over, the closer to an exact match.
        let score = (1000 * query.len() / name.len()) as i32;
        return Some((Tier::Prefix, score));
    }
    let score = if ascii {
        subsequence_score(name.as_bytes(), query.as_bytes())
    } else {
        name_chars.clear();
        name_chars.extend(name.chars());
        subsequence_score(name_chars, query_chars)
    }?;
    Some((Tier::Subsequence, score))
}

// The name without its extension. A leading dot starts the name
// (".bashrc"), not an extension.
fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

// Scores the tightest match of `query` in `name`, or `None` if it isn't a
// subsequence. `query` must not be empty. Generic so the same code matches
// bytes (ASCII queries) and `char`s (everything else).
fn subsequence_score<T: Copy + PartialEq + Into<char>>(name: &[T], query: &[T]) -> Option<i32> {
    // Forward pass: where the first complete match ends.
    let mut q = 0;
    let mut end = None;
    for (i, &c) in name.iter().enumerate() {
        if c == query[q] {
            q += 1;
            if q == query.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;

    // Backward pass from `end`: the latest start, so the match is as tight
    // as it can be.
    let mut q = query.len();
    let mut start = end;
    for i in (0..=end).rev() {
        if name[i] == query[q - 1] {
            q -= 1;
            if q == 0 {
                start = i;
                break;
            }
        }
    }

    // Score the match inside `start..=end`. The weights are the constants at
    // the top of the file; tune them there.
    let mut score = 0;
    let mut q = 0;
    let mut prev: Option<usize> = None;
    for i in start..=end {
        if q == query.len() || name[i] != query[q] {
            continue;
        }
        score += MATCH;
        if i == 0 || is_separator(name[i - 1].into()) {
            score += WORD_START;
        }
        match prev {
            Some(p) if p + 1 == i => score += CONSECUTIVE,
            Some(p) => score -= GAP_START + GAP_EXTEND * (i - p - 2) as i32,
            None => {}
        }
        prev = Some(i);
        q += 1;
    }

    // Scale so a match that is all word starts and runs scores 1000.
    let best = (MATCH + WORD_START + CONSECUTIVE) * query.len() as i32;
    Some((score.max(0) * 1000 / best).min(1000))
}

// Characters that start a new word in a file name, for the word-start bonus.
// Names are lowercased, so camelCase word starts aren't visible here.
fn is_separator(c: char) -> bool {
    matches!(c, '.' | '_' | '-' | ' ')
}

// The full result order. Best first: tier, then score, then shorter name,
// then shallower path, then earlier index, then earlier entry. Name length
// and depth are only looked up on a tie, so the common case stays cheap.
// Ranking signals belong in `Hit::score`, not here: see the top of this file.
fn compare(indexes: &[Index], a: &Hit, b: &Hit) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then(b.score.cmp(&a.score))
        .then_with(|| name_len(indexes, a).cmp(&name_len(indexes, b)))
        .then_with(|| depth(indexes, a).cmp(&depth(indexes, b)))
        .then(a.index.cmp(&b.index))
        .then(a.id.cmp(&b.id))
}

// A missing entry sorts last rather than panicking.
fn name_len(indexes: &[Index], hit: &Hit) -> usize {
    let name = indexes.get(hit.index as usize).and_then(|i| i.name(hit.id));
    name.map_or(usize::MAX, str::len)
}

// An entry whose parents form a cycle has no depth, so it sorts last.
fn depth(indexes: &[Index], hit: &Hit) -> u32 {
    let depth = indexes
        .get(hit.index as usize)
        .and_then(|i| i.depth(hit.id));
    depth.unwrap_or(u32::MAX)
}

// Position of the worst hit in `out`. A linear scan is fine: `out` holds
// only `limit` hits, and this only runs when a better hit replaces one.
fn find_worst(indexes: &[Index], out: &[Hit]) -> usize {
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
    use std::sync::atomic::AtomicU64;

    // A root folder with each name as a direct child file.
    fn index_of(names: &[&str]) -> Index {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        for name in names {
            index.push(name, root, false, 0, 0).unwrap();
        }
        index
    }

    // Display name and tier of each hit, in result order.
    fn results(index: &Index, query: &str, limit: usize) -> Vec<(String, Tier)> {
        let mut out = Vec::new();
        search(
            std::slice::from_ref(index),
            query,
            limit,
            &Cancel::never(),
            &mut out,
        )
        .unwrap();
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
    fn whole_name_is_exact() {
        assert_eq!(tier_of(&["readme.md"], "readme.md"), Some(Tier::Exact));
    }

    #[test]
    fn stem_is_exact() {
        assert_eq!(tier_of(&["readme.md"], "readme"), Some(Tier::Exact));
    }

    #[test]
    fn start_of_name_is_prefix() {
        assert_eq!(tier_of(&["readme.md"], "read"), Some(Tier::Prefix));
    }

    #[test]
    fn scattered_letters_are_subsequence() {
        assert_eq!(tier_of(&["readme.md"], "rdm"), Some(Tier::Subsequence));
    }

    #[test]
    fn letters_out_of_order_dont_match() {
        assert_eq!(tier_of(&["readme.md"], "mdr"), None);
    }

    #[test]
    fn query_is_case_insensitive() {
        assert_eq!(tier_of(&["ReadMe.md"], "README"), Some(Tier::Exact));
    }

    #[test]
    fn folders_match() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        index.push("docs", root, true, 0, 0).unwrap();
        let hits = results(&index, "docs", 10);
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

    // 'é' is the bytes C3 A9. "ã©" is C3 A3 C2 A9, so a byte-wise subsequence
    // would match 'é' across two different characters.
    #[test]
    fn non_ascii_match_does_not_split_characters() {
        assert_eq!(tier_of(&["ã©"], "é"), None);
    }

    fn names(index: &Index, query: &str, limit: usize) -> Vec<String> {
        results(index, query, limit)
            .into_iter()
            .map(|h| h.0)
            .collect()
    }

    // In the quality tests below, the better match is pushed second and has
    // the longer name, so the tiebreaks alone would put it last.

    #[test]
    fn prefix_closer_to_full_name_ranks_higher() {
        let index = index_of(&["readme_old_backup", "readme"]);
        assert_eq!(names(&index, "read", 10), ["readme", "readme_old_backup"]);
    }

    #[test]
    fn subsequence_word_starts_rank_higher() {
        let index = index_of(&["xfoxbar", "foo_bar.txt"]);
        assert_eq!(names(&index, "fb", 10), ["foo_bar.txt", "xfoxbar"]);
    }

    #[test]
    fn subsequence_consecutive_letters_rank_higher() {
        let index = index_of(&["xaxbxc", "xxxabcxxxx"]);
        assert_eq!(names(&index, "abc", 10), ["xxxabcxxxx", "xaxbxc"]);
    }

    #[test]
    fn subsequence_smaller_gaps_rank_higher() {
        let index = index_of(&["xaxxxxxxbx", "xaxbxxxxxxxx"]);
        assert_eq!(names(&index, "ab", 10), ["xaxbxxxxxxxx", "xaxxxxxxbx"]);
    }

    // Greedy left-to-right matching would take the first 'a' and leave a big
    // gap. The tight "ab" at the end is the better match.
    #[test]
    fn subsequence_finds_tightest_match() {
        let index = index_of(&["xaxxbx", "xaxxxxxxxx_ab"]);
        assert_eq!(names(&index, "ab", 10), ["xaxxxxxxxx_ab", "xaxxbx"]);
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
        let mut out = Vec::new();
        search(&[index], "a.txt", 10, &Cancel::never(), &mut out).unwrap();
        let ids: Vec<u32> = out.iter().map(|h| h.id).collect();
        assert_eq!(ids, [shallow, deep]);
    }

    #[test]
    fn ties_go_to_earlier_entry() {
        let mut index = Index::new();
        let root = index.push("root", u32::MAX, true, 0, 0).unwrap();
        let first = index.push("a.txt", root, false, 0, 0).unwrap();
        let second = index.push("a.txt", root, false, 0, 0).unwrap();
        let mut out = Vec::new();
        search(&[index], "a.txt", 10, &Cancel::never(), &mut out).unwrap();
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
    fn hits_across(indexes: &[Index], query: &str, limit: usize) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        search(indexes, query, limit, &Cancel::never(), &mut out).unwrap();
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
    fn one_typo_matches() {
        assert_eq!(tier_of(&["slack.exe"], "slark"), Some(Tier::Levenshtein));
    }

    #[test]
    fn extra_letter_matches() {
        assert_eq!(tier_of(&["slack.exe"], "slacck"), Some(Tier::Levenshtein));
    }

    // Plain Levenshtein counts a swap as 2 edits, over the 1 allowed here.
    #[test]
    fn swapped_letters_are_one_edit() {
        assert_eq!(tier_of(&["readme.md"], "raedme"), Some(Tier::Levenshtein));
    }

    // "maim.rs" is 3 edits from the stem "main", but 1 from the whole name.
    #[test]
    fn typo_against_whole_name_matches() {
        assert_eq!(tier_of(&["main.rs"], "maim.rs"), Some(Tier::Levenshtein));
    }

    #[test]
    fn short_query_allows_one_edit() {
        assert_eq!(tier_of(&["slack.exe"], "sxaxk"), None);
    }

    #[test]
    fn long_query_allows_two_edits() {
        let tier = tier_of(&["documentation.pdf"], "docuemntation");
        assert_eq!(tier, Some(Tier::Levenshtein));
    }

    #[test]
    fn very_short_query_has_no_typo_matching() {
        assert_eq!(tier_of(&["ac"], "ab"), None);
    }

    // 'é' is two bytes, so comparing bytes would count this as two edits.
    #[test]
    fn typo_counts_characters_not_bytes() {
        assert_eq!(tier_of(&["café.txt"], "cafe"), Some(Tier::Levenshtein));
    }

    #[test]
    fn typos_rank_below_other_tiers() {
        let index = index_of(&["slick", "slack_notes"]);
        let hits = results(&index, "slack", 10);
        assert_eq!(
            hits,
            [
                ("slack_notes".to_string(), Tier::Prefix),
                ("slick".to_string(), Tier::Levenshtein),
            ]
        );
    }

    // Enough earlier-tier matches to fill `limit`, so the typo pass is skipped.
    #[test]
    fn typo_pass_skipped_when_enough_results() {
        let index = index_of(&["slick", "slack_notes"]);
        assert_eq!(names(&index, "slack", 1), ["slack_notes"]);
    }

    #[test]
    fn fewer_edits_rank_higher() {
        let index = index_of(&["notexooxs", "notebooxs"]);
        assert_eq!(names(&index, "notebooks", 10), ["notebooxs", "notexooxs"]);
    }

    // The exact match is also 0 edits from the query; it must appear once.
    #[test]
    fn earlier_tier_match_is_not_repeated() {
        let index = index_of(&["slack.exe", "slick"]);
        assert_eq!(names(&index, "slack", 10), ["slack.exe", "slick"]);
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
        let result = search(
            &[index_of(&["a.txt"])],
            "a",
            10,
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
        let result = search(
            &[index_of(&["a.txt"])],
            "a",
            10,
            &Cancel::new(&latest, 1),
            &mut out,
        );
        assert_eq!(result, Err(Cancelled));
        assert!(out.is_empty());
    }
}
