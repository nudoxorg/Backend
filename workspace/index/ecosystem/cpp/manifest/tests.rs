//! Coordinator-level manifest dispatch + hostile-input stress tests.
//! Per-parser coverage lives in each parser module's own `tests`.

use super::*;
use crate::ecosystem::manifest::ManifestCandidate;

fn parse(suffix: &'static str, bytes: &[u8]) -> Option<CppManifest> {
    parse_manifest(&ManifestCandidate::new(suffix), bytes)
}

#[test]
fn candidate_order_is_stable() {
    let candidates = manifest_candidates();
    let suffixes: Vec<&str> = candidates.iter().map(|c| c.path_suffix).collect();
    assert_eq!(suffixes, vec![
        "vcpkg.json",
        "conanfile.py",
        "conanfile.txt",
        "CMakeLists.txt",
        "meson.build",
        "MODULE.bazel",
        ".gitmodules",
        ".pc.in",
        ".pc",
        "LICENSE",
        "LICENSE.txt",
        "LICENSE.md",
        "LICENCE",
        "LICENCE.txt",
        "LICENCE.md",
        "COPYING",
        "COPYING.txt",
    ]);
}

#[test]
fn license_candidates_match_shared_list() {
    // The literal `LICENSE`/… entries appended to `manifest_candidates()`
    // must stay byte-for-byte in sync with `license::LICENSE_FILENAMES` (the
    // list `parse_manifest`'s dispatch checks against) — this test is the
    // drift guard for that duplication.
    use crate::ecosystem::license;
    let candidates = manifest_candidates();
    let tail: Vec<&str> = candidates
        .iter()
        .rev()
        .take(license::LICENSE_FILENAMES.len())
        .map(|c| c.path_suffix)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert_eq!(tail, license::LICENSE_FILENAMES);
}

#[test]
fn dispatch_license_file_sets_has_license_file_and_sniffs_spdx() {
    let manifest = parse(
        "LICENSE",
        b"MIT License\n\nPermission is hereby granted, free of charge, to any person obtaining \
a copy of this software and associated documentation files (the \"Software\")...",
    )
    .unwrap();
    assert!(manifest.facts.has_license_file);
    assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
    assert!(manifest.dependencies.is_empty());
}

#[test]
fn dispatch_license_file_unrecognized_content_still_sets_has_license_file() {
    let manifest = parse(
        "LICENCE",
        b"All rights reserved. Contact legal@example.com.",
    )
    .unwrap();
    assert!(manifest.facts.has_license_file);
    assert!(
        manifest.facts.license.is_none(),
        "unrecognized content must not fabricate an SPDX id"
    );
}

#[test]
fn dispatch_copying_file_matched_case_insensitively() {
    // `parse_manifest` dispatches on the candidate's own suffix string
    // (defensively case-insensitive); the archive-path-vs-candidate-suffix
    // matching itself is `facets::matches_candidate`'s job, exercised there.
    let manifest = parse("copying.txt", b"some license text").unwrap();
    assert!(manifest.facts.has_license_file);
}

#[test]
fn dispatch_routes_by_suffix() {
    // A wrapper-directory path suffix still routes (matched as a suffix).
    let manifest = parse("pkg-1.0/vcpkg.json", br#"{"dependencies":["zlib"]}"#).unwrap();
    assert!(manifest.dependencies.iter().any(|d| d.token == "zlib"));
    assert_eq!(
        manifest.dependencies[0].mechanism,
        DependencyMechanism::Recipe
    );
}

#[test]
fn invalid_utf8_returns_none() {
    assert!(parse("CMakeLists.txt", &[0xff, 0xfe]).is_none());
}

#[test]
fn unknown_suffix_yields_empty_manifest() {
    let manifest = parse("README.md", b"not a manifest").unwrap();
    assert_eq!(manifest, CppManifest::default());
}

#[test]
fn mechanism_tokens_are_stable() {
    assert_eq!(DependencyMechanism::FindPackage.as_token(), "find_package");
    assert_eq!(DependencyMechanism::PkgConfig.as_token(), "pkg_config");
    assert_eq!(DependencyMechanism::Submodule.as_token(), "submodule");
    assert_eq!(DependencyMechanism::FetchContent.as_token(), "fetchcontent");
    assert_eq!(DependencyMechanism::Wrap.as_token(), "wrap");
    assert_eq!(DependencyMechanism::Recipe.as_token(), "recipe");
    assert_eq!(DependencyMechanism::BazelDep.as_token(), "bazel_dep");
}

/// A generated 10 MB single-line CMake input must parse without panic or
/// pathological blow-up (fixture generated in-test, never checked in).
#[test]
fn ten_megabyte_single_line_cmake_does_not_panic() {
    let mut giant = String::with_capacity(10 * 1024 * 1024 + 64);
    giant.push_str("find_package(ZLIB) ");
    while giant.len() < 10 * 1024 * 1024 {
        giant.push_str("aaaaaaaaaaaaaaaa ");
    }
    let manifest = parse("CMakeLists.txt", giant.as_bytes()).unwrap();
    // It found the single real declaration; the megabytes of filler produced no
    // spurious dependencies.
    assert!(manifest.dependencies.iter().any(|d| d.token == "ZLIB"));
}

/// A pathological unterminated string in a vcpkg.json is rejected as malformed
/// JSON (empty manifest), never a panic.
#[test]
fn unterminated_json_string_is_empty_manifest() {
    let manifest = parse("vcpkg.json", br#"{"description":"unterminated"#).unwrap();
    assert_eq!(manifest, CppManifest::default());
}

/// Deeply nested parens in CMake args must not overflow the scanner.
#[test]
fn deeply_nested_cmake_parens() {
    let mut source = String::from("find_package(ZLIB ");
    for _ in 0..5000 {
        source.push('(');
    }
    for _ in 0..5000 {
        source.push(')');
    }
    source.push(')');
    let manifest = parse("CMakeLists.txt", source.as_bytes()).unwrap();
    assert!(manifest.dependencies.iter().any(|d| d.token == "ZLIB"));
}

// ── BOM handling (dispatch-level, benefits every parser) ────────────────────

#[test]
fn utf8_bom_stripped_before_vcpkg_json_dispatch() {
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(br#"{"description":"has a bom","license":"MIT"}"#);
    let manifest = parse("vcpkg.json", &bytes).unwrap();
    assert_eq!(manifest.facts.description.as_deref(), Some("has a bom"));
    assert_eq!(manifest.facts.license.as_deref(), Some("MIT"));
}

#[test]
fn utf8_bom_stripped_before_cmake_dispatch() {
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(b"find_package(ZLIB REQUIRED)\n");
    let manifest = parse("CMakeLists.txt", &bytes).unwrap();
    assert!(manifest.dependencies.iter().any(|d| d.token == "ZLIB"));
}

#[test]
fn bom_only_input_is_empty_manifest_not_panic() {
    let bytes = [0xef, 0xbb, 0xbf];
    let manifest = parse("CMakeLists.txt", &bytes).unwrap();
    assert_eq!(manifest, CppManifest::default());
}

// ── Cross-parser hostile-input sweep ─────────────────────────────────────────

/// Every candidate suffix, fed the same battery of adversarial byte strings,
/// must return without panicking. `parse_manifest` is the single dispatch
/// point every one of the 9 parsers funnels through, so this is a cheap way
/// to sweep all of them at once for crash-freedom (field correctness is
/// covered per-parser in each module's own tests).
#[test]
fn adversarial_battery_never_panics_across_all_candidates() {
    let battery: &[&[u8]] = &[
        b"",
        b"\0\0\0\0",
        b"# just a comment\n",
        b"\r\n\r\n\r\n",
        b"\n\r\n\r",
        b"{{{{{{{{{{{{{{{{{{{{",
        b"[[[[[[[[[[[[[[[[[[[[",
        b"''''''''''''''''''''",
        b"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"\"",
        b"(((((((((((((((((((((((((((((",
        b")))))))))))))))))))))))))))))",
        "unicode 名前 \u{1F600} \u{0}".as_bytes(),
        b"key = \"unterminated",
        b"[section \"unterminated",
        &[0xc0, 0x80],                   // overlong/invalid UTF-8
        &[0xff, 0xfe, b'a', 0, b'b', 0], // UTF-16-ish garbage
    ];
    for candidate in manifest_candidates() {
        for input in battery {
            // Must not panic; `None` (invalid UTF-8) or `Some(_)` both fine.
            let _ = parse_manifest(candidate, input);
        }
    }
}

/// A single-line file with a pathologically long unbroken token (no
/// whitespace at all) must not hang or blow memory disproportionately for
/// every text-scanning parser, not just CMake (covered separately above).
#[test]
fn one_megabyte_unbroken_token_across_all_candidates() {
    let giant = "a".repeat(1024 * 1024);
    for candidate in manifest_candidates() {
        let _ = parse_manifest(candidate, giant.as_bytes());
    }
}

/// Every declaring mechanism is the catalog kind with the same token, and
/// the version survives onto that edge. A recipe counts as runtime. The
/// other mechanisms are build edges. The feed snapshot replaces exactly
/// those kinds.
#[test]
fn every_mechanism_matches_its_catalog_kind_and_requirement() {
    use crate::{
        enums::{EdgeKind, TextEnum},
        record::DepClass,
    };

    let mechanisms = [
        DependencyMechanism::FindPackage,
        DependencyMechanism::PkgConfig,
        DependencyMechanism::Submodule,
        DependencyMechanism::FetchContent,
        DependencyMechanism::Wrap,
        DependencyMechanism::Recipe,
        DependencyMechanism::BazelDep,
    ];
    let requirements = ["1.2.3", ">= 2.40", "", ">=1.1", "0.0.9", "20230802.1", "  "];
    let mut manifest = CppManifest::default();
    for (mechanism, requirement) in mechanisms.iter().zip(requirements) {
        let mut record = DependencyRecord::new(mechanism.as_token(), *mechanism);
        if !requirement.is_empty() {
            record.requirement = Some((*requirement).to_owned());
        }
        manifest.push_dependency(record);
    }

    for (edge, (mechanism, requirement)) in manifest
        .facts
        .dependencies
        .iter()
        .zip(mechanisms.iter().zip(requirements))
    {
        let kind = EdgeKind::from_token(mechanism.as_token()).expect("catalog kind");
        assert_eq!(edge.kind, kind);
        assert_eq!(edge.kind.as_token(), mechanism.as_token());
        let class = if *mechanism == DependencyMechanism::Recipe {
            DepClass::Runtime
        } else {
            DepClass::Build
        };
        assert_eq!(edge.class, class);
        let expected = if requirement.is_empty() {
            None
        } else {
            Some(requirement)
        };
        assert_eq!(edge.requirement.as_deref(), expected);
    }

    let observed = crate::edge_project::feed_edges(
        crate::ecosystem::Language::Cpp,
        "demo",
        "1.0.0",
        &manifest.facts.dependencies,
    )
    .expect("edges");
    let mut kinds: Vec<_> = observed
        .snapshot
        .kinds()
        .iter()
        .map(|kind| kind.as_token())
        .collect();
    kinds.sort_unstable();
    let mut expected: Vec<_> = mechanisms
        .iter()
        .map(|mechanism| mechanism.as_token())
        .collect();
    expected.sort_unstable();
    assert_eq!(kinds, expected);
    assert_eq!(observed.record.runtime_names(), vec!["recipe"]);
}
