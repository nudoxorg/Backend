//! Symmetric-delete spelling index for did-you-mean / zero-hit repair.
//!
//! Implements a subset of the **SymSpell** algorithm (Garbe 2012): at build
//! time every vocabulary word and all its deletes up to `max_distance` are
//! inserted into a hash map pointing back to the original word.  At query time
//! the same delete-expansion is generated for the query, each generated key is
//! looked up in the map, and the union of candidate words is verified with a
//! real Damerau–Levenshtein distance computation (with transpositions).
//!
//! # Complexity
//!
//! | Phase  | Time                 | Space        |
//! |--------|----------------------|--------------|
//! | Build  | O(N · L²)            | O(N · L²)    |
//! | Query  | O(L² + candidates)   | O(L²) stack  |
//!
//! where N is vocabulary size and L is word length.  Words longer than
//! [`MAX_WORD_BYTES`] are skipped at build time to bound worst-case blowup.
//!
//! # Pureness
//!
//! No I/O, no clocks, no randomness.  Given the same inputs the same
//! suggestions are always returned in the same order.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;

/// Maximum word length (bytes) indexed.  Words longer than this are silently
/// skipped to avoid combinatorial blowup in the delete map.
const MAX_WORD_BYTES: usize = 40;

/// A symmetric-delete spelling index over a name vocabulary (SymSpell).
///
/// Build cost O(N · L²); lookup is a handful of hash probes — no linear scan.
pub struct SpellIndex {
    /// Map from a delete-variant → list of indices into `names`.
    deletes: HashMap<SmolStr, Vec<u32>>,
    /// The lowercased vocabulary in insertion order (duplicates removed).
    names: Vec<SmolStr>,
    /// Maximum edit distance used at both build and query time.
    max_distance: u8,
}

impl SpellIndex {
    /// Build a `SpellIndex` from a vocabulary.
    ///
    /// Names are lowercased; duplicates are collapsed (first occurrence wins).
    /// `max_distance` is clamped to `1..=2`.
    pub fn build(names: impl IntoIterator<Item = impl AsRef<str>>, max_distance: u8) -> Self {
        let max_distance = max_distance.clamp(1, 2);

        let mut seen: HashMap<SmolStr, u32> = HashMap::new();
        let mut vocab: Vec<SmolStr> = Vec::new();

        for raw in names {
            let lower = SmolStr::new(raw.as_ref().to_lowercase());
            if seen.contains_key(&lower) {
                continue;
            }
            let idx = vocab.len() as u32;
            seen.insert(lower.clone(), idx);
            vocab.push(lower);
        }

        let mut deletes: HashMap<SmolStr, Vec<u32>> = HashMap::new();

        for (idx, name) in vocab.iter().enumerate() {
            if name.len() > MAX_WORD_BYTES {
                continue;
            }
            let idx = idx as u32;
            // Register the word itself so exact-hit probing works via the same map.
            deletes.entry(name.clone()).or_default().push(idx);
            // Generate all deletes up to max_distance.
            for variant in generate_deletes(name.as_str(), max_distance) {
                let entry = deletes.entry(SmolStr::new(variant)).or_default();
                if !entry.contains(&idx) {
                    entry.push(idx);
                }
            }
        }

        Self {
            deletes,
            names: vocab,
            max_distance,
        }
    }

    /// Suggestions for `term` within the index's `max_distance`, best first.
    ///
    /// Ordered by (true Damerau–Levenshtein distance asc, name asc), capped at
    /// `limit`.  An exact vocabulary hit returns just that name (no further
    /// candidates are considered).  Returns an empty `Vec` when no candidate
    /// falls within the configured distance.
    pub fn suggest(&self, term: &str, limit: usize) -> Vec<&str> {
        if limit == 0 {
            return Vec::new();
        }

        let query = term.to_lowercase();

        // Fast path: exact hit — return it alone.
        if let Some(candidates) = self.deletes.get(query.as_str()) {
            for &idx in candidates {
                if self.names[idx as usize].as_str() == query.as_str() {
                    return vec![self.names[idx as usize].as_str()];
                }
            }
        }

        // Collect candidate indices via delete-expansion of the query. The
        // candidate set is tiny relative to the vocabulary, so dedupe with a
        // small hash set rather than a vocabulary-sized bitmap.
        let mut seen_idx: HashSet<u32> = HashSet::new();
        let mut candidates: Vec<(u8, &str)> = Vec::new(); // (distance, name)

        let mut query_variants = generate_deletes(query.as_str(), self.max_distance);
        query_variants.insert(query.clone());

        for variant in &query_variants {
            if let Some(indices) = self.deletes.get(variant.as_str()) {
                for &idx in indices {
                    if !seen_idx.insert(idx) {
                        continue;
                    }
                    let candidate_name = self.names[idx as usize].as_str();
                    // Cheap length band before the O(L²) verification.
                    if candidate_name.len().abs_diff(query.len()) > self.max_distance as usize {
                        continue;
                    }
                    let dist = damerau_levenshtein(query.as_str(), candidate_name);
                    if dist <= self.max_distance as usize {
                        candidates.push((dist as u8, candidate_name));
                    }
                }
            }
        }

        // Sort: distance asc, then name asc (deterministic).
        candidates.sort_unstable_by(|(da, na), (db, nb)| da.cmp(db).then_with(|| na.cmp(nb)));
        candidates.truncate(limit);
        candidates.into_iter().map(|(_, name)| name).collect()
    }

    /// Like [`suggest`], but each entry is `(name, damerau_levenshtein_distance)`.
    ///
    /// Exact vocabulary hits return distance `0`. Empty when nothing is within
    /// `max_distance`.
    pub fn suggest_with_distance(&self, term: &str, limit: usize) -> Vec<(&str, u8)> {
        if limit == 0 {
            return Vec::new();
        }

        let query = term.to_lowercase();

        if let Some(candidates) = self.deletes.get(query.as_str()) {
            for &idx in candidates {
                if self.names[idx as usize].as_str() == query.as_str() {
                    return vec![(self.names[idx as usize].as_str(), 0)];
                }
            }
        }

        let mut seen_idx: HashSet<u32> = HashSet::new();
        let mut candidates: Vec<(u8, &str)> = Vec::new();

        let mut query_variants = generate_deletes(query.as_str(), self.max_distance);
        query_variants.insert(query.clone());

        for variant in &query_variants {
            if let Some(indices) = self.deletes.get(variant.as_str()) {
                for &idx in indices {
                    if !seen_idx.insert(idx) {
                        continue;
                    }
                    let candidate_name = self.names[idx as usize].as_str();
                    if candidate_name.len().abs_diff(query.len()) > self.max_distance as usize {
                        continue;
                    }
                    let dist = damerau_levenshtein(query.as_str(), candidate_name);
                    if dist <= self.max_distance as usize {
                        candidates.push((dist as u8, candidate_name));
                    }
                }
            }
        }

        candidates.sort_unstable_by(|(da, na), (db, nb)| da.cmp(db).then_with(|| na.cmp(nb)));
        candidates.truncate(limit);
        candidates.into_iter().map(|(d, name)| (name, d)).collect()
    }
}

/// Owned-string wrapper around [`SpellIndex::suggest`].
#[must_use]
pub fn suggest_names(spell: &SpellIndex, query: &str, limit: usize) -> Vec<String> {
    spell
        .suggest(query, limit)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// Build a temporary [`SpellIndex`] from `vocab` (max distance 2) and return
/// did-you-mean suggestions for `query`. Pure helper for zero-hit repair UIs.
#[must_use]
pub fn dym_suggestions(
    vocab: impl IntoIterator<Item = impl AsRef<str>>,
    query: &str,
    limit: usize,
) -> Vec<String> {
    let index = SpellIndex::build(vocab, 2);
    suggest_names(&index, query, limit)
}

/// Optional auto-repair for zero-hit queries: when there is a **unique best**
/// suggestion at Damerau–Levenshtein distance **exactly 1**, return it.
///
/// Returns `None` when:
/// - the query is empty or multi-token (whitespace),
/// - there is an exact vocabulary hit,
/// - the best suggestion is not distance 1,
/// - two or more suggestions share distance 1 (ambiguous).
///
/// Callers should only apply this after a zero-hit retrieve; this function does
/// not inspect hit counts itself.
#[must_use]
pub fn maybe_repair_query(spell: &SpellIndex, query: &str) -> Option<String> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    // Multi-token free text is not auto-repaired (would need per-term repair).
    if q.split_whitespace().count() != 1 {
        return None;
    }

    let ranked = spell.suggest_with_distance(q, 3);
    if ranked.is_empty() {
        return None;
    }

    let (best, best_dist) = ranked[0];
    if best_dist == 0 {
        // Exact hit — nothing to repair.
        return None;
    }
    if best_dist != 1 {
        return None;
    }
    // Ambiguous: another distance-1 candidate exists.
    if ranked.get(1).is_some_and(|(_, d)| *d == 1) {
        return None;
    }

    Some(best.to_owned())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Generate all unique single-character deletes of `word` up to depth
/// `max_distance` by recursive deletion.  Does not include `word` itself.
fn generate_deletes(word: &str, max_distance: u8) -> HashSet<String> {
    let mut results: HashSet<String> = HashSet::new();
    generate_deletes_inner(word, 0, max_distance, &mut results);
    results
}

fn generate_deletes_inner(word: &str, depth: u8, max: u8, out: &mut HashSet<String>) {
    if depth >= max {
        return;
    }
    let chars: Vec<char> = word.chars().collect();
    for i in 0..chars.len() {
        let mut variant: Vec<char> = chars.clone();
        variant.remove(i);
        let s: String = variant.iter().collect();
        if out.insert(s.clone()) {
            // Only recurse on first sight — deeper deletes of a repeated variant
            // are already covered.
            generate_deletes_inner(&s, depth + 1, max, out);
        }
    }
}

/// Optimal string alignment (restricted Damerau–Levenshtein) with
/// transpositions. Callers bound the cost with a length-difference band before
/// invoking; inputs here are already capped at [`MAX_WORD_BYTES`].
///
/// Returns the edit distance between `a` and `b`.
fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let la = a.len();
    let lb = b.len();

    if la == 0 {
        return lb;
    }
    if lb == 0 {
        return la;
    }

    // d[i][j] = edit distance between a[..i] and b[..j]
    let mut d: Vec<Vec<usize>> = vec![vec![0usize; lb + 1]; la + 1];

    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=la {
        for j in 1..=lb {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1) // delete
                .min(d[i][j - 1] + 1) // insert
                .min(d[i - 1][j - 1] + cost); // replace

            // Transposition (Damerau extension).
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + cost);
            }
        }
    }

    d[la][lb]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn index(words: &[&str]) -> SpellIndex {
        SpellIndex::build(words.iter().copied(), 2)
    }

    // ── Distance-1 transposition ──────────────────────────────────────────────

    /// "tokyo" → "tokio" is a transposition (distance 1, not 2).
    #[test]
    fn transposition_tokyo_to_tokio() {
        let idx = index(&["tokio", "serde", "requests"]);
        let suggestions = idx.suggest("tokyo", 5);
        assert!(
            suggestions.contains(&"tokio"),
            "expected 'tokio' in suggestions for 'tokyo', got: {suggestions:?}"
        );
    }

    // ── Deletion at distance 1 ────────────────────────────────────────────────

    /// "serd" → "serde" (one insertion / deletion gap, distance 1).
    #[test]
    fn deletion_serd_to_serde() {
        let idx = index(&["serde", "tokio", "reqwest"]);
        let suggestions = idx.suggest("serd", 5);
        assert!(
            suggestions.contains(&"serde"),
            "expected 'serde' in suggestions for 'serd', got: {suggestions:?}"
        );
    }

    // ── Distance-2 correction ─────────────────────────────────────────────────

    /// "reqests" → "requests" requires 2 edits (insert 'u', distance 2).
    #[test]
    fn distance2_reqests_to_requests() {
        let idx = index(&["requests", "serde", "tokio"]);
        let suggestions = idx.suggest("reqests", 5);
        assert!(
            suggestions.contains(&"requests"),
            "expected 'requests' in suggestions for 'reqests', got: {suggestions:?}"
        );
    }

    // ── Exact hit short-circuits to just that name ────────────────────────────

    #[test]
    fn exact_hit_returns_only_exact_match() {
        let idx = index(&["serde", "serdex", "ser"]);
        let suggestions = idx.suggest("serde", 10);
        // Exact hit: should return exactly ["serde"].
        assert_eq!(suggestions, vec!["serde"]);
    }

    // ── Beyond max_distance returns nothing ───────────────────────────────────

    #[test]
    fn no_suggestion_beyond_max_distance() {
        // "abcdef" vs "xyz" — distance far exceeds 2.
        let idx = index(&["xyz"]);
        let suggestions = idx.suggest("abcdef", 5);
        assert!(
            suggestions.is_empty(),
            "should return no suggestion for a distance > 2, got: {suggestions:?}"
        );
    }

    // ── Deterministic ordering ────────────────────────────────────────────────

    #[test]
    fn ordering_is_deterministic() {
        let vocab = &["tokio", "tokyio", "tok", "serde"];
        let idx = index(vocab);
        let r1 = idx.suggest("tokio", 10);
        let r2 = idx.suggest("tokio", 10);
        assert_eq!(r1, r2, "repeated calls must return identical results");
    }

    /// When multiple candidates share the same edit distance, they must come
    /// back sorted alphabetically (name asc).
    #[test]
    fn ties_broken_alphabetically() {
        // All single-char edits of "ser": serd, sere, serf are all distance-1.
        let idx = index(&["sera", "serb", "serc"]);
        let suggestions = idx.suggest("ser", 5);
        // Every suggestion is exactly one insertion from "ser" → distance 1.
        let mut expected = suggestions.clone();
        expected.sort_unstable();
        assert_eq!(
            suggestions, expected,
            "tie-broken suggestions should be in alphabetical order"
        );
    }

    // ── Empty vocabulary ──────────────────────────────────────────────────────

    #[test]
    fn empty_vocab_returns_no_suggestions() {
        let idx = SpellIndex::build(std::iter::empty::<&str>(), 2);
        let suggestions = idx.suggest("anything", 5);
        assert!(suggestions.is_empty());
    }

    // ── Case-folding ──────────────────────────────────────────────────────────

    #[test]
    fn case_fold_query_and_vocab() {
        let idx = index(&["Tokio", "SERDE"]);
        // Vocab is lowercased on insert; query is lowercased on lookup.
        let s = idx.suggest("tokio", 1);
        assert_eq!(s, vec!["tokio"]);
    }

    // ── Limit ────────────────────────────────────────────────────────────────

    #[test]
    fn limit_caps_results() {
        // Many single-char edits from "se": sea, seb, sec, ...
        let vocab: Vec<String> = (b'a'..=b'z').map(|c| format!("se{}", c as char)).collect();
        let vocab_refs: Vec<&str> = vocab.iter().map(String::as_str).collect();
        let idx = index(&vocab_refs);
        let suggestions = idx.suggest("sec", 3);
        assert!(suggestions.len() <= 3, "limit must be respected");
    }

    // ── Damerau–Levenshtein unit tests ───────────────────────────────────────

    #[test]
    fn dl_distance_exact() {
        assert_eq!(damerau_levenshtein("abc", "abc"), 0);
    }

    #[test]
    fn dl_distance_transposition() {
        // "ab" → "ba" is one transposition.
        assert_eq!(damerau_levenshtein("ab", "ba"), 1);
    }

    #[test]
    fn dl_distance_insertion() {
        assert_eq!(damerau_levenshtein("abc", "abcd"), 1);
    }

    #[test]
    fn dl_distance_deletion() {
        assert_eq!(damerau_levenshtein("abcd", "abc"), 1);
    }

    #[test]
    fn dl_distance_substitution() {
        assert_eq!(damerau_levenshtein("abc", "axc"), 1);
    }

    #[test]
    fn dl_distance_empty_strings() {
        assert_eq!(damerau_levenshtein("", ""), 0);
        assert_eq!(damerau_levenshtein("abc", ""), 3);
        assert_eq!(damerau_levenshtein("", "abc"), 3);
    }

    // ── DYM / suggest_names / maybe_repair_query ─────────────────────────────

    /// Vocabulary with serde suggests for distance-1 typos `serda` / `serd`.
    #[test]
    fn dym_suggests_serde_for_serda_and_serd() {
        let for_serda = dym_suggestions(["serde", "tokio", "reqwest"], "serda", 5);
        assert!(
            for_serda.iter().any(|s| s == "serde"),
            "expected serde for serda, got: {for_serda:?}"
        );
        let for_serd = dym_suggestions(["serde", "tokio", "reqwest"], "serd", 5);
        assert!(
            for_serd.iter().any(|s| s == "serde"),
            "expected serde for serd, got: {for_serd:?}"
        );
    }

    /// Close edit must rank above an unrelated long name.
    #[test]
    fn close_edit_preferred_over_unrelated_long_name() {
        let idx = index(&["serde", "serialization-framework-helper"]);
        let suggestions = idx.suggest("serda", 5);
        assert!(
            !suggestions.is_empty(),
            "expected at least one suggestion for serda"
        );
        assert_eq!(
            suggestions[0], "serde",
            "distance-1 'serde' must rank before long unrelated name, got: {suggestions:?}"
        );
        // Long name is far beyond max_distance from "serda" → should not appear.
        assert!(
            !suggestions.contains(&"serialization-framework-helper"),
            "unrelated long name must not appear in distance-bounded suggestions: {suggestions:?}"
        );
    }

    #[test]
    fn suggest_names_returns_owned_strings() {
        let idx = index(&["serde", "tokio"]);
        let owned = suggest_names(&idx, "serd", 3);
        assert!(owned.iter().any(|s| s == "serde"));
    }

    #[test]
    fn maybe_repair_unique_distance1() {
        let idx = index(&["serde", "tokio"]);
        // "serd" → unique distance-1 repair to "serde".
        assert_eq!(maybe_repair_query(&idx, "serd").as_deref(), Some("serde"));
    }

    #[test]
    fn maybe_repair_skips_exact_hit() {
        let idx = index(&["serde"]);
        assert_eq!(maybe_repair_query(&idx, "serde"), None);
    }

    #[test]
    fn maybe_repair_skips_ambiguous_distance1() {
        // "ser" → sera and serb both distance 1 → ambiguous.
        let idx = index(&["sera", "serb"]);
        assert_eq!(maybe_repair_query(&idx, "ser"), None);
    }

    #[test]
    fn maybe_repair_skips_multi_token() {
        let idx = index(&["serde"]);
        assert_eq!(maybe_repair_query(&idx, "serd json"), None);
    }

    #[test]
    fn maybe_repair_skips_distance2_only() {
        // "requess" → "requests" is two edits (t insert + t→s or similar);
        // auto-repair only fires for a *unique* distance-1 suggestion.
        // "reqests" is Damerau distance 1 from "requests" (transpose/insert)
        // on some implementations — use a clearly distance-2 typo.
        let idx = index(&["requests"]);
        assert_eq!(maybe_repair_query(&idx, "reqqess"), None);
    }
}
