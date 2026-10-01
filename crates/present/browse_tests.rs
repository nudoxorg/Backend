//! This repository's tree on 2026-09-27, read into the sentences every
//! surface shows. The fixture is real (`cargo metadata` and `Cargo.lock`),
//! and so is the advisory (RUSTSEC-2025-0141, verbatim).

#![allow(clippy::expect_used, clippy::panic)]

use crate::browse::read_tree;
use backend_advisory::{AdvisoryAuthority, AdvisorySource, AuthorityFeed, normalize_package};
use backend_library::browse::{
    LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership,
    ProjectTree, RoleId, TreeSource, build_tree, lockfile_input, metadata_input,
};

const METADATA: &[u8] = include_bytes!("../library/browse/fixtures/tree-2026-09-27/metadata.json");
const LOCKFILE: &str = include_str!("../library/browse/fixtures/tree-2026-09-27/Cargo.lock");
const BINCODE: &[u8] = include_bytes!("../advisory/fixtures/rustsec/crates/bincode/RUSTSEC-2025-0141.md");

fn tree() -> ProjectTree {
    let mut authority = AdvisoryAuthority::new(u64::MAX);
    authority
        .apply(AuthorityFeed::parse(AdvisorySource::RustSec, BINCODE, 1, None, None).expect("feed"))
        .expect("admit");
    let input = metadata_input(METADATA, "aarch64-apple-darwin", Some(LOCKFILE)).expect("metadata");
    let observe = |name: &str, version: &str| {
        authority.observe(&normalize_package("cargo", name).expect("identity"), version, false, false, 1, false)
    };
    build_tree(&input, &observe)
}

#[test]
fn the_tree_reads_as_the_library_page_says_it() {
    let reading = read_tree(&tree());
    assert_eq!(reading.name, "backend");
    assert_eq!(
        reading.lede,
        "Your 44 packages lean on 75 others directly, and 884 in all."
    );
    assert_eq!(
        reading.locked_inactive_note.as_deref(),
        Some("309 packages are locked but inactive for the current target/features")
    );
    assert_eq!(
        reading.twice_line.as_deref(),
        Some("60 crates appear at more than one version")
    );
    assert_eq!(
        reading.health,
        "advisories from a partial source, not a full check of 884"
    );
    let alert = &reading.alerts[0];
    assert_eq!(alert.title, "bincode 1.3.3 is unmaintained");
    assert_eq!(alert.id, "RUSTSEC-2025-0141");
    assert_eq!(alert.summary.as_deref(), Some("Bincode is unmaintained"));
    assert_eq!(
        alert.why,
        "desktop → gpui_ce_components 0.2.0 → gpui_ce_components_base 0.2.0 → syntect 5.3.0 → bincode 1.3.3"
    );
}

#[test]
fn inactive_rows_never_claim_they_are_other_platforms_and_keep_partial_coverage_visible() {
    let mut tree = tree();
    tree.locked_inactive = 1;
    tree.locked_inactive_coverage = LockedInactiveCoverage::Complete;
    assert_eq!(
        read_tree(&tree).locked_inactive_note.as_deref(),
        Some("1 package is locked but inactive for the current target/features")
    );

    tree.locked_inactive = 0;
    tree.locked_inactive_coverage = LockedInactiveCoverage::Partial {
        unmatched_packages: 2,
    };
    assert_eq!(
        read_tree(&tree).locked_inactive_note.as_deref(),
        Some("some package rows could not be matched exactly (2); the inactive-package count is a lower bound")
    );

    tree.locked_inactive_coverage = LockedInactiveCoverage::Unavailable;
    assert_eq!(
        read_tree(&tree).locked_inactive_note.as_deref(),
        Some("inactive rows cannot be counted without both the current resolution and Cargo.lock")
    );
}

#[test]
fn toml_is_here_twice_and_moving_yours_would_not_drop_a_copy() {
    let reading = read_tree(&tree());
    let toml = reading.twice.iter().find(|twice| twice.name == "toml").expect("toml twice");
    assert_eq!(
        toml.paths.as_ref(),
        [
            "desktop → toml 0.8.23".to_owned(),
            "frontend-rust → ra_ap_project_model 0.0.341 → toml 1.1.5".to_owned(),
        ]
    );
    assert_eq!(toml.copies.as_ref(), [("0.8.23".to_owned(), true), ("1.1.5".to_owned(), false)]);
    assert_eq!(
        toml.verdict,
        "Moving yours to 1.1.5 keeps both: cbindgen and rust-i18n-support still ask for 0.8."
    );
    let syn = reading.twice.iter().find(|twice| twice.name == "syn").expect("syn twice");
    assert!(syn.verdict.starts_with("Neither is yours to move: "), "{}", syn.verdict);
    assert!(syn.verdict.ends_with(" still ask for 2.x."), "{}", syn.verdict);
}

#[test]
fn a_third_recorded_version_never_reads_as_twice() {
    let mut source = tree();
    let toml = source.twice.iter_mut().find(|duplicate| duplicate.name == "toml").expect("toml duplicate");
    let mut copies = toml.copies.to_vec();
    let mut third = copies[1].clone();
    third.version = "1.2.0".to_owned();
    copies.push(third);
    toml.copies = copies.into_boxed_slice();
    let reading = read_tree(&source);
    assert_eq!(reading.twice_heading.as_ref().map(|(title, _)| title.as_str()), Some("Multiple versions"));
    assert!(reading.twice_line.as_deref().is_some_and(|line| line.contains("appear at more than one version")));
    let formats = reading.roles.iter().find(|role| role.id == RoleId::Formats).expect("formats");
    let toml = formats.rows.iter().find(|row| row.name == "toml").expect("toml row");
    assert_eq!(toml.at_rest.as_deref(), Some("3 versions · 0.8.23 · 1.1.5 · 1.2.0"));
}

#[test]
fn each_role_says_what_it_is_for_and_why_a_dependency_is_in_it() {
    let reading = read_tree(&tree());
    let formats = reading.roles.iter().find(|role| role.id == RoleId::Formats).expect("formats");
    assert_eq!(formats.label, "speaks formats");
    let toml = formats.rows.iter().find(|row| row.name == "toml").expect("toml row");
    assert_eq!(toml.evidence, "encoding · parser-implementations · parsing · used by advisory, desktop, engine and local-service");
    assert_eq!(toml.at_rest.as_deref(), Some("twice · 0.8.23 · 1.1.5"));
    let languages = reading.roles.iter().find(|role| role.id == RoleId::Languages).expect("languages");
    assert_eq!(languages.label, "reads languages");
    let hir = languages.rows.iter().find(|row| row.name == "ra_ap_hir").expect("ra_ap_hir");
    assert_eq!(hir.evidence, "declares nothing · used by frontend-rust, like tree-sitter-rust");
    let window = reading.roles.iter().find(|role| role.id == RoleId::Window).expect("window");
    assert_eq!(window.label, "draws the window");
    assert!(
        window.brings.as_deref().is_some_and(|brings| brings.ends_with("crates that come with them")),
        "{:?}",
        window.brings
    );
}

#[test]
fn dependency_rows_retain_aligned_source_references_even_at_the_same_version() {
    use backend_library::PackageReference;
    let mut source = tree();
    let reading = read_tree(&source);
    let toml = reading.roles.iter().flat_map(|role| &role.rows).find(|row| row.name == "toml").expect("toml");
    assert_eq!(toml.versions.len(), toml.sources.len());
    assert!(toml.sources.iter().all(|source| source.as_ref().is_some_and(|reference| reference.as_str().contains("cargo-authority="))));
    let gpui = reading.roles.iter().flat_map(|role| &role.rows).find(|row| row.name == "gpui-ce").expect("vendored direct dependency");
    assert!(gpui.sources.iter().any(Option::is_some));

    let version = toml.versions[0].clone();
    let direct = source.direct.iter_mut().find(|dependency| dependency.name == "toml").expect("direct dependency");
    let original = direct.package_references[0].clone().expect("first exact receipt");
    let distinct = PackageReference::parse(&format!(
        "pkg:cargo/toml@{version}?cargo-authority=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )).expect("distinct typed address");
    let mut versions = direct.versions.to_vec();
    versions.insert(1, version.clone());
    direct.versions = versions.into_boxed_slice();
    let mut references = direct.package_references.to_vec();
    references.insert(1, Some(distinct.clone()));
    direct.package_references = references.into_boxed_slice();
    let reading = read_tree(&source);
    let toml = reading.roles.iter().flat_map(|role| &role.rows).find(|row| row.name == "toml").expect("toml");
    assert_eq!(toml.versions[0], toml.versions[1]);
    assert_eq!(toml.sources[0].as_ref(), Some(&original));
    assert_eq!(toml.sources[1].as_ref(), Some(&distinct));
}

#[test]
fn lockfile_fallback_names_unattributed_edges_as_partial() {
    let mut source = tree();
    source.source = TreeSource::Lockfile {
        reason: "Cargo metadata could not be read".to_owned(),
        coverage: LockfileGraphCoverage::Partial {
            ambiguous_edges: 3,
            ambiguous_package_rows: 0,
        },
        workspace_membership: LockfileWorkspaceMembership::Unknown,
    };
    let reading = read_tree(&source);
    assert_eq!(
        reading.source_note.as_deref(),
        Some(
            "Read from Cargo.lock alone, without target/feature filtering or package metadata; workspace membership and local paths are unknown: Cargo metadata could not be read; 3 dependency edge(s) could not be attributed and 0 package row(s) share an indistinguishable source identity"
        )
    );
}

#[test]
fn lockfile_fallback_never_claims_workspace_membership() {
    let input = lockfile_input(
        LOCKFILE,
        "/workspace/backend",
        &Default::default(),
        "Cargo unavailable",
    )
    .expect("lockfile-only input");
    let tree = build_tree(&input, &|_, _| {
        let authority = AdvisoryAuthority::new(0);
        authority.observe(
            &normalize_package("cargo", "none").expect("test identity"),
            "0.0.0",
            false,
            false,
            0,
            false,
        )
    });
    let reading = read_tree(&tree);
    assert_eq!(
        reading.lede,
        "Cargo.lock lists 1237 package rows; workspace membership is unknown."
    );
    assert_eq!(tree.members.len(), 0);
    assert_eq!(tree.direct.len(), 0);
    assert!(tree.packages.iter().all(|package| package.why.is_empty()));
    assert!(
        reading.twice.iter().all(|duplicate| {
            duplicate
                .paths
                .iter()
                .all(|path| path == "Workspace path unknown")
                && duplicate.verdict.contains("Workspace membership is unknown")
        })
    );
    assert!(reading.source_note.as_deref().is_some_and(|note| {
        note.contains("workspace membership and local paths are unknown")
    }));
}

#[test]
fn the_cli_prints_the_same_sentences() {
    let view = crate::product_view(&backend_library::SurfaceReply::ProjectTree(Box::new(tree())));
    let text = crate::text::product(&view, crate::Theme::plain());
    for line in [
        "your tree · backend",
        "Your 44 packages lean on 75 others directly, and 884 in all.",
        "bincode 1.3.3 is unmaintained",
        "desktop → gpui_ce_components 0.2.0 → gpui_ce_components_base 0.2.0 → syntect 5.3.0 → bincode 1.3.3",
        "speaks formats",
        "desktop → toml 0.8.23  |  frontend-rust → ra_ap_project_model 0.0.341 → toml 1.1.5",
    ] {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
}
