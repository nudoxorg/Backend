//! Defines score behavior for `interface-search`, whose purpose is to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! This module owns the score invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One deterministic name-match score every lane uses, so lanes rank the same name the same way.

use crate::Score;

/// Score of an exact, case-sensitive name match.
pub const EXACT_NAME_SCORE: u32 = 1_000;
/// Score of a case-insensitive name match.
pub const FOLDED_NAME_SCORE: u32 = 900;
/// Score of a case-sensitive prefix match before its length penalty.
pub const PREFIX_NAME_SCORE: u32 = 800;
/// Score of a case-insensitive prefix match before its length penalty.
pub const FOLDED_PREFIX_SCORE: u32 = 700;
/// Score of an interior substring match before its length penalty.
pub const CONTAINED_NAME_SCORE: u32 = 500;

/// How well one declaration name answers one query term.
///
/// `None` means the name does not answer the query at all — never a zero score, because a zero
/// score is a row and a row is a claim. The penalty is the extra bytes the name carries beyond the
/// query, so `map` outranks `map_or_else` for the query `map` in every lane identically.
#[must_use]
pub fn name_score(query: &str, name: &str) -> Option<Score> {
    if query.is_empty() || name.is_empty() {
        return None;
    }
    let overflow = name.len().saturating_sub(query.len());
    let penalty = u32::try_from(overflow).unwrap_or(u32::MAX).min(255);
    if name == query {
        return Some(Score(EXACT_NAME_SCORE));
    }
    if name.eq_ignore_ascii_case(query) {
        return Some(Score(FOLDED_NAME_SCORE));
    }
    if name.starts_with(query) {
        return Some(Score(PREFIX_NAME_SCORE.saturating_sub(penalty)));
    }
    if folded_starts_with(name, query) {
        return Some(Score(FOLDED_PREFIX_SCORE.saturating_sub(penalty)));
    }
    if name.contains(query) {
        return Some(Score(CONTAINED_NAME_SCORE.saturating_sub(penalty)));
    }
    None
}

/// Case-insensitive prefix test over ASCII, without allocating a folded copy of either operand.
fn folded_starts_with(name: &str, query: &str) -> bool {
    let mut name_bytes = name.bytes();
    query
        .bytes()
        .all(|wanted| name_bytes.next().is_some_and(|have| have.eq_ignore_ascii_case(&wanted)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closer_names_outrank_longer_ones_and_misses_are_absent() {
        assert_eq!(name_score("map", "map"), Some(Score(EXACT_NAME_SCORE)));
        assert_eq!(name_score("map", "MAP"), Some(Score(FOLDED_NAME_SCORE)));
        let close = name_score("map", "map_or").unwrap_or_default();
        let far = name_score("map", "map_or_else").unwrap_or_default();
        assert!(close > far, "{close:?} should outrank {far:?}");
        assert!(name_score("map", "Map_Or").unwrap_or_default() < close);
        assert_eq!(name_score("map", "collect"), None);
        assert_eq!(name_score("", "map"), None);
    }
}
