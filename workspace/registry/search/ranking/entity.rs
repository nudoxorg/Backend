//! Cross-ecosystem entity clustering by repository slug.
//!
//! When the same upstream project publishes to multiple package registries
//! (e.g. a Rust crate + its WebAssembly npm shim, or a Python library + its
//! conda mirror), search results may contain several entries that represent
//! "the same thing" to the user.  This module groups those siblings into a
//! single [`RepoGroup`] so the SERP can surface one canonical result per
//! project.
//!
//! # Grouping rules
//!
//! - Items are clustered by **repository slug** (e.g. `"owner/repo"`).
//! - Items without a slug (`slug_of` returns `None`) are **never merged** — each
//!   becomes its own singleton [`RepoGroup`].  Two `None`-slug items must *not*
//!   collapse even when adjacent.
//! - Within a group the first item (in input order) becomes the
//!   `representative`; the rest go into `shadows` in input order.
//! - The order of groups in the output follows the **first appearance** of each
//!   cluster in the input, so a caller that passes items in best-first ranked
//!   order gets groups back in the same ranked order.
//!
//! # Pureness
//!
//! No I/O, no clocks, no randomness.  Deterministic output for deterministic
//! input.

use std::collections::HashMap;

/// One cross-registry entity: the best-ranked representative plus its
/// same-repository shadows (e.g. the crate vs its npm wasm shim).
#[derive(Debug)]
pub struct RepoGroup<T> {
	/// The first (best-ranked) item for this repository slug.
	pub representative: T,
	/// All subsequent items that share the same slug, in input order.
	pub shadows: Vec<T>,
}

/// Cluster `items` (already in ranked/best-first order) by repository slug.
///
/// Items without a slug are singleton groups.  The order of groups follows
/// the first appearance of each cluster; within a group `shadows` keep input
/// order.
///
/// The `slug_of` closure is called **exactly once per item** and may return a
/// borrowed `&str` reference derived from the item.
pub fn cluster_by_repo<T>(
	items: Vec<T>,
	slug_of: impl Fn(&T) -> Option<&str>,
) -> Vec<RepoGroup<T>> {
	// Map from slug string → index in `groups` (only for slug-bearing items).
	let mut slug_to_group: HashMap<String, usize> = HashMap::new();
	let mut groups: Vec<RepoGroup<T>> = Vec::new();

	for item in items {
		match slug_of(&item) {
			Some(slug) if !slug.is_empty() => {
				let slug_owned = slug.to_owned();
				if let Some(&group_idx) = slug_to_group.get(&slug_owned) {
					// Existing group: this item is a shadow.
					groups[group_idx].shadows.push(item);
				} else {
					// New slug: start a new group.
					let group_idx = groups.len();
					slug_to_group.insert(slug_owned, group_idx);
					groups.push(RepoGroup { representative: item, shadows: Vec::new() });
				}
			}
			// No slug (or empty slug): singleton group, never merged.
			_ => {
				groups.push(RepoGroup { representative: item, shadows: Vec::new() });
			}
		}
	}

	groups
}

/// Convenience: keep only representatives (cross-ecosystem dedup for
/// unscoped queries), preserving ranked order.
///
/// Equivalent to `cluster_by_repo(…).into_iter().map(|g| g.representative)`,
/// but avoids allocating shadow vecs.
pub fn dedup_by_repo<T>(
	items: Vec<T>,
	slug_of: impl Fn(&T) -> Option<&str>,
) -> Vec<T> {
	cluster_by_repo(items, slug_of)
		.into_iter()
		.map(|g| g.representative)
		.collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	/// A minimal test item: a name and an optional slug.
	#[derive(Debug, PartialEq, Clone)]
	struct Item {
		name: &'static str,
		slug: Option<&'static str>,
	}

	fn item(name: &'static str, slug: Option<&'static str>) -> Item {
		Item { name, slug }
	}

	fn slug_of(i: &Item) -> Option<&str> {
		i.slug
	}

	// ── Cluster ordering ──────────────────────────────────────────────────────

	/// Group order follows the first appearance of each slug, not slug name.
	#[test]
	fn cluster_order_follows_first_appearance() {
		let items = vec![
			item("crate-a",  Some("owner/alpha")),
			item("crate-b",  Some("owner/beta")),
			item("npm-alpha", Some("owner/alpha")), // shadow of group 0
		];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 2);
		assert_eq!(groups[0].representative.name, "crate-a");
		assert_eq!(groups[1].representative.name, "crate-b");
		assert_eq!(groups[0].shadows[0].name, "npm-alpha");
	}

	// ── Slugless items never merge ────────────────────────────────────────────

	/// Two items with `slug = None` must stay as separate singleton groups.
	#[test]
	fn slugless_items_never_merge() {
		let items = vec![
			item("a", None),
			item("b", None),
		];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 2, "each None-slug item must be its own group");
		assert!(groups[0].shadows.is_empty());
		assert!(groups[1].shadows.is_empty());
	}

	/// A None-slug item interleaved with slug-bearing items stays singleton.
	#[test]
	fn slugless_item_interleaved_stays_singleton() {
		let items = vec![
			item("a",        Some("owner/repo")),
			item("orphan",   None),
			item("b",        Some("owner/repo")),
		];
		let groups = cluster_by_repo(items, slug_of);
		// Groups: [owner/repo (a + b shadow)], [orphan]
		assert_eq!(groups.len(), 2);
		assert_eq!(groups[0].representative.name, "a");
		assert_eq!(groups[0].shadows[0].name, "b");
		assert_eq!(groups[1].representative.name, "orphan");
		assert!(groups[1].shadows.is_empty());
	}

	// ── Shadow order ──────────────────────────────────────────────────────────

	/// Shadows within a group preserve input order.
	#[test]
	fn shadows_preserve_input_order() {
		let items = vec![
			item("first",  Some("org/project")),
			item("second", Some("org/project")),
			item("third",  Some("org/project")),
		];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 1);
		let g = &groups[0];
		assert_eq!(g.representative.name, "first");
		assert_eq!(g.shadows[0].name, "second");
		assert_eq!(g.shadows[1].name, "third");
	}

	// ── dedup_by_repo ─────────────────────────────────────────────────────────

	/// dedup keeps only the representative (first occurrence per slug).
	#[test]
	fn dedup_keeps_first_occurrence() {
		let items = vec![
			item("best",   Some("owner/repo")),
			item("shadow", Some("owner/repo")),
			item("solo",   None),
		];
		let deduped = dedup_by_repo(items, slug_of);
		assert_eq!(deduped.len(), 2);
		assert_eq!(deduped[0].name, "best");
		assert_eq!(deduped[1].name, "solo");
	}

	/// Ranked order is preserved by dedup.
	#[test]
	fn dedup_preserves_ranked_order() {
		let items = vec![
			item("rank1", Some("a/a")),
			item("rank2", Some("b/b")),
			item("rank3", Some("c/c")),
			item("shadow_a", Some("a/a")), // goes away
			item("shadow_b", Some("b/b")), // goes away
		];
		let deduped = dedup_by_repo(items, slug_of);
		assert_eq!(deduped.iter().map(|i| i.name).collect::<Vec<_>>(),
			vec!["rank1", "rank2", "rank3"]);
	}

	// ── Empty input ───────────────────────────────────────────────────────────

	#[test]
	fn empty_input_returns_empty_groups() {
		let groups = cluster_by_repo(Vec::<Item>::new(), slug_of);
		assert!(groups.is_empty());
	}

	#[test]
	fn empty_input_dedup_returns_empty() {
		let deduped = dedup_by_repo(Vec::<Item>::new(), slug_of);
		assert!(deduped.is_empty());
	}

	// ── Single item ──────────────────────────────────────────────────────────

	#[test]
	fn single_item_with_slug_is_singleton_group() {
		let items = vec![item("only", Some("owner/only"))];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 1);
		assert_eq!(groups[0].representative.name, "only");
		assert!(groups[0].shadows.is_empty());
	}

	#[test]
	fn single_item_without_slug_is_singleton_group() {
		let items = vec![item("only", None)];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 1);
		assert_eq!(groups[0].representative.name, "only");
		assert!(groups[0].shadows.is_empty());
	}

	// ── All items share one slug ──────────────────────────────────────────────

	#[test]
	fn all_same_slug_collapses_to_one_group() {
		let items = vec![
			item("a", Some("mono/repo")),
			item("b", Some("mono/repo")),
			item("c", Some("mono/repo")),
		];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 1);
		assert_eq!(groups[0].shadows.len(), 2);
	}

	// ── Multiple distinct slugs ───────────────────────────────────────────────

	#[test]
	fn multiple_distinct_slugs_each_singleton() {
		let items = vec![
			item("a", Some("x/1")),
			item("b", Some("x/2")),
			item("c", Some("x/3")),
		];
		let groups = cluster_by_repo(items, slug_of);
		assert_eq!(groups.len(), 3);
		for g in &groups {
			assert!(g.shadows.is_empty());
		}
	}

	// ── Mixed slug and slugless ───────────────────────────────────────────────

	#[test]
	fn mixed_slug_and_slugless_correct_group_count() {
		let items = vec![
			item("a", Some("g/r")),
			item("b", None),
			item("c", Some("g/r")),
			item("d", None),
			item("e", Some("other/repo")),
		];
		let groups = cluster_by_repo(items, slug_of);
		// Groups: [g/r (a+c)], [b], [d], [other/repo (e)]
		assert_eq!(groups.len(), 4);
		assert_eq!(groups[0].representative.name, "a");
		assert_eq!(groups[0].shadows[0].name, "c");
		assert_eq!(groups[1].representative.name, "b");
		assert_eq!(groups[2].representative.name, "d");
		assert_eq!(groups[3].representative.name, "e");
	}
}
