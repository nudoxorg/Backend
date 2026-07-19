//! PackageIndex health snapshot — empty index vs post-absorb.

mod common;

use heart::ResolutionState;
use registry::search::tantivy::PackageIndex;

fn rust_record(name: &str) -> registry::GlobalPackage {
	common::global_package(
		common::rust_package(name, "1.0.0"),
		ResolutionState::Unindexed { needed: false },
	)
}

/// Fresh index reports zero docs and a zero watermark.
#[test]
fn empty_index_health_reports_zero_docs() {
	let directory = common::TempDir::new("search-health-empty");
	let index = PackageIndex::open(directory.path()).expect("open empty index");
	let h = index.health_snapshot();
	assert_eq!(h.num_docs, 0, "empty index must report 0 docs");
	assert_eq!(h.watermark_position, 0);
	assert_eq!(h.schema_version, 4, "schema v4");
	assert_eq!(h.path, directory.path());
}

/// After absorbing two packages, num_docs reflects the live document count.
#[test]
fn absorb_two_packages_health_reports_two_docs() {
	let directory = common::TempDir::new("search-health-absorb");
	let mut index = PackageIndex::open(directory.path()).expect("open index");
	let a = rust_record("alpha");
	let b = rust_record("beta");
	index
		.absorb([&a, &b], 99)
		.expect("absorb two packages");

	let h = index.health();
	assert!(
		h.num_docs >= 1,
		"after commit, searcher must see at least one doc; got {}",
		h.num_docs
	);
	assert_eq!(
		h.num_docs, 2,
		"absorbing two distinct packages must yield num_docs == 2"
	);
	assert_eq!(h.watermark_position, 99);
	assert_eq!(h.schema_version, 4);
}
