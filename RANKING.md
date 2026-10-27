# Ranking

How search results are ordered. This is the plan for `search.rs`; none of
it is built yet. The numbers are starting points to tune with real use.

## Input

Each search directory has its own `Index`, so search takes `&[Index]` and
identifies a result as (index number, entry id). Directories nested inside
another listed directory are dropped, so no file is indexed twice.

## Order

Results are sorted by, in order:

1. **Tier**: exact, then prefix, then Levenshtein (typos), then
   subsequence (fzf-style). Typos rank above subsequence matches: a
   near-miss of the whole name (`raedme` → `readme.md`) says more than the
   query's letters scattered through a long name (`deprecatedMethod.js`).
2. **Score** within the tier: match quality plus the scoring boosts (below),
   added together. Weights are set so match quality dominates unless a
   boost is large.
3. **Tiebreaks**: shorter name, then shallower path, then index order, so
   equal results never shuffle between searches.

Boosts only reorder results within a tier. The one exception is tier
jumping, below.

## Matching rules

- An exact match is the whole name or the stem, so `readme` exactly matches
  `readme.md`. A whole-name match ranks just above a stem match.
- Spaces in a query are literal characters: `foo bar` matches names
  containing `foo bar`, in that order.
- In path mode (`docs/rea`), each folder part must subsequence-match a
  folder name on the way up the parent chain. Results are ranked by the
  file-name part only.
- The Levenshtein tier allows edits by query length: none under 4
  characters, 1 for 4–7 characters, 2 for 8 or more. A 3-letter query is
  usually an abbreviation (`rdm` for `readme`), and since typos rank above
  subsequence matches, typo hits like `adm` would bury it. It skips names
  whose length difference rules them out.
- Levenshtein compares against both the stem and the whole name, and takes
  the smaller distance. The stem catches the usual typo (`slark` →
  `slack.exe`); the whole name catches typos in queries that include an
  extension (`maim.rs` → `main.rs`).
- Levenshtein counts characters, not bytes, so `cafe` is 1 edit from `café`.
- A swap of two neighbouring letters counts as 1 edit, not 2 (optimal
  string alignment), so `raedme` finds `readme`.
- Levenshtein runs in the same pass as the other tiers, for every name that
  isn't an exact or prefix match. It used to be a second pass that only
  ran when the first found fewer than `limit` hits, but on a real index
  scattered subsequence matches nearly always fill `limit`, so typos were
  never found.
- **Open history** is searched with every tier, including Levenshtein,
  even when earlier tiers already return many results. It's small, so this
  is cheap.
- For history entries, typos match while typing: the query is compared
  against the start of the stem (prefix edit distance), so `slar` matches
  `slack`.

## Match quality

How results are ranked within each tier, before boosts:

- **Exact:** a whole-name match beats a stem match.
- **Prefix:** the closer the name's length is to the query's, the better
  (`read` → `readme` beats `readme_old_backup`).
- **Subsequence:** fzf-style. Bonuses for matching at the start of the name,
  right after `.` `_` `-` or a space, and for runs of consecutive matched
  characters. Gaps cost points. No camelCase bonus, because names are
  lowercased in the index.
- **Levenshtein:** fewer edits is better: 1 edit scores 1000, 2 score 500.

Folders and files are ranked the same way, with no penalty or bonus for
either.

### Unicode

- ASCII queries are matched byte by byte, which is fast and correct.
- Other queries are matched character by character, because a byte-wise
  subsequence could match the first byte of one character and the second
  byte of another.
- No accent folding: `cafe` doesn't match `café`.

## Scoring signals

### Frecency

Oxidex keeps its own history of files opened through search.

- Each open adds +1.
- At most one open counts per 12 hours.
- The score halves every 7 days.

### Modified time

Files never opened through Oxidex fall back to their modified time, so
newer files rank higher.

Access time (atime) was rejected as a signal. It's unreliable: Linux
`relatime`, lazy or system-managed updates on NTFS, and reads by antivirus
and backup tools all make it noisy.

### Not used (for now)

- No boost by app or file type.

## Tier jumping

A very frequently opened file can move above higher-tier results. That
way a typo like `slark` finds a daily-opened `slack.exe` above an unopened
`slark.txt`.

A file can only jump tiers when:

- its frecency score is 4 or more (about 5 of the last 7 days),
- the query is 3+ characters, and
- the name's stem is within 1 edit of the query (2 edits for stems of 8+
  characters).

## Streaming

Results are sent to the popup in phases, so cached results appear on the
first frame and the full scan fills in after:

1. **Cached results**: history matches, plus the previous query's results
   filtered against the new query.
2. **The main index**: exact, prefix, Levenshtein and subsequence
   matches, in one pass.

Each update is the full current top-`limit` list, and the popup replaces
what it shows. History hits already have their final scores, so they don't
move as later phases arrive.

How it runs:

- Core exposes each phase as a plain function writing into a reused
  `out: &mut Vec<Hit>`. Core has no threads or channels.
- The app runs a search thread. The UI sends it `(gen, query)` over an
  `mpsc` channel, and it sends back `(gen, hits)` after each phase, then
  calls `ctx.request_repaint()`. It skips straight to the newest queued
  query.
- **Cancellation:** the UI bumps a shared `AtomicU64` on every keystroke.
  The scan checks it every 4096 entries and stops when its generation is
  stale. The UI also drops any snapshot whose generation isn't the latest.
- **Indexes:** the indexes are shared as `RwLock<Vec<Arc<Index>>>`. A
  search clones the `Arc`s and scans without holding the lock. A re-walk
  swaps in a new `Arc`, and in-flight searches keep using the old one.

## Previous-query cache

When the new query is the previous query with characters appended (`rea`
→ `read`), every subsequence match for the new query was also a match for
the old one. Phase 2 then re-checks only the previous matches instead of
scanning the whole index, so typing gets faster as the query grows.

- Each index keeps a reused `Vec<u32>` of all entries that matched the last
  completed phase 2. This is every match, not just the top `limit`.
- The cache is only used when the previous scan finished (wasn't
  cancelled) and the index's `Arc` hasn't been swapped since.
- Any other edit (backspace, an edit mid-query, a switch into or out of
  path mode) does a full scan.
- **Unsolved:** Levenshtein matches don't narrow this way. A name can be a
  typo match for `raedme` without having matched `raedm` at all. Since
  typos now run in the same pass, step 9 has to work out how to find typo
  matches without a full scan. Otherwise the cache won't help queries of
  4+ characters.

## Design

Scoring must be modular: adding a new signal or adjusting an old one
shouldn't touch the rest of the search code.

- Scoring is a trait, with one implementation per signal (frecency,
  modified time, ...).
- Each implementation is self-contained and keeps its tunable numbers in
  one obvious place.
- Adding a signal means writing a new implementation and registering it.

The plan, in `src/score.rs`:

```rust
// Everything a scorer may look at for one matched entry. New fields can be
// added without changing existing scorers.
pub struct Candidate<'a> {
    pub index: &'a Index,
    pub id: u32,
    pub tier: Tier,
    pub quality: i32,                       // from the matcher
    pub history: Option<&'a HistoryRecord>, // None until step 10
    pub now: u64,                           // passed in so tests are deterministic
}

pub trait Scorer: Send + Sync {
    fn name(&self) -> &'static str;
    fn score(&self, c: &Candidate) -> i32;
}
```

- Each signal is one struct holding its own numbers, e.g.
  `Modified { max_boost, half_life_days }`.
- `default_scorers()` builds the list. Every weight is tuned there.
- Match quality is a scorer too (`Quality { weight }`), so its weight sits
  in the same list.
- `name()` is for tuning: a debug view can show each scorer's
  contribution, e.g. `readme.md: quality 420, modified 31`.
- **Scale:** quality covers 0–1000 after weighting, and each boost is
  capped at about 100, so match quality dominates unless a boost is large.
- **Tier jumping isn't in the trait.** Scorers only change the score within
  a tier. The history matcher decides a history hit's tier, including jumps.
- `Entry` fields are private, so `Index` needs a `modified(id)` accessor.

Open question:

- **Dispatch.** Start with `Vec<Box<dyn Scorer>>`. Scorers only run on
  entries that matched, but a one-letter query matches hundreds of
  thousands of names, so benchmark that case. If it's slow, give each
  scorer a `max_boost()` and skip scoring candidates that couldn't reach
  the top `limit` even with every boost.

## TODO

- **Multi-word search.** fzf-style, where `foo bar` matches names
  containing both words in any order. Until then, spaces are literal.
- **Accent folding**, so `cafe` matches `café`.
- **camelCase bonus** in subsequence matching, using the display names.

## Build order

One commit each, about 200 lines or fewer, each building and passing tests:

1. A `search_time` example and benchmark baseline, using a naive
   `contains` scan as the reference.
2. Exact, prefix and subsequence tiers for one `Index`, keeping the top
   `limit` results.
3. Match quality scoring.
4. Search over `&[Index]`, with results as (index, id).
5. Cancellation.
6. The Levenshtein pass.
7. The `Scorer` trait and the modified-time scorer.
8. Path mode.
9. The previous-query cache.
10. History and frecency, including tier jumping.

Streaming and the search thread come once the app crate exists. History
storage and skipping folders like `.git` and `node_modules` during the walk
are decided when we reach them.
