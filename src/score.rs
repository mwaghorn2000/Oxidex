// Ranking signals. Each signal is one `Scorer`, and `search` adds up every
// scorer's points to rank hits within a tier. See RANKING.md, "Design".
//
// To add a signal: write a struct holding its tunable numbers, implement
// `Scorer` for it, and add it to `default_scorers`. Nothing in search.rs
// changes. To tune a signal, change its numbers in `default_scorers`.
//
// Scale: match quality is 0 to 1000 and each boost is capped at about 100,
// so match quality decides unless a boost is large.
use crate::index::Index;
use crate::search::Tier;

// Everything a scorer may look at for one matched entry. New fields can be
// added without changing existing scorers.
pub struct Candidate<'a> {
    pub index: &'a Index,
    pub id: u32,
    pub tier: Tier,
    // Match quality from the matcher, 0 to 1000.
    pub quality: i32,
    // Seconds since 1970. Passed in rather than read from the clock so tests
    // are deterministic and one search uses one time throughout.
    pub now: u64,
}

pub trait Scorer: Send + Sync {
    // Short label for tuning output, e.g. "modified".
    fn name(&self) -> &'static str;
    // Points for this candidate. Higher ranks higher.
    fn score(&self, c: &Candidate) -> i32;
}

// Match quality as points, so its weight is tuned alongside the others.
pub struct Quality {
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

// Newer files rank higher: a file modified just now gets `max_boost`, and the
// boost halves every `half_life_days`.
pub struct Modified {
    pub max_boost: i32,
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

// Every signal and its numbers, in one place.
pub fn default_scorers() -> Vec<Box<dyn Scorer>> {
    vec![
        Box::new(Quality { weight: 1 }),
        Box::new(Modified {
            max_boost: 50,
            half_life_days: 30,
        }),
    ]
}

// The sum of every scorer's points for `c`.
pub fn total(scorers: &[Box<dyn Scorer>], c: &Candidate) -> i32 {
    let mut sum = 0;
    for scorer in scorers {
        sum += scorer.score(c);
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
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
            now: NOW,
        }
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
}
