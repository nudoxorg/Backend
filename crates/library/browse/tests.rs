//! The tree of this repository as it stood on 2026-09-27, read through the
//! real Cargo metadata and lockfile, with the real RUSTSEC-2025-0141.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use backend_advisory::{
    AdvisoryAuthority, AdvisoryCoverage, AdvisorySource, AdvisoryStatus, AuthorityFeed,
    normalize_package,
};
use std::collections::BTreeSet;

const METADATA: &[u8] = include_bytes!("fixtures/tree-2026-09-27/metadata.json");
const LOCKFILE: &str = include_str!("fixtures/tree-2026-09-27/Cargo.lock");
const BINCODE: &[u8] =
    include_bytes!("../../advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md");

fn no_source(_: &str, _: &str) -> backend_advisory::AdvisoryObservation {
    AdvisoryAuthority::new(1).observe(
        &normalize_package("cargo", "none").expect("identity"),
        "0.0.0",
        false,
        false,
        0,
        false,
    )
}

fn rustsec() -> AdvisoryAuthority {
    let mut authority = AdvisoryAuthority::new(u64::MAX);
    authority
        .apply(AuthorityFeed::parse(AdvisorySource::RustSec, BINCODE, 1, None, None).expect("feed"))
        .expect("admit");
    authority
}

fn tree(authority: &AdvisoryAuthority) -> ProjectTree {
    let input = metadata_input(METADATA, "aarch64-apple-darwin", Some(LOCKFILE)).expect("metadata");
    let observe = |name: &str, version: &str| {
        let package = normalize_package("cargo", name).expect("identity");
        authority.observe(&package, version, false, false, 1, false)
    };
    build_tree(&input, &observe)
}

fn path(hops: &[WhyHop]) -> String {
    hops.iter()
        .map(|hop| match &hop.version {
            Some(version) => format!("{} {version}", hop.name),
            None => hop.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

#[test]
fn toml_is_here_twice_and_each_copy_has_its_own_reason() {
    let tree = tree(&rustsec());
    let ours = tree.package("toml", "0.8.23").expect("toml 0.8.23");
    assert_eq!(path(&ours.why), "desktop → toml 0.8.23");
    let theirs = tree.package("toml", "1.1.5+spec-1.1.0").expect("toml 1.1.5");
    assert_eq!(
        path(&theirs.why),
        "frontend-rust → ra_ap_project_model 0.0.341 → toml 1.1.5+spec-1.1.0"
    );
    let toml = tree.twice.iter().find(|duplicate| duplicate.name == "toml").expect("toml twice");
    let copies = toml
        .copies
        .iter()
        .map(|copy| {
            (copy.version.as_str(), copy.class.as_str(), copy.yours, copy.only_yours, copy.also_asked_by.to_vec())
        })
        .collect::<Vec<_>>();
    // The prototype said "moving yours to 1.1.5 drops a copy". The resolved
    // graph says otherwise: two other crates still ask for toml 0.8.
    assert_eq!(
        copies,
        [
            ("0.8.23", "0.8", true, false, vec!["cbindgen".to_owned(), "rust-i18n-support".to_owned()]),
            (
                "1.1.5+spec-1.1.0",
                "1",
                false,
                false,
                vec!["embed-resource".to_owned(), "ra_ap_project_model".to_owned(), "trybuild".to_owned()]
            ),
        ]
    );
}

#[test]
fn counts_are_what_builds_on_this_machine() {
    let tree = tree(&rustsec());
    assert_eq!(tree.source, TreeSource::Cargo { host: "aarch64-apple-darwin".to_owned() });
    assert_eq!(tree.name, "backend");
    assert_eq!(tree.members.len(), 44);
    assert_eq!(tree.direct.len(), 75);
    assert_eq!(tree.packages.len(), 884);
    assert_eq!(tree.other_platforms, 309);
    assert_eq!(tree.twice.len(), 60);
    let desktop = tree.members.iter().find(|member| member.short == "desktop").expect("desktop");
    assert_eq!(desktop.name, "backend-desktop");
    assert!(desktop.has_bin);
    let vendored = tree.package("gpui-ce", "0.2.2").expect("gpui-ce");
    assert_eq!(vendored.origin, PackageOrigin::Vendored { path: "vendor/gpui-ce".to_owned() });
}

#[test]
fn roles_are_derived_from_what_packages_declare() {
    let tree = tree(&rustsec());
    let find = |name: &str| tree.direct.iter().find(|dependency| dependency.name == name).expect(name);
    let toml = find("toml");
    assert_eq!(toml.role, RoleId::Formats);
    let RoleEvidence::Declared { words } = &toml.evidence else {
        panic!("toml declares its role: {:?}", toml.evidence)
    };
    assert_eq!(&words[..3], ["encoding", "parser-implementations", "parsing"]);
    let members = toml.by.iter().map(|edge| edge.member.as_str()).collect::<Vec<_>>();
    assert_eq!(members, ["advisory", "desktop", "engine", "local-service"]);

    let hir = find("ra_ap_hir");
    assert_eq!(hir.role, RoleId::Languages, "{:?}", hir.evidence);
    assert_eq!(hir.evidence, RoleEvidence::Cohort { peer: "tree-sitter-rust".to_owned() });
    assert_eq!(find("blake3").evidence, RoleEvidence::Described { phrase: "hash function".to_owned() });
    assert_eq!(find("gpui-ce").role, RoleId::Window);
    assert_eq!(find("tokio").role, RoleId::Concurrency);
    assert_eq!(find("ureq").role, RoleId::Network);
    assert_eq!(find("thiserror").role, RoleId::Errors);
    let base_db = find("ra_ap_base_db");
    assert_eq!(
        (base_db.role, &base_db.evidence),
        (RoleId::Languages, &RoleEvidence::Cohort { peer: "tree-sitter-rust".to_owned() }),
        "its description says \"database\", but it is used exactly as tree-sitter-rust is"
    );
    assert_eq!(
        (find("trustfall").role, &find("trustfall").evidence),
        (RoleId::Store, &RoleEvidence::Described { phrase: "query engine".to_owned() })
    );
    let other = tree.in_role(RoleId::Other).map(|dependency| dependency.name.as_str()).collect::<Vec<_>>();
    assert_eq!(other, ["derive_more", "fearless_simd", "turso"], "nothing on record says what these do");
    assert!(tree.brought_by(RoleId::Window).count() > 100, "gpui brings most of the tree");
}

#[test]
fn a_direct_dependency_is_marked_direct_and_a_transitive_one_is_not() {
    let tree = tree(&rustsec());
    // toml is a direct dependency (its own why-path is one hop: "desktop → toml 0.8.23",
    // proven by `toml_is_here_twice_and_each_copy_has_its_own_reason`).
    let toml = tree.package("toml", "0.8.23").expect("toml 0.8.23");
    assert_eq!(toml.role, PackageRole::Direct, "{:?}", toml.role);
    // bincode 1.3.3 is reached only through syntect, four hops from a member
    // ("desktop → gpui_ce_components 0.2.0 → gpui_ce_components_base 0.2.0 →
    // syntect 5.3.0 → bincode 1.3.3", proven by
    // `bincode_is_unmaintained_and_the_path_says_how_it_got_here`): no member
    // depends on it directly, so it must never read as Direct. A mutation
    // that marks every package Direct must not pass this line.
    let bincode = tree.package("bincode", "1.3.3").expect("bincode 1.3.3");
    assert_ne!(bincode.role, PackageRole::Direct, "{:?}", bincode.role);
}

#[test]
fn bincode_is_unmaintained_and_the_path_says_how_it_got_here() {
    let tree = tree(&rustsec());
    let affecting = tree
        .health
        .affecting
        .iter()
        .map(|advisory| {
            (
                advisory.package.as_str(),
                advisory.version.as_str(),
                advisory.id.as_str(),
                advisory.summary.as_deref(),
                advisory.statuses.to_vec(),
                path(&advisory.why),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        affecting,
        [(
            "bincode",
            "1.3.3",
            "RUSTSEC-2025-0141",
            Some("Bincode is unmaintained"),
            vec![AdvisoryStatus::Unmaintained],
            "desktop → gpui_ce_components 0.2.0 → gpui_ce_components_base 0.2.0 → syntect 5.3.0 → bincode 1.3.3"
                .to_owned(),
        )]
    );
    assert_eq!(tree.health.coverage, AdvisoryCoverage::Partial, "one advisory is not a full check");
    assert_eq!(tree.health.checked, 0);
    assert_eq!(tree.health.of, 884);
}

#[test]
fn with_no_advisory_source_nothing_is_claimed() {
    let input = metadata_input(METADATA, "aarch64-apple-darwin", None).expect("metadata");
    let tree = build_tree(&input, &no_source);
    assert!(tree.health.affecting.is_empty());
    assert_eq!(tree.health.coverage, AdvisoryCoverage::Unknown);
    assert_eq!(tree.other_platforms, 0, "no lockfile, no count");
}

#[test]
fn the_lockfile_alone_still_explains_the_tree() {
    let patched = ["gpui-ce", "gpui_ce_components", "gpui_ce_components_base", "gpui_ce_macos"]
        .map(ToOwned::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let input = lockfile_input(LOCKFILE, "/workspace/backend", &patched, "cargo was not found").expect("lock");
    let tree = build_tree(&input, &no_source);
    assert_eq!(
        tree.source,
        TreeSource::Lockfile { reason: "cargo was not found".to_owned() }
    );
    assert_eq!(tree.members.len(), 44);
    assert_eq!(tree.packages.len(), 1193, "every platform counts");
    let toml = tree.package("toml", "0.8.23").expect("toml");
    assert_eq!(toml.why.len(), 2, "{}", path(&toml.why));
    assert!(
        tree.direct.iter().all(|dependency| dependency.role == RoleId::Other),
        "no package states its metadata in a lockfile, so no role is claimed"
    );
}
