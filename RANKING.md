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
2. **Match quality** within the tier.
3. **Scoring boosts** (below).

Boosts only reorder results within a tier. Match quality wins in normal
cases. The one exception is tier jumping, below.

## Matching rules

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
