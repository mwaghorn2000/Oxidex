//! Ranking signals. Each signal is one `Scorer`, and `search` adds up every
//! scorer's points to rank hits within a tier. See RANKING.md, "Design".
//!
//! To add a signal: write a struct holding its tunable numbers, implement
//! `Scorer` for it, and add it to `default_scorers`. Nothing in search.rs
//! changes. To tune a signal, change its numbers in `default_scorers`.
//!
//! Scale: match quality is 0 to 1000 and each boost is capped at about 100,
//! so match quality decides unless a boost is large.
use crate::history::Record;
use crate::index::Index;
use crate::matching::Tier;

/// Everything a scorer may look at for one matched entry. New fields can be
/// added without changing existing scorers.
pub struct Candidate<'a> {
    /// The index the entry is in.
    pub index: &'a Index,
    /// Entry ID within `index`.
    pub id: u32,
    /// The tier the name matched in.
    pub tier: Tier,
    /// Match quality from the matcher, 0 to 1000.
    pub quality: i32,
    /// The hit's history record, if the user has opened it through Oxidex.
    pub history: Option<&'a Record>,
    /// Seconds since 1970. Passed in rather than read from the clock so tests
    /// are deterministic and one search uses one time throughout.
    pub now: u64,
}

/// One ranking signal.
pub trait Scorer: Send + Sync {
    /// Short label for tuning output, e.g. "modified".
    fn name(&self) -> &'static str;
    /// Points for this candidate. Higher ranks higher.
    fn score(&self, c: &Candidate) -> i32;
}

/// Match quality as points, so its weight is tuned alongside the others.
pub struct Quality {
    /// Points per point of match quality.
    pub weight: i32,
}

impl Scorer for Quality {
    fn name(&self) -> &'static str {
        "quality"
    }

    fn score(&self, c: &Candidate) -> i32 {
        c.quality * self.weight
    }
}

/// Newer files rank higher: a file modified just now gets `max_boost`, and the
/// boost halves every `half_life_days`. Applies to opened files too, with
/// `Frecency` added on top, so opening a file can only raise its score.
pub struct Modified {
    /// Points for a file modified just now.
    pub max_boost: i32,
    /// Days for the boost to halve.
    pub half_life_days: u64,
}

impl Scorer for Modified {
    fn name(&self) -> &'static str {
        "modified"
    }

    fn score(&self, c: &Candidate) -> i32 {
        let Some(modified) = c.index.modified(c.id) else {
            return 0;
        };
        // 0 means the OS couldn't give a time, and a time in the future means
        // a wrong clock, not a new file. Neither earns a boost.
        if modified == 0 || modified > c.now {
            return 0;
        }
        let age_days = (c.now - modified) as f64 / 86_400.0;
        let half_lives = age_days / self.half_life_days as f64;
        (self.max_boost as f64 * 0.5_f64.powf(half_lives)).round() as i32
    }
}

/// Files opened often and lately rank higher: `points_per_open` for each
/// open, decayed by `Record::frecency`, up to `max_boost`. See RANKING.md,
/// "Frecency".
pub struct Frecency {
    /// Points per open, before decay.
    pub points_per_open: i32,
    /// Most points a file can get.
    pub max_boost: i32,
}

impl Scorer for Frecency {
    fn name(&self) -> &'static str {
        "frecency"
    }

    fn score(&self, c: &Candidate) -> i32 {
        let Some(record) = c.history else {
            return 0;
        };
        let points = record.frecency(c.now) * self.points_per_open as f64;
        (points.round() as i32).min(self.max_boost)
    }
}

/// Every signal and its numbers, in one place.
pub fn default_scorers() -> Vec<Box<dyn Scorer>> {
    vec![
        Box::new(Quality { weight: 1 }),
        Box::new(Modified {
            max_boost: 50,
            half_life_days: 30,
        }),
        Box::new(Frecency {
            points_per_open: 25,
            max_boost: 100,
        }),
    ]
}

// The sum of every scorer's points for `c`.
pub(crate) fn total(scorers: &[Box<dyn Scorer>], c: &Candidate) -> i32 {
    let mut sum = 0;
    for scorer in scorers {
        sum += scorer.score(c);
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::index::NO_PARENT;

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_000 * DAY;

    fn index_modified_at(modified: u64) -> Index {
        let mut index = Index::new();
        index.push("a.txt", NO_PARENT, false, 0, modified).unwrap();
        index
    }

    fn candidate(index: &Index, quality: i32) -> Candidate<'_> {
        Candidate {
            index,
            id: 0,
            tier: Tier::Exact,
            quality,
            history: None,
            now: NOW,
        }
    }

    // A record whose score is `score` as of `NOW`.
    fn record(score: f64) -> Record {
        let json = format!(
            r#"{{"records": [{{"path": "/a.txt", "score": {score}, "counted_at": {NOW}}}]}}"#
        );
        History::read_from(json.as_bytes()).unwrap().records()[0].clone()
    }

    fn frecency_boost(record: &Record, now: u64) -> i32 {
        let index = index_modified_at(0);
        let c = Candidate {
            history: Some(record),
            now,
            ..candidate(&index, 0)
        };
        let scorer = Frecency {
            points_per_open: 25,
            max_boost: 100,
        };
        scorer.score(&c)
    }

    fn modified_boost(modified: u64) -> i32 {
        let index = index_modified_at(modified);
        let scorer = Modified {
            max_boost: 100,
            half_life_days: 30,
        };
        scorer.score(&candidate(&index, 0))
    }

    #[test]
    fn quality_applies_weight() {
        let index = index_modified_at(0);
        let scorer = Quality { weight: 3 };
        assert_eq!(scorer.score(&candidate(&index, 200)), 600);
    }

    #[test]
    fn modified_just_now_gets_max_boost() {
        assert_eq!(modified_boost(NOW), 100);
    }

    #[test]
    fn modified_boost_halves_each_half_life() {
        assert_eq!(modified_boost(NOW - 30 * DAY), 50);
        assert_eq!(modified_boost(NOW - 60 * DAY), 25);
    }

    // A time in the future means a wrong clock, not a new file.
    #[test]
    fn modified_in_future_gets_nothing() {
        assert_eq!(modified_boost(NOW + DAY), 0);
    }

    // 0 is what the walk stores when the OS can't give a time.
    #[test]
    fn unknown_modified_time_gets_nothing() {
        assert_eq!(modified_boost(0), 0);
    }

    // A scorer defined only here, to show a new signal plugs in with no other
    // changes.
    struct Fixed(i32);

    impl Scorer for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }

        fn score(&self, _c: &Candidate) -> i32 {
            self.0
        }
    }

    #[test]
    fn total_adds_every_scorer() {
        let index = index_modified_at(0);
        let scorers: Vec<Box<dyn Scorer>> =
            vec![Box::new(Quality { weight: 2 }), Box::new(Fixed(7))];
        assert_eq!(total(&scorers, &candidate(&index, 100)), 207);
    }

    #[test]
    fn total_of_no_scorers_is_zero() {
        let index = index_modified_at(0);
        assert_eq!(total(&[], &candidate(&index, 100)), 0);
    }

    // On the default scale, a clearly better match beats a brand new file.
    #[test]
    fn better_match_beats_newest_file() {
        let scorers = default_scorers();
        let old = index_modified_at(0);
        let new = index_modified_at(NOW);
        let better = total(&scorers, &candidate(&old, 1000));
        let newer = total(&scorers, &candidate(&new, 900));
        assert!(better > newer, "{better} vs {newer}");
    }

    #[test]
    fn frecency_gives_points_per_open() {
        assert_eq!(frecency_boost(&record(1.0), NOW), 25);
        assert_eq!(frecency_boost(&record(2.0), NOW), 50);
    }

    #[test]
    fn frecency_is_capped() {
        assert_eq!(frecency_boost(&record(9.0), NOW), 100);
    }

    #[test]
    fn frecency_decays_with_time() {
        assert_eq!(frecency_boost(&record(2.0), NOW + 7 * DAY), 25);
    }

    #[test]
    fn no_history_gets_no_frecency() {
        let index = index_modified_at(0);
        let scorer = Frecency {
            points_per_open: 25,
            max_boost: 100,
        };
        assert_eq!(scorer.score(&candidate(&index, 0)), 0);
    }

    // Opening a recently modified file must not trade its modified boost
    // for a smaller frecency one, even once the open has decayed.
    #[test]
    fn opening_a_file_never_lowers_its_score() {
        let scorers = default_scorers();
        let index = index_modified_at(NOW);
        let unopened = total(&scorers, &candidate(&index, 800));
        for open in [record(1.0), record(0.01)] {
            let c = Candidate {
                history: Some(&open),
                ..candidate(&index, 800)
            };
            let opened = total(&scorers, &c);
            assert!(opened >= unopened, "{opened} vs {unopened}");
        }
    }

    // On the default scale, a clearly better match beats a file opened often.
    #[test]
    fn better_match_beats_most_opened_file() {
        let scorers = default_scorers();
        let index = index_modified_at(0);
        let opened = record(9.0);
        let often = Candidate {
            history: Some(&opened),
            ..candidate(&index, 800)
        };
        let better = total(&scorers, &candidate(&index, 1000));
        let often = total(&scorers, &often);
        assert!(better > often, "{better} vs {often}");
    }
}
