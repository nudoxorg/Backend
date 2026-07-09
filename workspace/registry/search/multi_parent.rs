//! Multi-parent de-duplication.
//!
//! The same logical package can be reachable through more than one parent —
//! several federated [`RegistryOrigin`]s, a mirror plus the public index, a
//! vendored fork — producing multiple distinct [`GlobalPackage`] records that a
//! naive search would surface as duplicates. This module folds that fan-in into
//! a single coherent [`Scored`] view, so callers never see the multi-parent
//! structure.
//!
//! The merge is *identity-aware*: records that share a canonical
//! ([`PackageName`], [`PackageVersion`]) are collapsed even when their
//! [`PackageId`]s differ (because origin is part of the id), keeping the
//! highest-scored representative and recording the alternates as provenance.

use heart::{
	Scored,
	identity::{PackageId, RegistryOrigin},
};

use crate::GlobalPackage;

/// A merged search result: one representative package plus the alternate
/// origins the same logical package was also reachable through.
#[derive(Debug, Clone)]
pub struct Merged {
	/// The highest-scored representative of the logical package.
	pub representative: Scored<GlobalPackage>,

	/// The other origins the same (name, version) was reachable through, for
	/// provenance / "also available from" surfacing.
	pub alternates: Vec<Alternate>,
}

/// One alternate reachability path for a merged package.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alternate {
	/// The alternate origin.
	pub origin: RegistryOrigin,

	/// The distinct package id under that origin.
	pub id: PackageId,
}

/// Collapse a scored, possibly-duplicated result set into de-duplicated
/// [`Merged`] views.
///
/// Groups by canonical `(name, version)` (origin-independent), keeps the
/// best-scored record as representative, and folds the rest into `alternates`.
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
	let provenance = |package: &GlobalPackage| Alternate {
		origin: package.package.coordinates.origin.clone(),
		id: package.id,
	};

	let mut merged: Vec<Merged> = Vec::new();
	let mut groups: HashMap<_, usize> = HashMap::new();

	for scored in results {
		match groups.entry(key(&scored.value)) {
			Entry::Vacant(slot) => {
				slot.insert(merged.len());
				merged.push(Merged { representative: scored, alternates: Vec::new() });
			}
			Entry::Occupied(slot) => {
				let group = &mut merged[*slot.get()];
				// A strictly better score dethrones the representative; ties
				// keep the earlier arrival so pagination stays deterministic.
				if scored.score > group.representative.score {
					let dethroned = std::mem::replace(&mut group.representative, scored);
					group.alternates.push(provenance(&dethroned.value));
				} else {
					group.alternates.push(provenance(&scored.value));
				}
			}
		}
	}
	merged
}
