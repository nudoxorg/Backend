//! This repository's tree on 2026-09-27, read into the sentences every
//! surface shows. The fixture is real (`cargo metadata` and `Cargo.lock`),
//! and so is the advisory (RUSTSEC-2025-0141, verbatim).

#![allow(clippy::expect_used, clippy::panic)]

use crate::browse::read_tree;
use backend_advisory::{AdvisoryAuthority, AdvisorySource, AuthorityFeed, normalize_package};
use backend_library::browse::{PackageOrigin, ProjectTree, RoleId, build_tree, metadata_input};

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
    assert_eq!(reading.lede, "Your 44 packages lean on 75 others directly, and 884 in all.");
    assert_eq!(reading.elsewhere.as_deref(), Some("and 309 more for other platforms"));
    assert_eq!(reading.twice_line.as_deref(), Some("60 crates appear at more than one version"));
    assert_eq!(reading.health, "advisories from a partial source, not a full check of 884");
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
fn dependency_rows_retain_exact_source_origin_and_refuse_ambiguous_releases() {
    let mut source = tree();
    let reading = read_tree(&source);
    let toml = reading.roles.iter().flat_map(|role| &role.rows).find(|row| row.name == "toml").expect("toml");
    assert_eq!(toml.versions.len(), toml.sources.len());
    assert!(toml.sources.iter().all(|source| matches!(source, Some(PackageOrigin::Registry))));
    let gpui = reading.roles.iter().flat_map(|role| &role.rows).find(|row| row.name == "gpui-ce").expect("vendored direct dependency");
    assert!(gpui.sources.iter().any(|source| matches!(source, Some(PackageOrigin::Vendored { path }) if path == "vendor/gpui-ce")));

    let version = toml.versions[0].clone();
    let duplicate = source.packages.iter().find(|package| package.name == "toml" && package.version == version).expect("toml package").clone();
    let mut duplicate = duplicate;
    duplicate.origin = PackageOrigin::Git { url: "https://example.invalid/toml".to_owned() };
    let mut packages = source.packages.to_vec();
    packages.push(duplicate);
    source.packages = packages.into_boxed_slice();
    let reading = read_tree(&source);
    let toml = reading.roles.iter().flat_map(|role| &role.rows).find(|row| row.name == "toml").expect("toml");
    let at = toml.versions.iter().position(|candidate| *candidate == version).expect("version");
    assert_eq!(toml.sources[at], None, "same name and release at two origins has no safe link");
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
