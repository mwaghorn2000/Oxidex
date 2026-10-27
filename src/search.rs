// Filename search over one `Index`. See RANKING.md for the full plan.
//
// Results are ranked in three layers, each with its own place to extend:
//
// 1. Tier (`Tier`, decided in `match_name`): what kind of match it is. A
//    new kind of match, like the Levenshtein pass, is a new `Tier` variant
//    and a new check in `match_name`.
// 2. Score (`Hit::score`): how good the match is within its tier. Match
//    quality comes from `match_name`. New ranking signals (modified time,
//    frecency, ...) are added on top where the `Hit` is built in `search`,
//    marked "RANKING SIGNALS". They will be `Scorer` implementations
//    (RANKING.md, build step 7). Don't put them in `match_name` or `compare`.
// 3. Tiebreaks (`compare`): only for hits with equal tier and score, so the
//    order never shuffles between searches. Not a place for ranking signals.
use std::cmp::Ordering;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub id: u32,
    pub tier: Tier,
    // How good the hit is within its tier. Higher is better. For now this
    // is match quality alone, 0 to 1000; ranking signals will add to it.
    pub score: i32,
}

// Subsequence scoring, after fzf: every matched character earns `MATCH`, plus
// a bonus at the start of a word or for following the previous match. Gaps
// between matches cost a fixed amount plus a little per skipped character.
const MATCH: i32 = 16;
const WORD_START: i32 = 8;
const CONSECUTIVE: i32 = 8;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;

// Fills `out` with up to `limit` hits for `query`, best first. An exact
// match is the whole name or the name without its extension. Matching is
// case-insensitive.
pub fn search(index: &Index, query: &str, limit: usize, out: &mut Vec<Hit>) {
    out.clear();
    if query.is_empty() || limit == 0 {
        return;
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
    // position of the worst kept hit, once `out` is full.
    let mut worst = None;
    let mut id = 0;
    while let Some(name) = index.name(id) {
        if let Some((tier, score)) = match_name(name, &query, ascii, &query_chars, &mut name_chars)
        {
            // RANKING SIGNALS: new signals (modified time, frecency, ...)
            // add to `score` here, before the hit competes for a place in
            // `out`. See the top of this file.
            let hit = Hit { id, tier, score };
            if out.len() < limit {
                out.push(hit);
                if out.len() == limit {
                    worst = Some(find_worst(index, out));
                }
            } else if let Some(w) = worst
                && compare(index, &hit, &out[w]) == Ordering::Less
            {
                out[w] = hit;
                worst = Some(find_worst(index, out));
            }
        }
        id += 1;
    }
    out.sort_by(|a, b| compare(index, a, b));
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
    // A leading dot starts the name (".bashrc"), not an extension.
    let stem = match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    };
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
// then shallower path, then earlier entry. Name length and depth are only
// looked up on a tie, so the common case stays cheap. Ranking signals belong
// in `Hit::score`, not here: see the top of this file.
fn compare(index: &Index, a: &Hit, b: &Hit) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then(b.score.cmp(&a.score))
        .then_with(|| name_len(index, a.id).cmp(&name_len(index, b.id)))
        .then_with(|| depth(index, a.id).cmp(&depth(index, b.id)))
        .then(a.id.cmp(&b.id))
}

// A missing entry sorts last rather than panicking.
fn name_len(index: &Index, id: u32) -> usize {
    index.name(id).map_or(usize::MAX, str::len)
}

// An entry whose parents form a cycle has no depth, so it sorts last.
fn depth(index: &Index, id: u32) -> u32 {
    index.depth(id).unwrap_or(u32::MAX)
}

// Position of the worst hit in `out`. A linear scan is fine: `out` holds
// only `limit` hits, and this only runs when a better hit replaces one.
fn find_worst(index: &Index, out: &[Hit]) -> usize {
    let mut worst = 0;
    for i in 1..out.len() {
        if compare(index, &out[i], &out[worst]) == Ordering::Greater {
            worst = i;
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

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
        search(index, query, limit, &mut out);
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
        search(&index, "a.txt", 10, &mut out);
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
        search(&index, "a.txt", 10, &mut out);
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
}
