// Deciding whether one name matches a query, in which tier and how well
// (match quality, 0 to 1000). Pure functions over a name and a `Query`;
// scanning the index and keeping the best hits is search.rs. See RANKING.md,
// "Matching rules" and "Match quality".
//
// Adding a tier: see the overview at the top of search.rs. `may_match` runs
// first, so anything a new tier matches must pass it, or search skips the
// name before `match_name` sees it.
use std::path::{Component, Path};

use crate::index::{Index, char_mask, fold_case};

/// Ordered best first, so sorting by tier puts exact matches on top. Tiers
/// are strict: a hit in a better tier always ranks above one in a worse tier,
/// whatever their scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The whole name, or the name without its extension. Also a file opened
    /// almost daily that is within an edit or two of the query: see
    /// `match_history`.
    Exact,
    /// The start of the name.
    Prefix,
    /// Within a few typos (edits) of the name, with or without its extension.
    /// A swap of two neighbouring letters counts as one edit. Ranked above
    /// subsequence matches: a near-miss of the whole name says more than the
    /// query's letters scattered through a long name ("raedme" should find
    /// "readme.md" before "deprecatedMethod.js").
    Levenshtein,
    /// The query's characters appear in the name in order, with gaps allowed.
    Subsequence,
}

// Subsequence scoring, after fzf: every matched character earns `MATCH`, plus
// a bonus at the start of a word or for following the previous match. Gaps
// between matches cost a fixed amount plus a little per skipped character.
const MATCH: i32 = 16;
const WORD_START: i32 = 8;
const CONSECUTIVE: i32 = 8;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;

// Tier jumping (RANKING.md): the history score a file needs to rank with the
// exact matches. 4 is about 5 opens in the last 7 days.
const JUMP_FRECENCY: f64 = 4.0;
// Edits a tier jump allows for stems of 8+ characters (1 for shorter ones).
const MAX_JUMP_EDITS: usize = 2;

// The query in the forms matching needs, worked out once per search.
pub(crate) struct Query {
    // Lowercased, to match the lowercased names in the index. In path mode,
    // only the part after the last slash.
    pub(crate) text: String,
    // Path mode: the lowercased parts before the last slash, outermost first.
    // Empty when the query has no slash.
    pub(crate) folders: Vec<String>,
    pub(crate) chars: Vec<char>,
    // ASCII bytes never occur inside a multi-byte UTF-8 character, so an
    // ASCII query can be matched byte by byte without splitting characters.
    // Other queries are matched by `char`.
    pub(crate) ascii: bool,
    pub(crate) max_edits: usize,
    // `char_mask` of `text`, for `may_match`.
    pub(crate) mask: u64,
    // Each character the query has more than once, with its count, for
    // `lacking`. Usually empty.
    repeats: Vec<(char, usize)>,
}

impl Query {
    pub(crate) fn new(query: &str) -> Query {
        let lowercase = fold_case(query);
        let mut parts: Vec<&str> = lowercase.split(['/', '\\']).collect();
        let text = parts.pop().unwrap_or_default().to_string();
        // Empty parts ("/notes", "docs//notes") would match any folder, so
        // they're dropped rather than checked. So are "." and "~" from pasted
        // relative or home paths: they name no folder in the index.
        let folders = parts
            .into_iter()
            .filter(|p| !p.is_empty() && *p != "." && *p != "~")
            .map(String::from)
            .collect();
        let chars: Vec<char> = text.chars().collect();
        let mut repeats: Vec<(char, usize)> = Vec::new();
        for &c in &chars {
            let count = chars.iter().filter(|&&x| x == c).count();
            if count > 1 && !repeats.iter().any(|&(r, _)| r == c) {
                repeats.push((c, count));
            }
        }
        Query {
            repeats,
            folders,
            ascii: text.is_ascii(),
            max_edits: max_edits(chars.len()),
            mask: char_mask(&text),
            text,
            chars,
        }
    }

    // Quick filter on a name's `char_mask`, run on every name before
    // `match_name`. Each distinct query character missing from the name needs
    // at least one edit, and every tier but typos needs none missing. So a
    // name missing more than `max_edits` can't match in any tier.
    #[inline]
    pub(crate) fn may_match(&self, mask: u64) -> bool {
        (self.mask & !mask).count_ones() as usize <= self.max_edits
    }

    // How many copies of its repeated characters `name` lacks. The mask
    // only says which characters a name has, not how many, so a query like
    // "eeeeeeee" passes it for almost every name. Each edit supplies at most
    // one missing copy, so a name lacking more than `max_edits` can't be a
    // typo match, and one lacking any can't be a subsequence match.
    fn lacking(&self, name: &str) -> usize {
        let mut lacking = 0;
        for &(c, count) in &self.repeats {
            let have = if c.is_ascii() {
                name.bytes().filter(|&b| b == c as u8).count()
            } else {
                name.chars().filter(|&x| x == c).count()
            };
            lacking += count.saturating_sub(have);
        }
        lacking
    }

    // `may_match` for `match_history`, whose extras allow up to 2 edits even
    // when `max_edits` is lower (a tier jump), so up to 2 missing characters.
    #[inline]
    pub(crate) fn may_match_history(&self, mask: u64) -> bool {
        (self.mask & !mask).count_ones() as usize <= self.max_edits.max(MAX_JUMP_EDITS)
    }
}

// Buffers reused for every name, so matching doesn't allocate per name.
#[derive(Default)]
pub(crate) struct Scratch {
    // A name decoded into `char`s, for non-ASCII matching.
    name_chars: Vec<char>,
    // Rows of the edit-distance table.
    rows: (Vec<usize>, Vec<usize>, Vec<usize>),
}

// Typo allowance by query length in characters. Capped at 2. Short queries
// get none: typos rank above subsequence matches, and a 3-letter query is
// usually an abbreviation ("rdm" for "readme"), not a typo of "adm".
fn max_edits(query_len: usize) -> usize {
    match query_len {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

// Fewest edits between the query and either the whole name or its stem, if
// no more than `query.max_edits`. The stem is what people usually mistype
// ("slark" for "slack.exe"); the whole name covers typos in queries with an
// extension.
fn typo_edits(name: &str, stem: &str, query: &Query, scratch: &mut Scratch) -> Option<usize> {
    let max = query.max_edits;
    let has_ext = stem.len() < name.len();
    let Scratch { name_chars, rows } = scratch;
    // Unlike the subsequence check, an ASCII query needs `char`s here when
    // the name isn't ASCII: 'é' is two bytes, so bytes would count it twice.
    if query.ascii && name.is_ascii() {
        let q = query.text.as_bytes();
        let whole = edit_distance(name.as_bytes(), q, max, rows);
        if !has_ext {
            return whole;
        }
        let stem = edit_distance(stem.as_bytes(), q, max, rows);
        return whole.into_iter().chain(stem).min();
    }
    name_chars.clear();
    name_chars.extend(name.chars());
    let whole = edit_distance(name_chars, &query.chars, max, rows);
    if !has_ext {
        return whole;
    }
    let stem_len = stem.chars().count();
    let stem = edit_distance(&name_chars[..stem_len], &query.chars, max, rows);
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

// Decides the tier and match quality (0 to 1000) of `name`, or `None` if it
// doesn't match. Checks run best tier first, so a name gets its best tier.
pub(crate) fn match_name(name: &str, query: &Query, scratch: &mut Scratch) -> Option<(Tier, i32)> {
    let q = query.text.as_str();
    let stem = stem_of(name);
    // A whole-name match ranks just above a match without the extension.
    if name == q {
        return Some((Tier::Exact, 1000));
    }
    if stem == q {
        return Some((Tier::Exact, 900));
    }
    if name.starts_with(q) {
        // The less of the name left over, the closer to an exact match.
        let score = (1000 * q.len() / name.len()) as i32;
        return Some((Tier::Prefix, score));
    }
    // 1–2 character queries stop here. Almost every name contains one or two
    // given letters somewhere, so a subsequence match says nothing useful and
    // scoring them all is slow. Typos already need 4+ characters.
    if query.chars.len() < 3 {
        return None;
    }
    let lacking = query.lacking(name);
    if lacking > query.max_edits {
        return None;
    }
    if query.max_edits > 0
        && let Some(edits) = typo_edits(name, stem, query, scratch)
    {
        // One edit scores 1000, two score 500. Zero edits would have been an
        // exact match above.
        return Some((Tier::Levenshtein, 1000 / edits.max(1) as i32));
    }
    if lacking > 0 {
        return None;
    }
    let score = if query.ascii {
        subsequence_score(name.as_bytes(), q.as_bytes())
    } else {
        let name_chars = &mut scratch.name_chars;
        name_chars.clear();
        name_chars.extend(name.chars());
        subsequence_score(name_chars, &query.chars)
    }?;
    Some((Tier::Subsequence, score))
}

// `match_name` for a file in the user's history, which is `frecency` (its
// decayed score), with two extras that are only worth their cost on a list
// this small. See RANKING.md, "Matching rules" and "Tier jumping". Run
// `Query::may_match_history` first, not `may_match`: a jump allows edits a
// short query otherwise doesn't get.
pub(crate) fn match_history(
    name: &str,
    query: &Query,
    frecency: f64,
    scratch: &mut Scratch,
) -> Option<(Tier, i32)> {
    let normal = match_name(name, query, scratch);
    if let Some((Tier::Exact, _)) = normal {
        return normal;
    }
    let Scratch { name_chars, rows } = scratch;
    name_chars.clear();
    name_chars.extend(stem_of(name).chars());
    let q = &query.chars;

    // Tier jumping: a file opened almost daily whose stem is within an edit
    // of the query (two for long stems) ranks with the exact matches, so
    // `slark` finds a daily `slack.exe` above an unopened `slark.txt`.
    if frecency >= JUMP_FRECENCY && q.len() >= 3 {
        let max = if name_chars.len() >= 8 {
            MAX_JUMP_EDITS
        } else {
            1
        };
        if let Some(edits) = edit_distance(name_chars, q, max, rows) {
            return Some((Tier::Exact, 1000 / edits.max(1) as i32));
        }
    }
    if let Some((Tier::Prefix | Tier::Levenshtein, _)) = normal {
        return normal;
    }

    // Typos while typing: compare the query with each start of the stem
    // near its own length, so `slar` finds `slack` before it's fully typed.
    // Same edit allowance as `match_name`.
    let max = query.max_edits;
    let mut best: Option<usize> = None;
    if max > 0 {
        let shortest = q.len().saturating_sub(max).max(1);
        let longest = (q.len() + max).min(name_chars.len());
        for len in shortest..=longest {
            if let Some(edits) = edit_distance(&name_chars[..len], q, max, rows) {
                best = Some(best.map_or(edits, |b| b.min(edits)));
            }
        }
    }
    match best {
        Some(edits) => Some((Tier::Levenshtein, 1000 / edits.max(1) as i32)),
        None => normal,
    }
}

// Path mode: true if each folder part matches a folder above `id`, in order.
// The last part matches the nearest such folder and each earlier part one
// further up, so `a/b/x` needs `b` inside `a`. Taking the nearest match each
// time never misses a chain that a later choice would have found. Parts
// match as a subsequence, so `dcs` matches `docs`. Parts left over at the
// root are matched against the folders in the root's own path, so a pasted
// full path like `/home/me/projects/x` works with the root `/home/me/projects`.
pub(crate) fn folders_match(index: &Index, id: u32, folders: &[String]) -> bool {
    // `None` means the parents form a cycle. The depth also bounds the walk.
    let Some(depth) = index.depth(id) else {
        return false;
    };
    let mut remaining = folders.len();
    let mut curr = id;
    for _ in 0..depth {
        if remaining == 0 {
            break;
        }
        let Some(parent) = index.parent(curr) else {
            return false;
        };
        let Some(name) = index.name(parent) else {
            return false;
        };
        if in_order(name, &folders[remaining - 1]) {
            remaining -= 1;
        }
        curr = parent;
    }
    if remaining == 0 {
        return true;
    }
    // `curr` is the root now. Its name was its last part, so check the
    // folders above that, nearest first. Rare enough (only path-mode hits
    // with parts left over) that folding each part's case is fine.
    let Some(root_path) = index.display_name(curr).map(Path::new) else {
        return false;
    };
    let Some(above) = root_path.parent() else {
        return false;
    };
    for part in above.components().rev() {
        let Some(name) = part.as_os_str().to_str() else {
            continue;
        };
        if matches!(part, Component::Normal(_) | Component::Prefix(_))
            && in_order(&fold_case(name), &folders[remaining - 1])
        {
            remaining -= 1;
            if remaining == 0 {
                return true;
            }
        }
    }
    false
}

// True if `part`'s characters appear in `name` in order. Compares `char`s,
// so it works for any query; it's only run on a hit's few folders.
fn in_order(name: &str, part: &str) -> bool {
    let mut chars = name.chars();
    part.chars().all(|p| chars.any(|n| n == p))
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

#[cfg(test)]
mod tests {
    use super::*;

    // Tier and quality of `name` for `query`, as search sees them: the mask
    // filter first, then `match_name`. `name` must be lowercase, like the
    // names in an index.
    fn matched(name: &str, query: &str) -> Option<(Tier, i32)> {
        let query = Query::new(query);
        if !query.may_match(char_mask(name)) {
            return None;
        }
        match_name(name, &query, &mut Scratch::default())
    }

    fn tier(name: &str, query: &str) -> Option<Tier> {
        matched(name, query).map(|(tier, _)| tier)
    }

    fn quality(name: &str, query: &str) -> i32 {
        matched(name, query).unwrap().1
    }

    #[test]
    fn whole_name_is_exact() {
        assert_eq!(matched("readme.md", "readme.md"), Some((Tier::Exact, 1000)));
    }

    // Ranked just below a whole-name match.
    #[test]
    fn stem_is_exact() {
        assert_eq!(matched("readme.md", "readme"), Some((Tier::Exact, 900)));
    }

    #[test]
    fn start_of_name_is_prefix() {
        assert_eq!(tier("readme.md", "read"), Some(Tier::Prefix));
    }

    #[test]
    fn scattered_letters_are_subsequence() {
        assert_eq!(tier("readme.md", "rdm"), Some(Tier::Subsequence));
    }

    #[test]
    fn letters_out_of_order_dont_match() {
        assert_eq!(tier("readme.md", "mdr"), None);
    }

    // 'é' is the bytes C3 A9. "ã©" is C3 A3 C2 A9, so a byte-wise subsequence
    // would match 'é' across two different characters.
    #[test]
    fn non_ascii_match_does_not_split_characters() {
        assert_eq!(tier("ã©xy", "éxy"), None);
    }

    #[test]
    fn short_query_matches_exact_and_prefix_only() {
        assert_eq!(tier("banana", "a"), None);
        assert_eq!(tier("xaxb", "ab"), None);
        assert_eq!(tier("abc", "ab"), Some(Tier::Prefix));
    }

    #[test]
    fn three_letter_query_matches_subsequence() {
        assert_eq!(tier("xaxbxc", "abc"), Some(Tier::Subsequence));
    }

    #[test]
    fn prefix_closer_to_full_name_scores_higher() {
        assert!(quality("readme", "read") > quality("readme_old_backup", "read"));
    }

    #[test]
    fn subsequence_word_starts_score_higher() {
        assert!(quality("foo_bar.txt", "fba") > quality("xfoxbar", "fba"));
    }

    #[test]
    fn subsequence_consecutive_letters_score_higher() {
        assert!(quality("xxxabcxxxx", "abc") > quality("xaxbxc", "abc"));
    }

    #[test]
    fn subsequence_smaller_gaps_score_higher() {
        assert!(quality("xaxbxxxxxxxx", "abx") > quality("xaxxxxxxbx", "abx"));
    }

    // Greedy left-to-right matching would take the first 'a' and leave a big
    // gap, scoring below "xabxc". The tight "abc" at the end scores above it.
    #[test]
    fn subsequence_finds_tightest_match() {
        assert!(quality("xaxxxxxxxx_abc", "abc") > quality("xabxc", "abc"));
    }

    #[test]
    fn one_typo_matches() {
        assert_eq!(tier("slack.exe", "slark"), Some(Tier::Levenshtein));
    }

    #[test]
    fn extra_letter_matches() {
        assert_eq!(tier("slack.exe", "slacck"), Some(Tier::Levenshtein));
    }

    // Plain Levenshtein counts a swap as 2 edits, over the 1 allowed here.
    #[test]
    fn swapped_letters_are_one_edit() {
        assert_eq!(tier("readme.md", "raedme"), Some(Tier::Levenshtein));
    }

    // "maim.rs" is 3 edits from the stem "main", but 1 from the whole name.
    #[test]
    fn typo_against_whole_name_matches() {
        assert_eq!(tier("main.rs", "maim.rs"), Some(Tier::Levenshtein));
    }

    #[test]
    fn short_query_allows_one_edit() {
        assert_eq!(tier("slack.exe", "sxaxk"), None);
    }

    #[test]
    fn long_query_allows_two_edits() {
        let tier = tier("documentation.pdf", "docuemntation");
        assert_eq!(tier, Some(Tier::Levenshtein));
    }

    // 'y' and 'x' are both missing from the name: exactly the 2 edits a
    // 13-letter query allows, so `may_match` must let it by.
    #[test]
    fn two_missing_letters_within_two_edits_match() {
        let tier = tier("documentation.pdf", "dycumentatiox");
        assert_eq!(tier, Some(Tier::Levenshtein));
    }

    // Typos rank above subsequence matches, so a 3-letter query like "rdm"
    // would otherwise put names like "adm" above "readme".
    #[test]
    fn short_query_has_no_typo_matching() {
        assert_eq!(tier("abd", "abc"), None);
    }

    // 'é' is two bytes, so comparing bytes would count this as two edits.
    #[test]
    fn typo_counts_characters_not_bytes() {
        assert_eq!(tier("café.txt", "cafe"), Some(Tier::Levenshtein));
    }

    #[test]
    fn fewer_edits_score_higher() {
        assert_eq!(
            matched("notebooxs", "notebooks"),
            Some((Tier::Levenshtein, 1000))
        );
        assert_eq!(
            matched("notexooxs", "notebooks"),
            Some((Tier::Levenshtein, 500))
        );
    }

    // Counting repeated letters must never drop a real match.
    #[test]
    fn repeated_letters_still_match_every_tier() {
        assert_eq!(tier("bookkeeper.txt", "bookkeeper"), Some(Tier::Exact));
        assert_eq!(tier("aaab", "aaa"), Some(Tier::Prefix));
        // "boookeeper" has one 'k' and three 'o's: 2 edits, allowed at 10.
        assert_eq!(
            tier("bookkeeper.txt", "boookeeper"),
            Some(Tier::Levenshtein)
        );
        assert_eq!(tier("xaxaxa", "aaa"), Some(Tier::Subsequence));
    }

    #[test]
    fn name_lacking_repeated_letters_does_not_match() {
        // One 'a' short, and a 3-letter query gets no typos.
        assert_eq!(tier("xaxa_long_file", "aaa"), None);
        assert_eq!(tier("eee.txt", "eeeeeeee"), None);
    }

    // `match_history` for an opened file with history score `frecency`, after
    // the same mask filter search runs.
    fn opened(name: &str, query: &str, frecency: f64) -> Option<Tier> {
        let query = Query::new(query);
        if !query.may_match_history(char_mask(name)) {
            return None;
        }
        match_history(name, &query, frecency, &mut Scratch::default()).map(|(tier, _)| tier)
    }

    #[test]
    fn daily_file_jumps_to_exact() {
        assert_eq!(opened("slack.exe", "slark", 4.0), Some(Tier::Exact));
    }

    #[test]
    fn file_opened_less_often_does_not_jump() {
        assert_eq!(opened("slack.exe", "slark", 3.9), Some(Tier::Levenshtein));
    }

    // A 3-letter query gets no typos in `match_name`, but can still jump.
    #[test]
    fn three_letter_query_can_jump() {
        assert_eq!(opened("cat.txt", "cay", 5.0), Some(Tier::Exact));
        assert_eq!(opened("cat.txt", "cay", 0.0), None);
    }

    #[test]
    fn two_letter_query_does_not_jump() {
        assert_eq!(opened("ab.txt", "ax", 9.0), None);
    }

    #[test]
    fn long_stem_jumps_within_two_edits() {
        let tier = opened("documentation.pdf", "docuemntatoin", 4.0);
        assert_eq!(tier, Some(Tier::Exact));
    }

    #[test]
    fn short_stem_does_not_jump_with_two_edits() {
        assert_eq!(opened("slack.exe", "slxcx", 9.0), None);
    }

    // Not a typo of "slack" yet, but one of its start "slac".
    #[test]
    fn opened_file_matches_typos_while_typing() {
        assert_eq!(tier("slack.exe", "slar"), None);
        assert_eq!(opened("slack.exe", "slar", 0.0), Some(Tier::Levenshtein));
    }

    // From RANKING.md's TODO: one wrong letter in a partly typed long name.
    #[test]
    fn opened_long_name_matches_typo_while_typing() {
        let name = "the binding of isaac rebirth.url";
        assert_eq!(
            opened(name, "the binding of iscac", 0.0),
            Some(Tier::Levenshtein)
        );
    }

    // "slask" is a subsequence of the name, but also 1 edit from its start
    // "slack", which is the better tier.
    #[test]
    fn typo_while_typing_beats_subsequence() {
        let name = "slack_notes_sk";
        assert_eq!(tier(name, "slask"), Some(Tier::Subsequence));
        assert_eq!(opened(name, "slask", 0.0), Some(Tier::Levenshtein));
    }
}
