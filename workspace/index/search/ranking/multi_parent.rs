//! Multi-parent de-duplication.
//!
//! The same logical package can be reachable through more than one parent —
//! several federated registry origins, a mirror plus the public index, a
//! vendored fork — producing multiple distinct [`GlobalPackage`] records that a
//! naive search would surface as duplicates. This module folds that fan-in into
//! a single coherent [`Scored`] view, so callers never see the multi-parent
//! structure.
//!
//! The merge is *identity-aware*: records that share a canonical
//! `(name, version)` are collapsed even when their package ids differ (because
//! origin is part of the id), keeping the highest-scored representative.

use heart::Scored;

use crate::GlobalPackage;

/// A merged search result: the highest-scored representative of the logical
/// package after de-duplicating multi-parent reachability.
#[derive(Debug, Clone)]
pub struct Merged {
    /// The highest-scored representative of the logical package.
    pub representative: Scored<GlobalPackage>,
}

/// Collapse a scored, possibly-duplicated result set into de-duplicated
/// [`Merged`] views.
///
/// Groups by canonical `(name, version)` (origin-independent) and keeps the
/// best-scored record as representative.
/// Stable: input order breaks ties so pagination stays deterministic.
pub fn merge(results: Vec<Scored<GlobalPackage>>) -> Vec<Merged> {
    let key = |package: &GlobalPackage| {
        let coordinates = &package.package.coordinates;
        (
            coordinates.ecosystem(),
            coordinates.name.canonical().to_owned(),
            coordinates.version.canonical(),
        )
    };
    merge_by(results, key)
        .into_iter()
        .map(|representative| Merged { representative })
        .collect()
}

/// Collapse duplicates by `key_of`. A strictly better score replaces the
/// representative; ties keep the earlier arrival.
pub fn merge_by<T, K, F>(results: Vec<Scored<T>>, mut key_of: F) -> Vec<Scored<T>>
where
    K: Eq + std::hash::Hash,
    F: FnMut(&T) -> K,
{
    use std::collections::{HashMap, hash_map::Entry};

    let mut merged: Vec<Scored<T>> = Vec::new();
    let mut groups: HashMap<K, usize> = HashMap::new();
    for scored in results {
        match groups.entry(key_of(&scored.value)) {
            Entry::Vacant(slot) => {
                slot.insert(merged.len());
                merged.push(scored);
            }
            Entry::Occupied(slot) => {
                let group = &mut merged[*slot.get()];
                if scored.score > group.score {
                    *group = scored;
                }
            }
        }
    }
    merged
}
