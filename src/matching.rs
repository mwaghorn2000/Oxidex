// Deciding whether one name matches a query, in which tier and how well
// (match quality, 0 to 1000). Pure functions over a name and a `Query`;
// scanning the index and keeping the best hits is search.rs. See RANKING.md,
// "Matching rules" and "Match quality".
//
// A new kind of match is a new `Tier` variant plus a check in `match_name`.
// It must keep `Query::may_match` true for every name it can match, or
// search skips those names before `match_name` sees them.
use crate::index::{Index, char_mask};

// Ordered best first, so sorting by tier puts exact matches on top. Tiers
// are strict: a hit in a better tier always ranks above one in a worse tier,
// whatever their scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    // The whole name, or the name without its extension.
    Exact,
    // The start of the name.
    Prefix,
    // Within a few typos (edits) of the name, with or without its extension.
    // A swap of two neighbouring letters counts as one edit. Ranked above
    // subsequence matches: a near-miss of the whole name says more than the
    // query's letters scattered through a long name ("raedme" should find
    // "readme.md" before "deprecatedMethod.js").
    Levenshtein,
    // The query's characters appear in the name in order, with gaps allowed.
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
}

impl Query {
    pub(crate) fn new(query: &str) -> Query {
        let lowercase = query.to_lowercase();
        let mut parts: Vec<&str> = lowercase.split(['/', '\\']).collect();
        let text = parts.pop().unwrap_or_default().to_string();
        // Empty parts ("/notes", "docs//notes") would match any folder, so
        // they're dropped rather than checked.
        let folders = parts
            .into_iter()
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        let chars: Vec<char> = text.chars().collect();
        Query {
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
    if query.max_edits > 0
        && let Some(edits) = typo_edits(name, stem, query, scratch)
    {
        // One edit scores 1000, two score 500. Zero edits would have been an
        // exact match above.
        return Some((Tier::Levenshtein, 1000 / edits.max(1) as i32));
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

// Path mode: true if each folder part matches a folder above `id`, in order.
// The last part matches the nearest such folder and each earlier part one
// further up, so `a/b/x` needs `b` inside `a`. Taking the nearest match each
// time never misses a chain that a later choice would have found. Parts
// match as a subsequence, so `dcs` matches `docs`.
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
    remaining == 0
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
