//! Corpus-wide reverse-dependency counting — the ecosystem-fair popularity
//! signal (in-degree over the mirror's own manifests; no upstream API).
//!
//! # How the signal reaches ranking
//!
//! 1. **Ingest** stores each package's direct deps in
//!    [`SearchFacets::dependencies`](crate::metadata::SearchFacets::dependencies).
//! 2. **Sweep** ([`crate::index::Index::refresh_dependents`]) builds
//!    [`DependencyRow`]s from those facets, runs [`count_dependents`], and
//!    writes the per-package in-degree back to
//!    [`SearchFacets::dependents`](crate::metadata::SearchFacets::dependents).
//! 3. **Search candidate build** (`search/mod.rs`) copies `facets.dependents`
//!    onto [`ranking::Candidate::dependents`]. When downloads are absent,
//!    [`ranking::Candidate::popularity_weight`] uses dependents alone
//!    (converted via `DEPENDENT_DOWNLOAD_EQUIV`).

use std::collections::{HashMap, HashSet};

use ecosystem::Language;
use smol_str::SmolStr;

/// One package's contribution to the sweep: who it is and what it depends on.
pub struct DependencyRow {
	pub ecosystem: Language,
	/// Canonical lowercase package name.
	pub name: SmolStr,
	/// Lowercase direct-dependency names (from `SearchFacets::dependencies`).
	pub dependencies: Vec<SmolStr>,
}

/// Count direct dependents per `(ecosystem, name)`. Each depending package
/// counts once per target even if it appears with multiple versions — pass one
/// row per package (latest generation); duplicate `(ecosystem, name)` rows are
/// collapsed, self-dependencies ignored.
///
/// The resulting counts are the values persisted into `SearchFacets.dependents`
/// and later consumed by ranking popularity when downloads are `None`.
pub fn count_dependents(
	rows: impl IntoIterator<Item = DependencyRow>,
) -> HashMap<(Language, SmolStr), u32> {
	// Collapse duplicate (ecosystem, name) dependers: track which (eco, name)
	// pairs we've already processed as a depender so their deps count only once.
	let mut seen_dependers: HashSet<(Language, SmolStr)> = HashSet::new();
	let mut counts: HashMap<(Language, SmolStr), u32> = HashMap::new();

	for row in rows {
		let depender_key = (row.ecosystem, row.name.clone());
		if !seen_dependers.insert(depender_key) {
			continue;
		}
		for dep in &row.dependencies {
			if *dep == row.name {
				continue; // self-dependency: ignore
			}
			*counts.entry((row.ecosystem, dep.clone())).or_default() += 1;
		}
	}

	counts
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	fn row(eco: Language, name: &str, deps: &[&str]) -> DependencyRow {
		DependencyRow {
			ecosystem: eco,
			name: SmolStr::new(name),
			dependencies: deps.iter().map(|&d| SmolStr::new(d)).collect(),
		}
	}

	#[test]
	fn basic_in_degree() {
		let rows = vec![
			row(Language::Rust, "a", &["serde", "tokio"]),
			row(Language::Rust, "b", &["serde"]),
			row(Language::Rust, "c", &["tokio"]),
		];
		let counts = count_dependents(rows);
		assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 2);
		assert_eq!(counts[&(Language::Rust, SmolStr::new("tokio"))], 2);
	}

	#[test]
	fn duplicate_depender_rows_collapse() {
		// Same (ecosystem, name) depender appears twice — should count only once.
		let rows = vec![
			row(Language::Rust, "a", &["serde"]),
			row(Language::Rust, "a", &["serde"]),
		];
		let counts = count_dependents(rows);
		assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 1);
	}

	#[test]
	fn self_dep_ignored() {
		let rows = vec![
			row(Language::Rust, "mylib", &["mylib", "serde"]),
		];
		let counts = count_dependents(rows);
		assert!(!counts.contains_key(&(Language::Rust, SmolStr::new("mylib"))),
			"self-dependency must not count");
		assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 1);
	}

	#[test]
	fn cross_ecosystem_names_dont_collide() {
		// "serde" in Rust vs "serde" in npm are separate targets.
		let rows = vec![
			row(Language::Rust,       "a",     &["serde"]),
			row(Language::Typescript, "b",     &["serde"]),
		];
		let counts = count_dependents(rows);
		assert_eq!(counts[&(Language::Rust,       SmolStr::new("serde"))], 1);
		assert_eq!(counts[&(Language::Typescript, SmolStr::new("serde"))], 1);
	}

	#[test]
	fn empty_input_returns_empty() {
		let counts = count_dependents(std::iter::empty());
		assert!(counts.is_empty());
	}
}
