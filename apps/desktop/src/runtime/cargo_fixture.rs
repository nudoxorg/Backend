//! Complete, checked-in Cargo producer input for source-authority regressions.
//! This models an owner-side stable-input witness; it performs no Cargo run or
//! filesystem observation and cannot prove acceptance by a live owner.

use backend_library::browse::{
    CargoTreeError, TreeInput, metadata_input_with_stable_source_witness,
};

// Cargo 1.97.1 actually observed both dependencies under this target, including
// their empty resolved-feature arrays. Keeping the original bytes, target, and
// matching lockfile together avoids claiming authority from trimmed captures.
const METADATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/library/browse/fixtures/filter-platform-cargo-1.97/windows.json"
));
const LOCKFILE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/library/browse/fixtures/filter-platform-cargo-1.97/project/Cargo.lock"
));
const TARGET: &str = "x86_64-pc-windows-msvc";

pub(crate) const PACKAGE_NAME: &str = "common-proof";
pub(crate) const PACKAGE_VERSION: &str = "0.1.0";

pub(crate) fn input() -> Result<TreeInput, CargoTreeError> {
    metadata_input_with_stable_source_witness(METADATA, TARGET, Some(LOCKFILE), [7; 32])
}

#[test]
fn complete_capture_preserves_observed_empty_features_and_exact_path_sources()
-> Result<(), Box<dyn std::error::Error>> {
    use backend_library::{CargoPackageSourceAuthorityStateV1, CargoPackageSourceV1};

    let captured: serde_json::Value = serde_json::from_slice(METADATA)?;
    let nodes = captured["resolve"]["nodes"]
        .as_array()
        .ok_or("captured resolve nodes")?;
    assert_eq!(nodes.len(), 3);
    assert!(
        nodes
            .iter()
            .all(|node| node["features"].as_array().is_some_and(Vec::is_empty))
    );
    let input = input()?;
    assert_eq!(input.packages.len(), 3);
    assert_eq!(input.locked_inactive, 1);
    let external = input
        .packages
        .iter()
        .filter(|row| !row.member)
        .collect::<Vec<_>>();
    assert_eq!(
        external
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        [PACKAGE_NAME, "windows-only-proof"]
    );
    assert!(external.iter().all(|row| row.version == PACKAGE_VERSION));
    let mut references = Vec::new();
    for row in &input.packages {
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &row.source_authority else {
            return Err(format!("complete capture lost source authority: {}", row.name).into());
        };
        assert_eq!(authority.name(), row.name);
        assert_eq!(authority.version(), row.version);
        assert_eq!(authority.effective_target(), TARGET);
        assert_eq!(authority.source(), &CargoPackageSourceV1::Path);
        assert_ne!(authority.resolved_features_digest(), [0; 32]);
        let reference = authority
            .package_reference()
            .map_err(|error| format!("captured package reference: {error:?}"))?;
        assert!(reference.as_str().contains("?cargo-authority="));
        references.push(reference);
    }
    references.sort();
    references.dedup();
    assert_eq!(
        references.len(),
        3,
        "each observed source keeps its exact identity"
    );
    Ok(())
}

#[test]
fn trimmed_legacy_capture_cannot_supply_source_authority_even_with_a_stable_witness()
-> Result<(), CargoTreeError> {
    use backend_library::{
        CargoPackageSourceAuthorityFailureV1, CargoPackageSourceAuthorityStateV1,
    };

    let input = metadata_input_with_stable_source_witness(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"
        )),
        "aarch64-apple-darwin",
        None,
        [7; 32],
    )?;
    assert!(!input.packages.is_empty());
    assert!(input.packages.iter().all(|row| matches!(
        row.source_authority,
        CargoPackageSourceAuthorityStateV1::Unavailable(
            CargoPackageSourceAuthorityFailureV1::MissingResolvedFeatures
        )
    )));
    Ok(())
}
