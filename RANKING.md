# Ranking

How search results are ordered. This is the plan for `search.rs`; none of
it is built yet. The numbers are starting points to tune with real use.

## Input

Each search directory has its own `Index`, so search takes `&[Index]` and
identifies a result as (index number, entry id). Directories nested inside
another listed directory are dropped, so no file is indexed twice.

## Order

Results are sorted by, in order:

1. **Tier**: exact, then prefix, then subsequence (fzf-style), then
   Levenshtein.
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
- The Levenshtein tier is capped at 2 edits, and it skips names whose
  length difference rules them out.
- Levenshtein compares against the name's stem and ignores the extension.
- On the main index, Levenshtein only runs when earlier tiers return few
  results.
- **Open history** is searched with every tier, including Levenshtein,
  even when earlier tiers already return many results. It's small, so this
  is cheap.
- For history entries, typos match while typing: the query is compared
  against the start of the stem (prefix edit distance), so `slar` matches
  `slack`.

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
2. **Exact, prefix and subsequence** matches from the main index.
3. **Levenshtein** matches, only when that pass runs.

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
- Levenshtein matches don't narrow this way, so the fuzzy pass always
  scans in full. It only runs when earlier tiers return few results.

## Design

Scoring must be modular: adding a new signal or adjusting an old one
shouldn't touch the rest of the search code.

- Scoring is a trait, with one implementation per signal (frecency,
  modified time, ...).
- Each implementation is self-contained and keeps its tunable numbers in
  one obvious place.
- Adding a signal means writing a new implementation and registering it.

Open questions:

- **Dispatch.** `Vec<Box<dyn Scorer>>` is the most flexible but adds a
  virtual call per candidate. Calling a fixed set of scorers directly
  avoids that. Benchmark both before choosing.
- **Tier jumping in the trait.** Frecency can move a result into another
  tier, so the trait may need more than a plain score boost.

## TODO

- **Multi-word search.** fzf-style, where `foo bar` matches names
  containing both words in any order. Until then, spaces are literal.
