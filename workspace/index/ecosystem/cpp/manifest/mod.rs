//! `cpp` manifest extraction (REGISTRYLESS §8).
//!
//! Seven pure text-scanning parsers, one per build-descriptor family, plus a
//! conservative `LICENSE`/`LICENCE`/`COPYING` file sniffer
//! ([`crate::ecosystem::license`]) — eight dispatch destinations total. Each
//! yields a [`CppManifest`] carrying the shared [`ExtractedFacts`] plus typed
//! dependency records `(token, mechanism)`. Parsers never execute code
//! (conanfile.py is scanned as text, never run), never panic, and log-not-fatal
//! on partial input.
//!
//! `CMakeLists.txt` and a bare `.pc`/`.pc.in` file carry no license
//! *expression* field of their own (only `has_license_file`, from a
//! `CPACK_RESOURCE_FILE_LICENSE` var or nothing at all) — a repo that ships
//! one of those plus a standalone `LICENSE` file used to get **no** license
//! at all, because `server::coordination::indexing::facets::extract_facets`
//! stopped at the first manifest that parsed. That call site now merges
//! across every matching candidate (`ExtractedFacts::merge`), so the license
//! candidates below are finally reachable for those manifest shapes.

use crate::ecosystem::{
    license,
    manifest::{ExtractedFacts, ManifestCandidate, ManifestFacts},
};

pub mod cmake;
pub mod conan;
pub mod gitmodules;
pub mod meson;
pub mod modulebazel;
pub mod pkgconfig;
pub mod vcpkg;

#[cfg(test)]
mod tests;

/// How a `cpp` dependency was declared. This is the catalog
/// [`crate::enums::EdgeKind`]; parsers do not keep a second vocabulary.
pub type DependencyMechanism = crate::enums::EdgeKind;

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
    /// Version expression the manifest wrote beside the token.
    pub requirement: Option<String>,
}

impl DependencyRecord {
    /// Construct a record from a token and mechanism.
    pub fn new(token: impl Into<String>, mechanism: DependencyMechanism) -> Self {
        Self {
            token: token.into(),
            mechanism,
            requirement: None,
        }
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
    ///
    /// Catalog edges appear when the manifest is folded through
    /// [`ManifestFacts::into_facts`].
    pub fn from_dependencies(dependencies: Vec<DependencyRecord>) -> Self {
        Self {
            facts: ExtractedFacts::default(),
            dependencies,
        }
    }

    /// Push a typed dependency record. The catalog edge is projected later.
    pub fn push_dependency(&mut self, record: DependencyRecord) {
        self.dependencies.push(record);
    }
}

fn project_edge(record: &DependencyRecord) -> crate::record::DepEdge {
    let mut edge = crate::record::DepEdge::runtime(record.token.clone());
    edge.kind = record.mechanism;
    edge.class = crate::engine::class_of_kind(record.mechanism);
    if let Some(requirement) = record
        .requirement
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        edge.requirement = Some(requirement.into());
    }
    edge
}

impl ManifestFacts for CppManifest {
    fn into_facts(self) -> ExtractedFacts {
        let mut facts = self.facts;
        facts.dependencies = self.dependencies.iter().map(project_edge).collect();
        facts
    }
}

/// The manifest candidates for `cpp`, in priority order (REGISTRYLESS §6.3).
///
/// The `LICENSE`/`LICENCE`/`COPYING` candidates are last: they're a
/// heuristic content sniff (`license::detect_spdx`), strictly less
/// authoritative than an explicit `license = "..."` expression a
/// higher-priority build descriptor (`vcpkg.json`, `conanfile.*`,
/// `meson.build`) may already have supplied — the merge in `facets.rs`
/// leaves an already-set `license` field alone, so ordering here is what
/// gives the declared expression precedence over the sniffed one. Kept in
/// sync with [`license::LICENSE_FILENAMES`] — see the
/// `license_candidates_match_shared_list` test.
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
        ManifestCandidate::new("LICENSE"),
        ManifestCandidate::new("LICENSE.txt"),
        ManifestCandidate::new("LICENSE.md"),
        ManifestCandidate::new("LICENCE"),
        ManifestCandidate::new("LICENCE.txt"),
        ManifestCandidate::new("LICENCE.md"),
        ManifestCandidate::new("COPYING"),
        ManifestCandidate::new("COPYING.txt"),
    ];
    CANDIDATES
}

/// Dispatch a manifest candidate to the matching parser (REGISTRYLESS §8).
/// Returns `None` only when the bytes are not valid UTF-8; otherwise a manifest
/// (possibly empty) so facet extraction degrades to name-only, never fails.
pub fn parse_manifest(candidate: &ManifestCandidate, bytes: &[u8]) -> Option<CppManifest> {
    let text = std::str::from_utf8(bytes).ok()?;
    // Strip a leading UTF-8 BOM (`\u{FEFF}`) — common on files saved by
    // Windows-native editors (vcpkg.json, CMakeLists.txt in particular).
    // Left in place, it corrupts the first token every one of these
    // hand-rolled scanners looks for (a leading `{` for JSON, a command name
    // for CMake/meson/Bazel, a `[` for `.gitmodules`/`.txt`), so every field
    // silently comes back empty instead of populated.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
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
    } else if std::path::Path::new(suffix)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pc"))
        || suffix.ends_with(".pc.in")
    {
        pkgconfig::parse(text)
    } else if license::LICENSE_FILENAMES
        .iter()
        .any(|f| suffix.eq_ignore_ascii_case(f))
    {
        // A standalone license file carries no dependency/description/
        // homepage information — only ever `has_license_file` (always true,
        // the file exists and is non-empty-check-free: an empty LICENSE file
        // is still a declared intent) and a best-effort sniffed `license`.
        CppManifest {
            facts: ExtractedFacts {
                has_license_file: true,
                license: license::detect_spdx(text),
                ..ExtractedFacts::default()
            },
            dependencies: Vec::new(),
        }
    } else {
        CppManifest::default()
    };
    Some(manifest)
}
