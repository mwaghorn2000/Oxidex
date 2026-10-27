use crate::index::Index;

// Ordered best first, so sorting by tier puts exact matches on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Exact,
    Prefix,
    Subsequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub id: u32,
    pub tier: Tier,
}

// Fills `out` with up to `limit` hits for `query`, best tier first. An exact
// match is the whole name or the name without its extension. Matching is
// case-insensitive.
pub fn search(_index: &Index, _query: &str, _limit: usize, _out: &mut Vec<Hit>) {
    todo!()
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
}
