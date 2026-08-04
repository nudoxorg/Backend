//! `cpp` manifest extraction (REGISTRYLESS §8).
//!
//! Seven pure text-scanning parsers, one per build-descriptor family. Each
//! yields a [`CppManifest`] carrying the shared [`ExtractedFacts`] plus typed
//! dependency records `(token, mechanism)`. Parsers never execute code
//! (conanfile.py is scanned as text, never run), never panic, and log-not-fatal
//! on partial input.

use crate::ecosystem::manifest::{ExtractedFacts, ManifestCandidate, ManifestFacts};

pub mod cmake;
pub mod conan;
pub mod gitmodules;
pub mod meson;
pub mod modulebazel;
pub mod pkgconfig;
pub mod vcpkg;

#[cfg(test)]
mod tests;

/// The mechanism by which a `cpp` dependency is declared — the `edges.kind`
/// vocabulary (REGISTRYLESS RL-5). Recorded exactly as the producer saw it
/// (EDB); resolution to a stem happens later (IDB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyMechanism {
	/// CMake `find_package(<Name>)`.
	FindPackage,
	/// pkg-config (`pkg_check_modules`, `.pc` `Requires:`, meson `dependency()`).
	PkgConfig,
	/// A git submodule (`.gitmodules`).
	Submodule,
	/// CMake `FetchContent_Declare(<n> GIT_REPOSITORY …)`.
	FetchContent,
	/// A meson subproject / wrap (`subproject('<n>')`).
	Wrap,
	/// A vcpkg / Conan recipe dependency.
	Recipe,
	/// A Bazel `bazel_dep(name, version)`.
	BazelDep,
}

impl DependencyMechanism {
	/// The stable lowercase token stored in `edges.kind`.
	pub const fn as_token(self) -> &'static str {
		match self {
			DependencyMechanism::FindPackage => "find_package",
			DependencyMechanism::PkgConfig => "pkg_config",
			DependencyMechanism::Submodule => "submodule",
			DependencyMechanism::FetchContent => "fetchcontent",
			DependencyMechanism::Wrap => "wrap",
			DependencyMechanism::Recipe => "recipe",
			DependencyMechanism::BazelDep => "bazel_dep",
		}
	}
}

/// One extracted dependency: the literal requirement token and the mechanism
/// that introduced it (REGISTRYLESS §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyRecord {
	/// The literal token as written in the manifest (`zlib`, `ZLIB`,
	/// `libpng16`, a repo slug for FetchContent/submodule). Never normalized
	/// here — that is the resolution pass's job (RL-5).
	pub token: String,
	/// How the dependency was declared.
	pub mechanism: DependencyMechanism,
}

impl DependencyRecord {
	/// Construct a record from a token and mechanism.
	pub fn new(token: impl Into<String>, mechanism: DependencyMechanism) -> Self {
		Self { token: token.into(), mechanism }
	}
}

/// A parsed `cpp` manifest: shared facets plus typed dependency records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CppManifest {
	/// Shared, ecosystem-erased facets (description, license, homepage,
	/// dependency name list).
	pub facts: ExtractedFacts,
	/// Typed dependency records with their declaring mechanism.
	pub dependencies: Vec<DependencyRecord>,
}

impl CppManifest {
	/// A manifest carrying only typed dependency records (no facets).
	pub fn from_dependencies(dependencies: Vec<DependencyRecord>) -> Self {
		let facts = ExtractedFacts {
			dependencies: dependencies.iter().map(|d| d.token.clone()).collect(),
			..ExtractedFacts::default()
		};
		Self { facts, dependencies }
	}

	/// Push a typed dependency record, keeping the erased `facts.dependencies`
	/// token mirror in sync.
	pub fn push_dependency(&mut self, record: DependencyRecord) {
		self.facts.dependencies.push(record.token.clone());
		self.dependencies.push(record);
	}
}

impl ManifestFacts for CppManifest {
	fn into_facts(self) -> ExtractedFacts { self.facts }
}

/// The manifest candidates for `cpp`, in priority order (REGISTRYLESS §6.3).
pub fn manifest_candidates() -> &'static [ManifestCandidate] {
	static CANDIDATES: &[ManifestCandidate] = &[
		ManifestCandidate::new("vcpkg.json"),
		ManifestCandidate::new("conanfile.py"),
		ManifestCandidate::new("conanfile.txt"),
		ManifestCandidate::new("CMakeLists.txt"),
		ManifestCandidate::new("meson.build"),
		ManifestCandidate::new("MODULE.bazel"),
		ManifestCandidate::new(".gitmodules"),
		// `*.pc.in` matched before `*.pc` so the template wins the suffix race
		// (a repo usually ships one or the other).
		ManifestCandidate::new(".pc.in"),
		ManifestCandidate::new(".pc"),
	];
	CANDIDATES
}

/// Dispatch a manifest candidate to the matching parser (REGISTRYLESS §8).
/// Returns `None` only when the bytes are not valid UTF-8; otherwise a manifest
/// (possibly empty) so facet extraction degrades to name-only, never fails.
pub fn parse_manifest(candidate: &ManifestCandidate, bytes: &[u8]) -> Option<CppManifest> {
	let text = std::str::from_utf8(bytes).ok()?;
	let suffix = candidate.path_suffix;
	let manifest = if suffix.ends_with("vcpkg.json") {
		vcpkg::parse(text)
	} else if suffix.ends_with("conanfile.py") || suffix.ends_with("conanfile.txt") {
		conan::parse(text)
	} else if suffix.ends_with("CMakeLists.txt") {
		cmake::parse(text)
	} else if suffix.ends_with("meson.build") {
		meson::parse(text)
	} else if suffix.ends_with("MODULE.bazel") {
		modulebazel::parse(text)
	} else if suffix.ends_with(".gitmodules") {
		gitmodules::parse(text)
	} else if suffix.ends_with(".pc") || suffix.ends_with(".pc.in") {
		pkgconfig::parse(text)
	} else {
		CppManifest::default()
	};
	Some(manifest)
}
