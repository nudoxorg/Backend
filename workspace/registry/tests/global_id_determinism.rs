//! Pipeline part: **deterministic global identity** (`registry::identity`).
//!
//! TDD specs for the version-agnostic `SymbolId` derivation. The whole
//! point is that any system recomputes the same id offline — never random.

mod common;

use heart::identity::{EntryUri, namespace};
use smol_str::SmolStr;

/// The entry URI for `serde@1.0.0 :: de::Deserializer`, built fresh each call
/// so equality between two constructions is a real derivation fact.
fn deserializer_uri() -> EntryUri {
    EntryUri {
        package: common::rust_coordinates("serde", "1.0.0").id(),
        path: vec![SmolStr::new("de"), SmolStr::new("Deserializer")].into_boxed_slice(),
    }
}

const INSTANCE: &str = "acme/corpus";

/// The same instance + entry URI always yields the same id.
///
/// Act: `compute(instance, uri)` twice.
/// Assert: both calls return the identical `SymbolId` (UUID v5).
#[test]
fn id_is_deterministic() {
    let first = deserializer_uri().symbol_id(INSTANCE);
    let second = deserializer_uri().symbol_id(INSTANCE);
    assert_eq!(first, second, "two independent derivations of the same (instance, uri) diverged");
    assert_eq!(first.as_uuid().get_version_num(), 5, "symbol ids are UUIDv5 fingerprints");
}

/// Different entry URIs yield different ids.
#[test]
fn different_uris_give_different_ids() {
    let base = deserializer_uri();
    let sibling = EntryUri {
        package: base.package,
        path: vec![SmolStr::new("de"), SmolStr::new("Serializer")].into_boxed_slice(),
    };
    let other_package = EntryUri {
        package: common::rust_coordinates("tokio", "1.0.0").id(),
        path: base.path.clone(),
    };

    assert_ne!(
        base.symbol_id(INSTANCE),
        sibling.symbol_id(INSTANCE),
        "distinct paths within one package must mint distinct ids"
    );
    assert_ne!(
        base.symbol_id(INSTANCE),
        other_package.symbol_id(INSTANCE),
        "the same path in distinct packages must mint distinct ids"
    );
}

/// Different terminus instances yield different ids for the same URI.
///
/// Assert: the instance is salted into the hash (so `org-a/db` and `org-b/db`
///   diverge for the same symbol).
#[test]
fn different_instances_give_different_ids() {
    let uri = deserializer_uri();
    assert_ne!(
        uri.symbol_id("org-a/db"),
        uri.symbol_id("org-b/db"),
        "the instance token must salt the derivation"
    );
}

/// The id matches the canonical NUL-separated v5 formula.
///
/// Assert: `compute(instance, uri)` equals
///   `UUIDv5(NUDOX_SYMBOL_NS, "{instance}\0{uri}")` — guarding the layering so
///   external systems can reproduce it.
#[test]
fn id_matches_the_canonical_formula() {
    let uri = deserializer_uri();

    // The canonical URI string is the package id followed by `/`-joined segments.
    let expected_canonical = format!("{}/de/Deserializer", uri.package);
    assert_eq!(uri.canonical(), expected_canonical);

    // instance ++ 0x00 ++ canonical-uri, hashed v5 under the SYMBOL namespace.
    let mut name = INSTANCE.as_bytes().to_vec();
    name.push(0);
    name.extend_from_slice(expected_canonical.as_bytes());
    let expected = uuid::Uuid::new_v5(&namespace::SYMBOL, &name);

    assert_eq!(
        *uri.symbol_id(INSTANCE).as_uuid(),
        expected,
        "the derivation drifted from the published v5(NS, instance ++ NUL ++ uri) formula"
    );
}

/// Entry URIs are stable round-trips (parse ∘ display == identity).
///
/// `EntryUri` has no parse/Display pair in the current API (a genuine gap
/// against this spec); the closest real contract is that the canonical string
/// form is stable and injective and that reconstruction from equal inputs is
/// the identity — including Rust crate-slug canonicalization
/// (`serde_json` == `serde-json`).
#[test]
fn entry_uri_round_trips() {
    // Rebuilding from the same components is the identity, all the way down to
    // the canonical form and the derived id.
    let first = deserializer_uri();
    let second = deserializer_uri();
    assert_eq!(first, second);
    assert_eq!(first.canonical(), second.canonical());
    assert_eq!(first.symbol_id(INSTANCE), second.symbol_id(INSTANCE));

    // Crate-slug canonicalization folds into the package id, so the two
    // spellings of one crate yield literally the same URI.
    let underscored = EntryUri {
        package: common::rust_coordinates("serde_json", "1.0.0").id(),
        path: vec![SmolStr::new("Value")].into_boxed_slice(),
    };
    let dashed = EntryUri {
        package: common::rust_coordinates("serde-json", "1.0.0").id(),
        path: vec![SmolStr::new("Value")].into_boxed_slice(),
    };
    assert_eq!(underscored, dashed, "crates.io `_`/`-` equivalence must fold into identity");
    assert_eq!(underscored.canonical(), dashed.canonical());
    assert_eq!(underscored.symbol_id(INSTANCE), dashed.symbol_id(INSTANCE));

    // Injectivity of the segmentation: joining the segments differently can
    // never alias the same canonical string.
    let joined = EntryUri {
        package: first.package,
        path: vec![SmolStr::new("de::Deserializer")].into_boxed_slice(),
    };
    assert_ne!(joined.canonical(), first.canonical());
}
