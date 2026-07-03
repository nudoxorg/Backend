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
	let _ = results;
	todo!("group by (name.canonical, version.canonical), keep max score, collect alternates")
}
