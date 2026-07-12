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

use crate::registry::GlobalPackage;

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
	use std::collections::{HashMap, hash_map::Entry};

	// The logical-package key: origin-independent, so federated copies of the
	// same (name, version) fold together.
	let key = |package: &GlobalPackage| {
		let coordinates = &package.package.coordinates;
		(
			coordinates.ecosystem(),
			coordinates.name.canonical().to_owned(),
			coordinates.version.canonical(),
		)
	};

	let mut merged: Vec<Merged> = Vec::new();
	let mut groups: HashMap<_, usize> = HashMap::new();

	for scored in results {
		match groups.entry(key(&scored.value)) {
			Entry::Vacant(slot) => {
				slot.insert(merged.len());
				merged.push(Merged { representative: scored });
			}
			Entry::Occupied(slot) => {
				let group = &mut merged[*slot.get()];
				// A strictly better score dethrones the representative; ties
				// keep the earlier arrival so pagination stays deterministic.
				if scored.score > group.representative.score {
					group.representative = scored;
				}
			}
		}
	}
	merged
}
