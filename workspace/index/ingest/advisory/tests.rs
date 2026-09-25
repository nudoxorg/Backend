use super::*;

#[test]
fn osv_affected_packages_become_sources_and_withdrawn_keeps_the_id() {
    let body = br#"{
        "id": "RUSTSEC-2021-0001",
        "published": "2021-01-02T00:00:00Z",
        "withdrawn": "2021-02-03T00:00:00Z",
        "summary": "overflow",
        "references": [{"url": "https://rustsec.org/advisories/RUSTSEC-2021-0001"}],
        "database_specific": {"severity": "HIGH"},
        "affected": [
            {"package": {"ecosystem": "crates.io", "name": "smallvec"},
             "ranges": [{"events": [{"introduced": "0"}, {"fixed": "1.6.1"}]}]},
            {"package": {"ecosystem": "crates.io", "name": ""}},
            {"package": {"name": "other"}}
        ]
    }"#;
    let sources = parse_osv(body, 1).expect("osv");
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].upstream_id, "RUSTSEC-2021-0001");
    assert_eq!(sources[0].affected_name.as_deref(), Some("smallvec"));
    assert_eq!(sources[0].affected_ecosystem.as_deref(), Some("crates.io"));
    assert_eq!(
        sources[0].version_range.as_deref(),
        Some("introduced:0,fixed:1.6.1")
    );
    assert_eq!(sources[0].severity.as_deref(), Some("HIGH"));
    assert!(sources[0].stem_id.is_none());
    assert!(sources[0].valid_to.is_some());
    assert_eq!(sources[1].affected_name.as_deref(), Some("other"));
    let op = sources[0].to_upsert_op();
    assert!(matches!(op, CatalogOp::UpsertAdvisory { .. }));
}

#[test]
fn osv_without_id_is_rejected_and_versions_list_is_the_range() {
    assert!(parse_osv(br#"{"summary":"x"}"#, 0).is_err());
    let body = br#"{"id":"GHSA-1","affected":[{"package":{"ecosystem":"npm","name":"left-pad"},"versions":["1.1.2","1.0.0"]}]}"#;
    let sources = parse_osv(body, 9).expect("osv");
    assert_eq!(sources[0].version_range.as_deref(), Some("1.1.2,1.0.0"));
    assert_eq!(sources[0].valid_from, 9);
}

#[test]
fn resolve_osv_assigns_one_stem_per_language_and_keeps_unknown_ecosystems_open() {
    let body = br#"{
        "id": "GHSA-9",
        "withdrawn": "2024-01-01T00:00:00Z",
        "affected": [
            {"package": {"ecosystem": "crates.io", "name": "Serde_JSON"}},
            {"package": {"ecosystem": "crates.io", "name": "serde-json"}},
            {"package": {"ecosystem": "npm", "name": "serde-json"}},
            {"package": {"ecosystem": "Packagist", "name": "serde-json"}},
            {"package": {"ecosystem": "crates.io", "name": "not a crate"}}
        ]
    }"#;
    let sources = resolve_osv(body, 1).expect("resolve");
    assert_eq!(sources.len(), 5);
    let rust_a = sources[0].stem_id.expect("Serde_JSON resolves");
    let rust_b = sources[1].stem_id.expect("serde-json resolves");
    assert_eq!(rust_a, rust_b, "crates.io folds case and underscores");
    let npm = sources[2].stem_id.expect("npm resolves");
    assert_ne!(
        npm, rust_a,
        "the same spelling in two ecosystems is two stems"
    );
    assert!(
        sources[3].stem_id.is_none(),
        "an unknown ecosystem stays unresolved"
    );
    assert!(
        sources[4].stem_id.is_none(),
        "a name that fails the grammar stays unresolved"
    );
    assert!(sources[0].valid_to.is_some(), "withdrawal still resolves");
    let direct = {
        use crate::ecosystem::{Language, LanguageExt};
        let parsed = Language::Rust
            .spec()
            .parse_name("serde-json")
            .expect("parse");
        let id = heart::identity::derive::package_id_from_parts([
            Language::Rust.as_token().as_bytes(),
            parsed.canonical().as_bytes(),
        ]);
        PackageStemId::from_uuid(*id.as_uuid())
    };
    assert_eq!(rust_a, direct);
}

#[test]
fn explicit_versions_emit_one_listing_and_ranges_do_not() {
    let listed = br#"{"id":"GHSA-1","affected":[{"package":{"ecosystem":"npm","name":"left-pad"},"versions":["1.0.0"," 1.0.0 "," "]}]}"#;
    let sources = resolve_osv(listed, 9).expect("resolve");
    let ops = sources[0].catalog_ops();
    assert!(matches!(ops[0], CatalogOp::UpsertAdvisory { .. }));
    match &ops[1] {
        CatalogOp::SetListing {
            status: ListingStatus::Advisory,
            reason,
            version,
            ..
        } => {
            assert_eq!(reason.as_deref(), Some("GHSA-1"));
            let stem = sources[0].stem_id.expect("stem");
            let expected = heart::identity::derive::package_id_from_parts([
                stem.to_blob().as_slice(),
                b"1.0.0".as_slice(),
            ]);
            assert_eq!(*version, expected);
        }
        other => panic!("expected one advisory listing, got {other:?}"),
    }
    assert_eq!(ops.len(), 2);

    assert!(version_in_osv_range("introduced:0,fixed:1.6.1", "1.6.0"));
    assert!(!version_in_osv_range("introduced:0,fixed:1.6.1", "1.6.1"));
    assert!(version_in_osv_range(
        "introduced:1.2.0,last_affected:1.2.5",
        "1.2.5"
    ));
    assert!(!version_in_osv_range(
        "introduced:1.2.0,last_affected:1.2.5",
        "1.2.6"
    ));
    assert!(!version_in_osv_range("introduced:0,fixed:1.0.0", "abc123"));

    let ranged = br#"{"id":"RUSTSEC-1","affected":[{"package":{"ecosystem":"crates.io","name":"smallvec"},"ranges":[{"events":[{"introduced":"0"},{"fixed":"1.6.1"}]}]}]}"#;
    let ranged_source = &resolve_osv(ranged, 1).expect("range")[0];
    let covered = heart::identity::PackageId::from_uuid(uuid::Uuid::from_u128(1));
    let fixed = heart::identity::PackageId::from_uuid(uuid::Uuid::from_u128(2));
    let listings = range_listings_for(ranged_source, &[(covered, "1.0.0"), (fixed, "1.6.1")]);
    assert_eq!(listings.len(), 1);
    assert!(matches!(listings[0], CatalogOp::SetListing { version, .. } if version == covered));
    assert_eq!(
        resolve_osv(ranged, 1).expect("range")[0]
            .catalog_ops()
            .len(),
        1
    );
    let withdrawn = br#"{"id":"GHSA-2","withdrawn":"2024-01-01T00:00:00Z","affected":[{"package":{"ecosystem":"npm","name":"left-pad"},"versions":["1.0.0"]}]}"#;
    assert_eq!(
        resolve_osv(withdrawn, 1).expect("withdrawn")[0]
            .catalog_ops()
            .len(),
        1
    );
}

#[test]
fn a_rustsec_file_uses_the_same_window_as_an_osv_range() {
    let toml = r#"
        [advisory]
        id = "RUSTSEC-2020-0001"
        package = "smallvec"
        date = "2020-01-15"
        url = "https://rustsec.org/advisories/RUSTSEC-2020-0001"
        title = "overflow"

        [versions]
        patched = [">= 1.6.1"]
    "#;
    let rustsec = parse_rustsec(toml, 1).expect("rustsec");
    let osv = &resolve_osv(
        br#"{"id":"RUSTSEC-2020-0001","affected":[{"package":{"ecosystem":"crates.io","name":"smallvec"},"ranges":[{"events":[{"introduced":"0"},{"fixed":"1.6.1"}]}]}]}"#,
        1,
    )
    .expect("osv")[0];
    assert_eq!(rustsec.upstream_id, osv.upstream_id);
    assert_eq!(rustsec.version_range, osv.version_range);
    assert_eq!(rustsec.stem_id, osv.stem_id);
    assert!(version_in_osv_range(
        rustsec.version_range.as_deref().unwrap(),
        "1.6.0"
    ));
    assert!(!version_in_osv_range(
        rustsec.version_range.as_deref().unwrap(),
        "1.6.1"
    ));

    let several = r#"
        [advisory]
        id = "RUSTSEC-2"
        package = "smallvec"
        date = "2020-01-15"
        [versions]
        patched = [">= 0.2.19", ">= 0.3.1"]
    "#;
    let split = parse_rustsec(several, 1).expect("two lines");
    assert!(split.version_range.is_none());
    assert_eq!(split.catalog_ops().len(), 1);
}
