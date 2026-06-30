//! Pipeline part: **PEP 440 version resolution** for external Python deps
//! (`compiler::languages::python::traversal`).
//!
//! These tests are fully OFFLINE: they feed fixture release metadata / version
//! lists / git tags to the pure selection logic and assert the chosen version.
//! Nothing here hits PyPI. (The network functions in `traversal` —
//! `fetch_pypi_metadata`, `download_and_extract_sdist`, `resolve_and_fetch_sdist`
//! — are integration-only and intentionally have no live test here.)

use std::str::FromStr;

use compiler::languages::python::traversal::{
    parse_releases, resolve_version_from_tags, select_best_release, select_best_version,
};
use uv_pep440::{Version, VersionSpecifiers};

fn v(s: &str) -> Version {
    Version::from_str(s).unwrap_or_else(|e| panic!("bad version `{s}`: {e}"))
}

fn req(s: &str) -> VersionSpecifiers {
    VersionSpecifiers::from_str(s).unwrap_or_else(|e| panic!("bad specifier `{s}`: {e}"))
}

#[test]
fn picks_newest_satisfying_stable_version() {
    let versions = [
        v("1.0.0"),
        v("1.2.0"),
        v("1.3.0"),
        v("1.4.0rc1"), // prerelease — must NOT be chosen over 1.3.0
        v("2.0.0"),    // out of range
    ];
    let chosen = select_best_version(&versions, &req(">=1.2,<2")).expect("a version should match");
    assert_eq!(chosen, v("1.3.0"), "should pick newest in-range STABLE version");
}

#[test]
fn excludes_out_of_range_and_returns_none_when_nothing_matches() {
    let versions = [v("1.0.0"), v("1.1.0")];
    assert!(
        select_best_version(&versions, &req(">=2,<3")).is_none(),
        "no version in [2,3) exists"
    );
}

#[test]
fn falls_back_to_prerelease_only_when_no_stable_satisfies() {
    // Specifier operand is itself a prerelease, so prereleases are admissible
    // and there is no stable candidate — selection must return the newest pre.
    let versions = [v("1.5.0rc1"), v("1.5.0rc2")];
    let chosen =
        select_best_version(&versions, &req(">=1.5.0rc1,<2")).expect("a prerelease should match");
    assert_eq!(chosen, v("1.5.0rc2"), "newest satisfying prerelease");
}

#[test]
fn parses_pypi_metadata_and_selects_the_sdist_of_the_best_version() {
    // Minimal PyPI JSON shape: a `releases` map of version -> [artifacts].
    let metadata = serde_json::json!({
        "info": { "name": "demo" },
        "releases": {
            "1.0.0": [
                { "filename": "demo-1.0.0.tar.gz", "url": "https://files/demo-1.0.0.tar.gz",
                  "packagetype": "sdist", "yanked": false }
            ],
            "1.2.0": [
                { "filename": "demo-1.2.0-py3-none-any.whl", "url": "https://files/demo-1.2.0.whl",
                  "packagetype": "bdist_wheel", "yanked": false },
                { "filename": "demo-1.2.0.tar.gz", "url": "https://files/demo-1.2.0.tar.gz",
                  "packagetype": "sdist", "yanked": false }
            ],
            "1.3.0": [
                // 1.3.0 only has a YANKED sdist -> 1.3.0 must be skipped.
                { "filename": "demo-1.3.0.tar.gz", "url": "https://files/demo-1.3.0.tar.gz",
                  "packagetype": "sdist", "yanked": true }
            ],
            "not-a-version": [
                { "filename": "junk", "url": "x", "packagetype": "sdist", "yanked": false }
            ]
        }
    });

    let releases = parse_releases(&metadata);
    // The non-PEP440 "not-a-version" key is dropped; 4 valid artifacts remain.
    assert_eq!(releases.len(), 4, "parsed releases: {releases:#?}");

    let chosen = select_best_release(&releases, &req(">=1.0"))
        .expect("an sdist should be selectable");
    assert_eq!(chosen.version, v("1.2.0"), "1.3.0's only sdist is yanked, so 1.2.0 wins");
    assert_eq!(chosen.package_type, "sdist");
    assert_eq!(chosen.filename, "demo-1.2.0.tar.gz");
    assert!(chosen.url.ends_with("demo-1.2.0.tar.gz"));
}

#[test]
fn resolves_version_from_git_tags_stripping_leading_v() {
    let tags = ["v1.0.0", "v1.2.0", "1.3.0", "v2.0.0", "v1.4.0rc1"];
    // Returns the ORIGINAL tag string so the caller can check it out.
    let chosen = resolve_version_from_tags(&tags, &req(">=1.2,<2")).expect("a tag should match");
    assert_eq!(chosen, "1.3.0", "newest in-range stable tag, original form preserved");
}

#[test]
fn git_tag_resolution_returns_none_when_no_tag_in_range() {
    let tags = ["v0.9.0", "v3.0.0"];
    assert!(resolve_version_from_tags(&tags, &req(">=1,<2")).is_none());
}
