# Ranking

How search results are ordered: what's built (in `search.rs`,
`matching.rs` and `score.rs`) and what's still planned. The build order at
the end marks which is which. The numbers are starting points to tune with
real use.

## Input

Each search directory has its own `Index`, so search takes `&[Index]` and
identifies a result as (index number, entry id). Directories nested inside
another listed directory are dropped, so no file is indexed twice
(`walk::distinct_roots`). Each folder is made canonical first, and nesting
is checked by whole path components, so `/a/xy` isn't inside `/a/x`.

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
- In path mode (`docs/rea`, or with `\`), each folder part must
  subsequence-match a folder name on the way up the parent chain. Results
  are ranked by the file-name part only.
  - Folder parts match in order, but not necessarily as direct parents:
    `docs/notes` finds `docs/old/notes.txt`, and `a/b/x` needs `b` inside
    `a`.
  - Empty parts are ignored (`/notes`, `docs//notes`), and so are `.` and
    `~` (`./src/x`, `~/projects/x`). An empty file part (`docs/`) returns
    nothing for now.
  - A root is matched by its last part, like any folder. Folder parts left
    over at the root are matched against the folders in the root's own
    path, so a pasted full path works: `/home/me/projects/src/x` with the
    root `/home/me/projects`.
  - Folders are only checked after the file part matches, so path mode
    costs nothing for names that don't.
- Queries of 1–2 characters only match exactly or as a prefix. Almost
  every name contains one or two given letters somewhere, so a subsequence
  match says nothing useful, and scoring them all was slow. Subsequence
  matching starts at 3 characters, typos at 4.
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
- **Opened files** (history) are matched first, in their own phase, by
  `match_history` in `matching.rs`. The full scan then skips them, so each
  appears once. History is small, so it gets two extras that would cost
  too much on every name:
  - **Typos while typing:** if the name doesn't already match in the
    exact, prefix or typo tier, the query is compared against each start
    of the stem within `max_edits` of its own length (prefix edit
    distance), with the usual edit allowance. So `slar` finds `slack` and
    `the binding of iscac` finds `The Binding of Isaac Rebirth.url`. A hit
    goes in the Levenshtein tier, so it beats a subsequence match.
  - **Tier jumping**, below.
  - History has its own mask filter, `may_match_history`, which allows up
    to 2 missing characters, the most a jump can use.

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
- Names and queries are lowercased the same way (`index::fold_case`).
  Greek final sigma `ς` is folded to `σ`, because plain lowercasing turns
  a word-final `Σ` into `ς` and any other `Σ` into `σ`. Without this, the
  query `ΟΔΟΣ` wouldn't be a prefix of `ΟΔΟΣΤΡΩΜΑ`.

## Scoring signals

### Frecency

Oxidex keeps its own history of files opened through search
(`History` in `history.rs`).

- Each open adds +1.
- At most one open counts per 12 hours.
- The score halves every 7 days.
- A last count in the future (a wrong clock) is moved back to now at the
  next open. Otherwise it would block every open until the real clock
  caught up.
- The `Frecency` scorer gives 25 points per open, decayed, up to 100.

The frontend owns the `History`, and core never picks a file location:

1. At startup it loads the history with `History::read_from`.
2. It calls `resolve` after every walk, and after each `record_open`.
3. It saves with `write_to` whenever it likes.

The format is JSON (`{"records": [...]}`). History records paths, because
they last across walks. `resolve` maps them to this walk's entry IDs in one
pass per index, about 5 ms on 61k entries.

### Modified time

Newer files rank higher: up to 50 points, halving every 30 days. This
applies to opened files too, with frecency added on top.

It used to be a fallback for files never opened. But one open is worth 25
points, so opening a recently modified file lowered its rank.

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

A jumped hit goes in the Exact tier with quality `1000 / edits`, so with
its frecency boost it ranks above a stem match nobody opens (900). The
thresholds are `JUMP_FRECENCY` and `MAX_JUMP_EDITS` in `matching.rs`.
They're guesses until there's real history to tune them on.

## Streaming

Results are sent to the popup in phases, so cached results appear on the
first frame and the full scan fills in after:

1. **Cached results**: history matches. (The previous query's results,
   filtered against the new query, are part of the deferred cache below.)
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

**Deferred.** The missing-character filter now does most of this job. The
scan over every entry is just the mask check, about 1 ms on `/home/mitch`
(`zzqxj` takes 1.0 ms in total). Most of a query's time goes on matching
the names that pass the mask, which the cache wouldn't avoid. Revisit only
if a benchmark shows the full mask scan matters. If it's built, cache the
names that passed the mask check rather than the matches: appending
characters can only make more of them go missing, so that set only shrinks
while `max_edits` stays the same. That covers typo matches too. The plan
below predates the filter.

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
- So does going from 2 to 3 characters: 1–2 character queries only match
  exactly or as a prefix, so the previous results don't include the
  subsequence matches that start at 3.
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

In `oxidex-core/src/score.rs`:

```rust
// Everything a scorer may look at for one matched entry. New fields can be
// added without changing existing scorers.
pub struct Candidate<'a> {
    pub index: &'a Index,
    pub id: u32,
    pub tier: Tier,
    pub quality: i32,                 // from the matcher
    pub history: Option<&'a Record>,  // if the user has opened it
    pub now: u64,                     // passed in so tests are deterministic
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

**Dispatch:** `Vec<Box<dyn Scorer>>`. Benchmarked with the quality and
modified-time scorers running on every match: 0.2–0.5 ms per query on
`/home/mitch` (see BENCHMARKS.md), so there's no need for anything more
complicated.

## TODO

- **Typos in a partly typed name lose every result**, except for opened
  files, which get this through `match_history`. `the binding of isc`
  finds `The Binding of Isaac Rebirth.url` as a subsequence match, but one
  more wrong letter (`the binding of iscac`, 1 edit from `isaac`) finds
  nothing. It's no longer a subsequence, and the typo check only compares
  against the whole name or stem: 28 characters against the query's 20, far
  over the 2-edit limit on length alone. Likely fix: the same prefix edit
  distance history uses, for every name, once a benchmark shows it's
  affordable on the full scan. The mask filter stays valid, because each
  missing character still costs at least one edit.

- **Multi-word search.** fzf-style, where `foo bar` matches names
  containing both words in any order. Until then, spaces are literal.
- **Accent folding**, so `cafe` matches `café`.
- **Folder contents for `docs/`**: show what's inside matching folders
  instead of nothing.
- **camelCase bonus** in subsequence matching, using the display names.

## Build order

One commit each, about 200 lines or fewer, each building and passing tests:

1. **Done.** A `search_time` example and benchmark baseline, using a naive
   `contains` scan as the reference.
2. **Done.** Exact, prefix and subsequence tiers for one `Index`, keeping
   the top `limit` results.
3. **Done.** Match quality scoring.
4. **Done.** Search over `&[Index]`, with results as (index, id).
5. **Done.** Cancellation.
6. **Done.** The Levenshtein pass.
7. **Done.** The `Scorer` trait and the modified-time scorer.
8. **Done.** Path mode.
9. ~~The previous-query cache.~~ Deferred, see "Previous-query cache".
10. **Done.** A command-line search (`src/main.rs`), to try search on real
    folders.
11. **Done.** History and frecency, including tier jumping.

Streaming and the search thread come once the app crate exists. History is
stored by the frontend, as above. The walk skips the contents of folders
like `.git` and `node_modules` (the fixed `SKIP_DIRS` list in `walk.rs`),
but still indexes the folder itself.
