//! Direct-git enumeration (REGISTRYLESS-PLAN §7.4 step 2): turn a cpp stem's
//! upstream repository into catalog ops — one `UpsertVersion` per tag, or a
//! single pseudo-version from `HEAD` when the repo is untagged (§3.3).
//!
//! # Flow
//!
//! 1. The caller (route handler / git monitor) has a normalized repo URL for a
//!    cpp stem.
//! 2. [`enumerate_git_versions`] pulls `ls-remote` bytes via the
//!    [`GitRepository`] adapter and hands them to the ecosystem's **pure** cpp
//!    listing parser (`crate::ecosystem::cpp::listing::parse_ls_remote`), which
//!    does all tag/peel logic and pins the peeled commit oid in the `raw` slot
//!    as `"<tag>@<oid>"` (RL-14).
//! 3. Each listed version becomes an [`UpsertVersion`] op with `source: Git {
//!    url, rev: <peeled oid>, registry_checksum: None }`.
//! 4. Zero tags ⇒ one pseudo-version synthesized from `HEAD` via
//!    `crate::ecosystem::cpp::synthesize_pseudo_version` (§3.3).
//!
//! Sealing the ObjectPack and queueing IR are **other planes** (RL-15); this
//! module stops at catalog ops.

use crate::{
    ecosystem::{Language, cpp::listing::parse_ls_remote},
    enums::SourceKind,
    ids::{PackageId, PackageStemId},
    protocol::{CatalogOp, FacetWire, PackageStemWire, SourceAcquisitionWire, VersionCoordinates},
};
use heart::identity::derive;

use crate::ingest::git::{GitRepository, GitRepositoryError};

/// Why enumeration failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The git adapter failed to list refs or read HEAD.
    #[error(transparent)]
    Git(#[from] GitRepositoryError),
    /// The repository had no tags **and** no readable `HEAD`, so neither a tag
    /// version nor a pseudo-version could be produced.
    #[error("repository {url} has neither tags nor a readable HEAD")]
    NoVersions {
        /// The repository URL.
        url: String,
    },
}

/// Backwards-compatible alias: the enumeration error (now [`Error`]).
pub use self::Error as EnumerateError;

/// Derive the deterministic stem id for a cpp direct-git package. The stem
/// identity **is** its normalized repo slug (REGISTRYLESS-PLAN §15), so the id
/// is a v5 hash of `(ecosystem, slug)` — stable across re-enumeration and
/// across the machines that host it.
pub fn cpp_stem_id(repo_slug: &str) -> PackageStemId {
    // Canonical stem framing: the `(ecosystem_token, slug)` parts under heart's
    // frozen injective law ([`heart::identity::derive`]). Every producer that
    // mints an id for the same slug MUST route here or its rows orphan.
    let id =
        derive::package_id_from_parts([Language::Cpp.as_token().as_bytes(), repo_slug.as_bytes()]);
    PackageStemId::from_uuid(*id.as_uuid())
}

/// Derive the deterministic version id from its stem and canonical string
/// (matches the catalog's `UNIQUE(stem_id, version_canonical)` identity).
pub fn cpp_version_id(stem_id: PackageStemId, version_canonical: &str) -> PackageId {
    // Version framing: the `(stem_blob, version_canonical)` parts under the same
    // frozen injective law, so a version id is stable and never collides across
    // a stem boundary.
    derive::package_id_from_parts([stem_id.to_blob().as_slice(), version_canonical.as_bytes()])
}

/// The `UpsertPackage` op that registers a cpp direct-git stem before its
/// versions (REGISTRYLESS-PLAN §7.4 step 1).
pub fn upsert_cpp_package(repo_slug: &str, repo_url: &str) -> CatalogOp {
    let stem_id = cpp_stem_id(repo_slug);
    CatalogOp::UpsertPackage {
        stem: PackageStemWire {
            stem_id,
            ecosystem: Language::Cpp,
            // The slug is the canonical identity; there is no registry name.
            name_struct: format!("pkg:generic/{repo_slug}"),
            name_canonical: repo_slug.to_owned(),
            name_original: repo_slug.to_owned(),
        },
        repo_url: Some(repo_url.to_owned()),
    }
}

/// Split the listing parser's `"<tag>@<oid>"` raw slot into the published tag
/// string and the peeled object id it pins (RL-14). A raw slot without an `@`
/// (defensive) yields the whole string as the tag and no oid.
fn split_tag_and_oid(raw: &str) -> (&str, Option<&str>) {
    match raw.rsplit_once('@') {
        Some((tag, oid)) if !oid.is_empty() => (tag, Some(oid)),
        _ => (raw, None),
    }
}

/// Enumerate a cpp stem's versions from its git remote (REGISTRYLESS-PLAN §7.4
/// step 2). Returns `UpsertVersion` ops (the `UpsertPackage` is the caller's
/// responsibility — usually the git monitor, which already knows the stem).
///
/// * Tagged repo → one op per tag, `source_rev = <peeled oid>`.
/// * Untagged repo → one pseudo-version op from `HEAD`.
pub fn enumerate_git_versions<Repository: GitRepository>(
    git: &Repository,
    repo_slug: &str,
    repo_url: &str,
    commit_timestamp: u64,
) -> Result<Vec<CatalogOp>, Error> {
    let stem_id = cpp_stem_id(repo_slug);
    let bytes = git.ls_remote_bytes(repo_url)?;
    let listed = parse_ls_remote(&bytes);

    if listed.is_empty() {
        return enumerate_pseudo_version(git, stem_id, repo_url, commit_timestamp)
            .map(|op| op.into_iter().collect());
    }

    let mut ops = Vec::with_capacity(listed.len());
    for version in listed {
        let (tag, oid) = split_tag_and_oid(&version.raw);
        let version_canonical = tag.to_owned();
        let version_id = cpp_version_id(stem_id, &version_canonical);
        ops.push(CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id,
                stem_id,
                version_canonical,
                version_original: tag.to_owned(),
            },
            published_at: None,
            toolchain: None,
            license: None,
            edges: Vec::new(),
            facets: FacetWire::default(),
            source: Some(SourceAcquisitionWire {
                source_kind: SourceKind::Git,
                source_pack: None,
                // Pin the peeled commit oid so a force-pushed tag becomes a new
                // version event, never a silent rewrite (REGISTRYLESS-PLAN §15).
                source_rev: oid.map(str::to_owned),
                registry_checksum: None,
                registry_package_uri: None,
            }),
        });
    }
    Ok(ops)
}

/// Keep `SourceMoved` and only the versions whose peeled rev differs from
/// `prior`. A prior canonical absent from this listing becomes
/// [`VersionDelta::Removed`]. An empty `prior` keeps every version (first
/// poll).
pub fn retain_changed_versions(
    ops: Vec<CatalogOp>,
    prior: &std::collections::BTreeMap<String, Option<String>>,
    stem_id: PackageStemId,
) -> Vec<CatalogOp> {
    use std::collections::BTreeSet;

    let mut observed = BTreeSet::new();
    let mut kept = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            CatalogOp::UpsertVersion {
                coordinates,
                published_at,
                toolchain,
                license,
                edges,
                facets,
                source,
            } if coordinates.stem_id == stem_id => {
                let rev = source.as_ref().and_then(|source| source.source_rev.clone());
                observed.insert(coordinates.version_canonical.clone());
                if prior.get(&coordinates.version_canonical) == Some(&rev) {
                    continue;
                }
                kept.push(CatalogOp::UpsertVersion {
                    coordinates,
                    published_at,
                    toolchain,
                    license,
                    edges,
                    facets,
                    source,
                });
            }
            other => kept.push(other),
        }
    }
    for (canonical, _) in prior {
        if observed.contains(canonical) {
            continue;
        }
        kept.push(CatalogOp::VersionDelta {
            delta: crate::protocol::VersionDelta::Removed {
                stem_id,
                version_id: cpp_version_id(stem_id, canonical),
            },
        });
    }
    kept
}

/// Synthesize the single pseudo-version op for an untagged repo from `HEAD`
/// (REGISTRYLESS-PLAN §7.4 step 2 fallback, §3.3). Returns `None` only via the
/// [`Error::NoVersions`] error when `HEAD` is unreadable.
fn enumerate_pseudo_version<Repository: GitRepository>(
    git: &Repository,
    stem_id: PackageStemId,
    repo_url: &str,
    commit_timestamp: u64,
) -> Result<Option<CatalogOp>, Error> {
    let Some(head_oid) = git.head_object_id(repo_url)? else {
        return Err(Error::NoVersions {
            url: repo_url.to_owned(),
        });
    };
    // The Go pseudo-version grammar takes a 12-hex commit prefix; a short remote
    // HEAD (defensive) is padded/truncated to 12 by the synthesis helper's own
    // validation, so guard here and fall back to Raw when it cannot form one.
    let hash12: String = head_oid.chars().take(12).collect();
    let synthesized =
        crate::ecosystem::cpp::synthesize_pseudo_version(None, commit_timestamp, &hash12);

    // If the timestamp/hash could not form a valid Go pseudo-version, fall back
    // to a raw HEAD-pinned version so the stem still gets exactly one version.
    let version_canonical = synthesized.unwrap_or_else(|| format!("0.0.0-head-{hash12}"));

    let version_id = cpp_version_id(stem_id, &version_canonical);
    Ok(Some(CatalogOp::UpsertVersion {
        coordinates: VersionCoordinates {
            version_id,
            stem_id,
            version_canonical: version_canonical.clone(),
            version_original: version_canonical,
        },
        published_at: None,
        toolchain: None,
        license: None,
        edges: Vec::new(),
        facets: FacetWire::default(),
        source: Some(SourceAcquisitionWire {
            source_kind: SourceKind::Git,
            source_pack: None,
            source_rev: Some(head_oid),
            registry_checksum: None,
            registry_package_uri: None,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-crate consistency: the ingestor's `cpp_stem_id` MUST agree with
    /// the shared heart framing law for the same slug. If a second producer
    /// (the add-by-URL route, P6 sealing) derives the id straight from
    /// heart, it has to land on the exact same stem — otherwise catalog
    /// rows orphan.
    #[test]
    fn cpp_stem_id_agrees_with_shared_heart_law() {
        for slug in ["github.com/madler/zlib", "system/pthread"] {
            let via_ingestor = cpp_stem_id(slug);
            let via_heart = {
                let id = derive::package_id_from_parts([
                    Language::Cpp.as_token().as_bytes(),
                    slug.as_bytes(),
                ]);
                PackageStemId::from_uuid(*id.as_uuid())
            };
            assert_eq!(
                via_ingestor, via_heart,
                "stem framing must match for {slug:?}"
            );
        }
    }

    /// A version id derives from `(stem_blob, version_canonical)` under the
    /// same law, and is stable across calls.
    #[test]
    fn retain_changed_versions_drops_stable_revs_and_emits_removals() {
        use std::collections::BTreeMap;
        let stem = cpp_stem_id("example.test/repo");
        let gone_id = cpp_version_id(stem, "v0");
        let ops = vec![CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id: cpp_version_id(stem, "v1"),
                stem_id: stem,
                version_canonical: "v1".into(),
                version_original: "v1".into(),
            },
            published_at: None,
            toolchain: None,
            license: None,
            edges: Vec::new(),
            facets: FacetWire::default(),
            source: Some(SourceAcquisitionWire {
                source_kind: SourceKind::Git,
                source_pack: None,
                source_rev: Some("aaa".into()),
                registry_checksum: None,
                registry_package_uri: None,
            }),
        }];
        let mut prior = BTreeMap::new();
        prior.insert("v1".into(), Some("aaa".into()));
        prior.insert("v0".into(), Some("old".into()));
        let kept = retain_changed_versions(ops, &prior, stem);
        assert!(
            kept.iter()
                .all(|op| !matches!(op, CatalogOp::UpsertVersion { .. })),
            "unchanged rev is omitted"
        );
        assert!(kept.iter().any(|op| matches!(
            op,
            CatalogOp::VersionDelta { delta: crate::protocol::VersionDelta::Removed { version_id, .. } }
                if *version_id == gone_id
        )));
    }

    #[test]
    fn cpp_version_id_is_stable_and_law_framed() {
        let stem = cpp_stem_id("github.com/madler/zlib");
        let a = cpp_version_id(stem, "v1.3.1");
        let b = cpp_version_id(stem, "v1.3.1");
        assert_eq!(a, b);

        let via_heart =
            derive::package_id_from_parts([stem.to_blob().as_slice(), b"v1.3.1".as_slice()]);
        assert_eq!(a, via_heart, "version framing must match the shared law");
    }
}
