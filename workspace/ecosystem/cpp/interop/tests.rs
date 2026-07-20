//! Tests for the derived purl / SWHID interop renderings.

use super::*;
use crate::cpp::name::parse_name;

#[test]
fn github_purl_uses_github_type() {
	let stem = parse_name("https://github.com/madler/zlib").unwrap();
	assert_eq!(purl(&stem, "v1.3.1", None), "pkg:github/madler/zlib@v1.3.1");
}

#[test]
fn non_github_purl_is_generic_with_vcs_url() {
	let stem = parse_name("https://gitlab.com/libeigen/eigen").unwrap();
	let rendered = purl(&stem, "3.4.0", Some("deadbeef"));
	assert_eq!(
		rendered,
		"pkg:generic/eigen@3.4.0?vcs_url=https://gitlab.com/libeigen/eigen.git@deadbeef"
	);
}

#[test]
fn non_github_purl_without_rev() {
	let stem = parse_name("https://gitlab.freedesktop.org/cairo/cairo").unwrap();
	assert_eq!(
		purl(&stem, "1.18.0", None),
		"pkg:generic/cairo@1.18.0?vcs_url=https://gitlab.freedesktop.org/cairo/cairo.git"
	);
}

#[test]
fn swhid_rev_format() {
	assert_eq!(swhid_rev("94a9c0d4d1a3c1e3"), "swh:1:rev:94a9c0d4d1a3c1e3");
}
