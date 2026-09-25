//! Emit-time metadata: listing signals, download counts, and search facets.

#[allow(unused_imports)]
use crate::server::registry;
use crate::{
    ecosystem::{DynSpec, LanguageExt},
    server::registry::{
        blob::{BlobManifest, FileEntry, creation::PendingSection},
        identity::PackageCoordinates,
        metadata::{
            SearchFacets,
            rich::{self, ExtractionInput},
        },
    },
};

/// Fetch the registry listing body and parse release/freshness signals.
///
/// Non-fatal: returns `None` on any failure so emit still succeeds.
/// Tuple: `(total, withdrawn, this_version_withdrawn, last_release_days_ago)`.
pub(super) async fn fetch_listing_signals(
    coordinates: &PackageCoordinates,
    client: &registry::upstream::UpstreamClient,
) -> Option<(u32, u32, bool, Option<u32>)> {
    use crate::{
        ecosystem::LanguageExt,
        server::registry::search::listing_signals::listing_signals_from_body,
    };

    let spec = coordinates.ecosystem().spec();
    let name = coordinates.name.canonical();
    let version = coordinates.version.canonical();

    // Prefer reusing the absolute download/listing URL when available (crates.io,
    // npm packument patterns). Relative templates alone need an origin we don't
    // always have at this call site.
    let full_url = if let Some(dl) = spec.download_source() {
        let url = dl.url.replace("{name}", &name);
        // crates.io download source *is* the listing JSON — use it for signals.
        if coordinates.ecosystem() == crate::ecosystem::Language::Rust {
            url
        } else {
            // npm downloads API is not the packument; use known packument/JSON URLs.
            match coordinates.ecosystem() {
                crate::ecosystem::Language::Typescript => {
                    format!("https://registry.npmjs.org/{name}")
                }
                crate::ecosystem::Language::Python => {
                    format!("https://pypi.org/pypi/{name}/json")
                }
                _ => url,
            }
        }
    } else {
        match coordinates.ecosystem() {
            crate::ecosystem::Language::Python => format!("https://pypi.org/pypi/{name}/json"),
            crate::ecosystem::Language::Typescript => format!("https://registry.npmjs.org/{name}"),
            _ => return None,
        }
    };

    let body = match client.get(coordinates.ecosystem(), &full_url).await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(
                package = %name,
                ecosystem = ?coordinates.ecosystem(),
                error = %e,
                "listing signals fetch failed"
            );
            return None;
        }
    };
    let signals = listing_signals_from_body(
        coordinates.ecosystem(),
        &body,
        version.as_str(),
        chrono::Utc::now(),
    )?;
    tracing::debug!(
        package = %name,
        total = signals.total,
        withdrawn = signals.withdrawn,
        days = ?signals.last_release_days_ago,
        "listing signals fetched"
    );
    Some((
        signals.total,
        signals.withdrawn,
        signals.this_version_withdrawn,
        signals.last_release_days_ago,
    ))
}

/// Fetch the download count for `coordinates` from the ecosystem's
/// [`DownloadEndpoint`](crate::ecosystem::upstream::DownloadEndpoint) (if any)
/// and store it in `facets.downloads`.
///
/// Ecosystems with a source today: TypeScript (npm downloads API), C# (NuGet
/// search `totalDownloads`), Rust (crates.io listing `crate.recent_downloads`).
/// Others return `download_source() = None` and leave `downloads` unset so
/// ranking uses the fairness floor (and/or corpus `dependents`).
///
/// Non-fatal: any network/parse failure is logged at debug level and the
/// `downloads` field is left as `None`. Never fails ingest. Explicit zero
/// counts from a successful parse are stored as `Some(0)`.
pub(super) async fn fetch_and_set_downloads(
    coordinates: &PackageCoordinates,
    facets: &mut SearchFacets,
    client: &registry::upstream::UpstreamClient,
) {
    let spec = coordinates.ecosystem().spec();
    let Some(endpoint) = spec.download_source() else {
        return; // ecosystem has no download-count API — fairness floor handles it
    };
    let name = coordinates.name.canonical();
    let url = endpoint.url.replace("{name}", name);
    match client.get(coordinates.ecosystem(), &url).await {
        Ok(body) => match spec.parse_download_count(&body) {
            Some(count) => {
                facets.downloads = Some(count);
                tracing::debug!(
                    package = %coordinates.name.canonical(),
                    ecosystem = ?coordinates.ecosystem(),
                    downloads = count,
                    "download count fetched"
                );
            }
            None => {
                tracing::debug!(
                    package = %coordinates.name.canonical(),
                    ecosystem = ?coordinates.ecosystem(),
                    "download count endpoint returned unparseable body"
                );
            }
        },
        Err(e) => {
            tracing::debug!(
                package = %coordinates.name.canonical(),
                ecosystem = ?coordinates.ecosystem(),
                error = %e,
                "download count fetch failed (non-fatal)"
            );
        }
    }
}

/// Build the [`SearchFacets`] for a freshly-emitted snapshot from its manifest
/// files and their still-in-memory `sections` bytes.
///
/// Ecosystem-generic: manifest discovery iterates `spec.manifest_candidates()`
/// in priority order, case-insensitively matched against snapshot entries,
/// preferring root or one-wrapper-dir paths. Parsing is delegated to
/// `spec.extract_facts()`. README discovery is root-first across all
/// ecosystems. `loc` is a cheap honest newline count of source-file sections.
/// Category mapping (`spec.search_norms().map_category`) is applied inside
/// `spec.extract_facts()` for ecosystems that do so at parse time (Python trove
/// classifiers); for ecosystems whose native categories ARE the shared taxonomy
/// (Rust), `extract_facts` passes them through via `map_internal_category`.
/// Either way the facts arrive already mapped into
/// `ExtractedFacts::categories`.
///
/// Non-fatal by design: any missing/unparseable input degrades to a minimal
/// name-only facet set (or `None`), so ingest never fails because search
/// metadata could not be derived.
///
/// `listing` is release/freshness signals observed from the registry listing
/// body (or resolve): `(total, withdrawn, this_withdrawn,
/// last_release_days_ago)`. Pass `None` when no listing fetch is available.
pub(super) struct FacetExtract {
    pub facets: SearchFacets,
    /// Manifest edges, including requirement and optional. Facet slugs stay
    /// on [`SearchFacets::dependencies`] for search.
    pub edges: Vec<crate::record::DepEdge>,
}

pub(super) fn extract_facets(
    coordinates: &PackageCoordinates,
    manifest: &BlobManifest,
    sections: &[PendingSection],
    identifiers: &[String],
    heuristics: Option<&crate::server::Heuristics>,
    listing: Option<(u32, u32, bool, Option<u32>)>,
) -> Option<FacetExtract> {
    let (synonyms, specifics) = match heuristics {
        Some(h) => (Some(h.synonyms()), Some(h.specifics())),
        None => (None, None),
    };

    let spec: &'static dyn DynSpec = coordinates.ecosystem().spec();
    let norms = spec.search_norms();
    let name = coordinates.name.canonical().to_owned();

    // ── Manifest discovery ────────────────────────────────────────────────────
    // Bytes of a manifest file are fetched from `sections` by content hash — the
    // same hash the `FileEntry` records — so no post-emit blob round-trip is
    // needed.
    let file_bytes = |entry: &FileEntry| -> Option<bytes::Bytes> {
        let section = sections.iter().find(|s| s.hash == entry.hash)?;
        Some(section.bytes.clone())
    };

    // Path matches `suffix` case-insensitively, at root or under a single
    // wrapper dir (e.g. `pkg-1.0/Cargo.toml` is depth-1, which is fine).
    let matches_candidate = |path: &str, suffix: &str| -> bool {
        let path_lower = path.to_ascii_lowercase();
        let suffix_lower = suffix.to_ascii_lowercase();
        // Exact match (root).
        if path_lower == suffix_lower {
            return true;
        }
        // One wrapper dir: path ends with `/<suffix>` and has no further `/`.
        if let Some(stripped) = path_lower.strip_suffix(&format!("/{suffix_lower}")) {
            return !stripped.contains('/');
        }
        false
    };

    let facts = {
        // Merge across *every* matching candidate, in priority order — not just
        // the first one that parses. Ecosystems with a single manifest kind
        // (Rust, TypeScript, C#, Java) are unaffected: the loop below still only
        // ever finds one match, so the merge is a no-op and the result is
        // byte-identical to the old first-match-wins behavior. Ecosystems with
        // several manifest kinds (C/C++'s 9, Python's 3, Go's now 9 counting
        // LICENSE candidates) get real fact-merging: a higher-priority manifest's
        // fields win, a lower-priority one only fills gaps — see
        // `ExtractedFacts::merge` for the exact per-field-kind policy.
        //
        // Cost: one file read/parse attempt per matching candidate rather than
        // stopping at the first success, so packages whose earlier candidates
        // parse but leave later ones unread now pay for those later reads too.
        // The per-candidate scan itself is unchanged (linear in `manifest.files`);
        // this only changes how many candidates are visited to completion.
        let candidates = spec.manifest_candidates();
        let mut merged = crate::ecosystem::manifest::ExtractedFacts::default();
        let mut any_found = false;
        for candidate in candidates {
            for entry in &manifest.files {
                if matches_candidate(entry.path.as_str(), candidate.path_suffix)
                    && let Some(bytes) = file_bytes(entry)
                    && let Some(parsed) = spec.extract_facts(candidate, &bytes)
                {
                    tracing::debug!(
                        package = %manifest.package,
                        manifest = entry.path.as_str(),
                        "manifest parsed"
                    );
                    merged.merge(parsed);
                    any_found = true;
                    // One successful parse per candidate; move on to the next
                    // candidate rather than re-scanning remaining entries for
                    // another file matching the same suffix.
                    break;
                }
            }
        }
        if any_found {
            merged
        } else {
            tracing::debug!(
                package = %manifest.package,
                ecosystem = ?coordinates.ecosystem(),
                "no manifest found; name-only facets"
            );
            crate::ecosystem::manifest::ExtractedFacts::default()
        }
    };

    // ── README discovery ──────────────────────────────────────────────────────
    // Root-first: README.md / README.rst / README.txt / README (case-insensitive),
    // else use the manifest-declared readme_hint path.
    let readme_text = {
        let readme_leaf_matches = |path: &str| -> bool {
            let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
            (leaf == "readme.md"
                || leaf == "readme.rst"
                || leaf == "readme.txt"
                || leaf == "readme")
                && path.matches('/').count() <= 1
        };
        let entry = manifest
            .files
            .iter()
            .find(|e| readme_leaf_matches(e.path.as_str()));
        let entry = entry.or_else(|| {
            facts.readme_hint.as_deref().and_then(|hint| {
                manifest
                    .files
                    .iter()
                    .find(|e| e.path.as_str().eq_ignore_ascii_case(hint))
            })
        });
        entry
            .and_then(file_bytes)
            .and_then(|b| String::from_utf8(b.to_vec()).ok())
    };

    // ── LOC count ─────────────────────────────────────────────────────────────
    // Cheap honest count: newlines across every source-file section already in
    // `sections`. Capped at u32::MAX.
    let loc: u32 = {
        let total: u64 = sections
            .iter()
            .map(|s| s.bytes.iter().filter(|&&b| b == b'\n').count() as u64)
            .sum();
        total.min(u32::MAX as u64) as u32
    };

    // ── Build ExtractionInput + run rich extraction ───────────────────────────
    let (release_count, withdrawn_count, last_release_days_ago) = match listing {
        Some((total, withdrawn, _, days)) => (Some(total), Some(withdrawn), days),
        None => (None, None, None),
    };

    let dependency_names = facts.dependency_names();
    let input = ExtractionInput {
        name: &name,
        description: facts.description.as_deref(),
        manifest_keywords: &facts.keywords,
        manifest_categories: &facts.categories,
        readme: readme_text.as_deref(),
        identifiers,
        dependencies: &dependency_names,
        has_repository: facts.repository.is_some(),
        has_documentation: facts.documentation,
        has_license: facts.license.is_some() || facts.has_license_file,
        loc,
        release_count,
        withdrawn_count,
        last_release_days_ago,
    };

    let rich = rich::extract(&input, norms, synonyms, specifics);
    let mut facets = SearchFacets::from_rich(&rich);
    // Search keywords may mention every manifest name. The stored facet list
    // is only the runtime edges, so it agrees with the catalog.
    facets.dependencies = crate::record::runtime_facet_names(&facts.dependencies);

    // Propagate the manifest description into the facet row (S2).
    facets.description = facts.description.map(smol_str::SmolStr::from);

    // Propagate repository slug, license, and release stats into facets.
    facets.repo_slug = facts
        .repository
        .as_deref()
        .and_then(crate::ecosystem::repo::normalize_repo_url)
        .map(|slug| smol_str::SmolStr::from(slug.as_str()));

    facets.license = facts
        .license
        .as_deref()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| smol_str::SmolStr::from(l.to_ascii_lowercase()));

    // Keyword extraction saw every manifest name. `facets.dependencies` was
    // replaced with the runtime edge names above.

    if let Some((total, withdrawn, this_withdrawn, _)) = listing {
        facets.release_count = Some(total);
        facets.withdrawn_count = Some(withdrawn);
        facets.withdrawn = this_withdrawn;
    }

    // verified_repo soft signal from name + repo slug.
    facets.verified_repo =
        crate::server::registry::search::gates::verified_repo(&name, facets.repo_slug.as_deref());

    // Automatic squat / land-grab heuristic (quality-gated; never flags mature
    // pkgs).
    facets.squat_suspect = crate::server::registry::search::squat::is_squat_suspect(
        crate::server::registry::search::squat::SquatInput {
            name: &name,
            quality: facets.quality(),
            downloads: facets.downloads,
            release_count: facets.release_count,
            description: facets.description.as_deref(),
            has_repository: facets.repo_slug.is_some(),
        },
    );

    let edges = facts.dependencies.clone();
    Some(FacetExtract { facets, edges })
}

// ── Tests ────────────────────────────────────────────────────────────────
//
// These exercise `extract_facets`'s manifest-merge behavior end to end (real
// `BlobBuilder`/`BlobManifest`/`PendingSection` plumbing, no mocking of the
// candidate-matching or parsing) — the multi-candidate-merge change is the
// point of this test module, so it goes through the exact code path
// `execute_emit_phase` (`mod.rs`, out of scope for this change) calls.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ecosystem::PackageNameExt as _,
        server::registry::{
            blob::BlobBuilder,
            package::{Coordinates, PackageName},
        },
    };
    use heart::{Language, PackageVersion, RegistryOrigin, Toolchain};

    /// Build a minimal, valid `(BlobManifest, Vec<PendingSection>)` from
    /// `(path, bytes)` file entries, attaching empty-but-valid IR/reference
    /// sections so `finalize()` succeeds without pulling in the cage/IR
    /// pipeline this module doesn't otherwise depend on.
    fn build_manifest(
        toolchain: Toolchain,
        files: &[(&'static str, &'static [u8])],
    ) -> (BlobManifest, Vec<PendingSection>) {
        let package = heart::PackageId::from_uuid(uuid::Uuid::from_bytes([9u8; 16]));
        let mut builder = BlobBuilder::new(package, toolchain);
        for (path, bytes) in files {
            builder
                .push_file(
                    smol_str::SmolStr::new(path),
                    bytes::Bytes::from_static(bytes),
                )
                .expect("push file");
        }
        super::super::ir_stream::attach_empty_ir_sections(&mut builder);
        builder.finalize().expect("finalize")
    }

    fn cpp_coordinates(name: &str) -> PackageCoordinates {
        Coordinates {
            origin: RegistryOrigin::Git,
            name: PackageName::new(Language::Cpp, name).expect("fixture cpp name"),
            version: PackageVersion::try_from((Language::Cpp, "1.0.0"))
                .expect("fixture cpp version"),
        }
    }

    fn rust_coordinates(name: &str) -> PackageCoordinates {
        Coordinates {
            origin: RegistryOrigin::CratesIo,
            name: PackageName::new(Language::Rust, name).expect("fixture rust name"),
            version: PackageVersion::try_from((Language::Rust, "1.0.0"))
                .expect("fixture rust version"),
        }
    }

    const CMAKE_WITH_NO_LICENSE: &[u8] =
        br#"project(demo VERSION 1.0 DESCRIPTION "A demo C++ library" LANGUAGES CXX)
set(CMAKE_PROJECT_HOMEPAGE_URL "https://example.com/demo")
"#;

    const MIT_LICENSE_TEXT: &[u8] = b"MIT License\n\n\
Permission is hereby granted, free of charge, to any person obtaining a copy of this software \
and associated documentation files (the \"Software\"), to deal in the Software without \
restriction, including without limitation the rights to use, copy, modify...";

    #[test]
    fn cpp_cmake_plus_license_merges_a_license_cmake_alone_never_had() {
        // CMakeLists.txt alone sets description/repository/documentation but
        // never a `license` expression (only meson/vcpkg/conan do) — the bug
        // this task fixes: a CMake-only C/C++ repo with a separate LICENSE
        // file used to end up with `license: None` because facets.rs stopped
        // at the first manifest that parsed (CMakeLists.txt, priority 4 of
        // 9) and never looked at the lower-priority LICENSE candidate.
        let (manifest, sections) = build_manifest(
            Toolchain::Cpp {
                compiler: semver::Version::new(17, 0, 0),
            },
            &[
                ("CMakeLists.txt", CMAKE_WITH_NO_LICENSE),
                ("LICENSE", MIT_LICENSE_TEXT),
            ],
        );
        let coordinates = cpp_coordinates("github.com/example/demo");
        let facets = extract_facets(&coordinates, &manifest, &sections, &[], None, None)
            .expect("facets")
            .facets;

        assert_eq!(
            facets.description.as_deref(),
            Some("A demo C++ library"),
            "CMakeLists.txt's description must still populate"
        );
        assert_eq!(
            facets.license.as_deref(),
            Some("mit"),
            "the LICENSE candidate must now be reached and merged in"
        );
    }

    #[test]
    fn single_manifest_cpp_package_is_unaffected_by_merge() {
        // A package with exactly one manifest candidate present must behave
        // identically to the old stop-at-first-match code: the merge loop
        // still only ever finds one match.
        let (manifest, sections) = build_manifest(
            Toolchain::Cpp {
                compiler: semver::Version::new(17, 0, 0),
            },
            &[("CMakeLists.txt", CMAKE_WITH_NO_LICENSE)],
        );
        let coordinates = cpp_coordinates("github.com/example/demo");
        let facets = extract_facets(&coordinates, &manifest, &sections, &[], None, None)
            .expect("facets")
            .facets;

        assert_eq!(facets.description.as_deref(), Some("A demo C++ library"));
        assert!(
            facets.license.is_none(),
            "no LICENSE file present: license must stay unset, not fabricated"
        );
    }

    #[test]
    fn higher_priority_manifest_wins_when_both_set_the_same_field() {
        // vcpkg.json (candidate priority 1) and meson.build (candidate
        // priority 5) both carry a `license` expression for this fixture;
        // vcpkg.json's must win per the documented merge policy (first
        // non-None, in priority order) even though both parse successfully.
        let vcpkg_json = br#"{"name":"demo","license":"Apache-2.0"}"#;
        let meson_build = b"project('demo', license: 'MIT')\n";
        let (manifest, sections) = build_manifest(
            Toolchain::Cpp {
                compiler: semver::Version::new(17, 0, 0),
            },
            &[("vcpkg.json", vcpkg_json), ("meson.build", meson_build)],
        );
        let coordinates = cpp_coordinates("github.com/example/demo");
        let facets = extract_facets(&coordinates, &manifest, &sections, &[], None, None)
            .expect("facets")
            .facets;

        assert_eq!(
            facets.license.as_deref(),
            Some("apache-2.0"),
            "vcpkg.json is higher priority than meson.build and must win"
        );
    }

    #[test]
    fn single_manifest_rust_package_unaffected_by_merge() {
        // Sanity check on an ecosystem with exactly one manifest candidate at
        // all (Rust: only Cargo.toml) — never touches the merge path.
        let cargo_toml = br#"
[package]
name = "demo"
version = "1.0.0"
description = "A demo crate"
license = "MIT"
"#;
        let (manifest, sections) = build_manifest(
            Toolchain::Rust {
                compiler: semver::Version::new(1, 85, 0),
                edition: heart::Edition::E2024,
            },
            &[("Cargo.toml", cargo_toml)],
        );
        let coordinates = rust_coordinates("demo");
        let facets = extract_facets(&coordinates, &manifest, &sections, &[], None, None)
            .expect("facets")
            .facets;

        assert_eq!(facets.description.as_deref(), Some("A demo crate"));
        assert_eq!(facets.license.as_deref(), Some("mit"));
    }

    #[test]
    fn no_manifest_at_all_degrades_to_name_only_facets() {
        let (manifest, sections) = build_manifest(
            Toolchain::Cpp {
                compiler: semver::Version::new(17, 0, 0),
            },
            &[("README.md", b"# demo\n")],
        );
        let coordinates = cpp_coordinates("github.com/example/demo");
        let facets = extract_facets(&coordinates, &manifest, &sections, &[], None, None)
            .expect("facets")
            .facets;

        assert!(facets.description.is_none());
        assert!(facets.license.is_none());
    }
}
