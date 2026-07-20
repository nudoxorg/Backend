//! Coordinator-level manifest dispatch + hostile-input stress tests.
//! Per-parser coverage lives in each parser module's own `tests`.

use super::*;
use crate::manifest::ManifestCandidate;

fn parse(suffix: &'static str, bytes: &[u8]) -> Option<CppManifest> {
	parse_manifest(&ManifestCandidate::new(suffix), bytes)
}

#[test]
fn candidate_order_is_stable() {
	let candidates = manifest_candidates();
	let suffixes: Vec<&str> = candidates.iter().map(|c| c.path_suffix).collect();
	assert_eq!(
		suffixes,
		vec![
			"vcpkg.json",
			"conanfile.py",
			"conanfile.txt",
			"CMakeLists.txt",
			"meson.build",
			"MODULE.bazel",
			".gitmodules",
			".pc.in",
			".pc",
		]
	);
}

#[test]
fn dispatch_routes_by_suffix() {
	// A wrapper-directory path suffix still routes (matched as a suffix).
	let manifest = parse("pkg-1.0/vcpkg.json", br#"{"dependencies":["zlib"]}"#).unwrap();
	assert!(manifest.dependencies.iter().any(|d| d.token == "zlib"));
	assert_eq!(manifest.dependencies[0].mechanism, DependencyMechanism::Recipe);
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
