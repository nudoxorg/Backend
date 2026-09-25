//! One projection from a manifest [`DepEdge`] onto a catalog [`EdgeWire`].
//! Feed publishes that only know names go through the same function after
//! [`runtime_edges_from_names`](crate::record::runtime_edges_from_names). A
//! peer edge is not an installed dependency, so it is absent here. Runtime and
//! optional classes share [`EdgeKind::Runtime`] because the catalog kind has
//! no optional bit; those two classes stay distinct versioned rows.

use std::collections::BTreeSet;

use heart::Language;

use crate::{
    enums::{EdgeKind, EdgeSource, TextEnum},
    protocol::EdgeWire,
    record::{DepClass, DepEdge},
};

/// One feed observation: the runtime names a compile or publish learned.
///
/// Built once and applied to SQL and the versioned ledger, so the two stores
/// cannot invent different wires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedObservation {
    /// Identity used when the ledger has no body for this version yet.
    pub record: crate::record::PackageRecord,
    /// Runtime replace. Other kinds stay.
    pub snapshot: crate::protocol::EdgeSnapshot,
}

/// A non-empty dependency list, as one feed snapshot.
///
/// An empty list is not an observation: callers leave stored edges alone.
#[must_use]
pub fn feed_observation(
    ecosystem: Language,
    name: &str,
    version: &str,
    names: &[impl AsRef<str>],
) -> Option<FeedObservation> {
    if names.is_empty() {
        return None;
    }
    let record = crate::record::PackageRecord::published(ecosystem, name, version, names);
    let snapshot = crate::protocol::EdgeSnapshot::feed(project_edges(
        ecosystem,
        &record.edges,
        EdgeSource::Feed,
    ));
    Some(FeedObservation { record, snapshot })
}

/// Catalog wires for `edges`, in first-seen order.
///
/// A later edge with the same name and the same [`EdgeKind`] is dropped. The
/// requirement is the manifest expression, or empty when the manifest named
/// none.
#[must_use]
pub fn project_edges(ecosystem: Language, edges: &[DepEdge], source: EdgeSource) -> Vec<EdgeWire> {
    let mut seen = BTreeSet::new();
    let mut wires = Vec::new();
    for edge in edges {
        let Some(kind) = kind_of(edge.class) else {
            continue;
        };
        if !seen.insert((edge.name.clone(), kind.as_token())) {
            continue;
        }
        wires.push(EdgeWire {
            dep_ecosystem: ecosystem,
            dep_name_canonical: edge.name.to_string(),
            requirement: edge.requirement.as_deref().unwrap_or("").to_owned(),
            kind,
            source,
            resolved_stem: None,
        });
    }
    wires
}

/// One effect a catalog batch has on the versioned package ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEffect {
    /// Write or revise this package. `edges` says which kinds move.
    Upsert {
        /// Package body. Its edges are the replacement list.
        record: crate::record::PackageRecord,
        /// Which stored edges this write may replace.
        edges: crate::protocol::EdgeSnapshot,
    },
    /// Tombstone this package version and every edge it owns.
    Remove {
        /// Ecosystem of the removed package.
        ecosystem: Language,
        /// Canonical package name.
        name: String,
        /// Published version string.
        version: String,
    },
}

/// Package records for the versions in `ops`. Removals are omitted; use
/// [`effects_from_ops`] when the batch can delete a version.
#[must_use]
pub fn records_from_ops(ops: &[crate::protocol::CatalogOp]) -> Vec<crate::record::PackageRecord> {
    records_from_ops_named(ops, None)
}

/// Like [`records_from_ops`], and names a version whose stem is not in the
/// batch.
///
/// Git polls emit [`VersionDelta`](crate::protocol::VersionDelta) after the
/// package was registered on an earlier commit. `fallback` is that package.
#[must_use]
pub fn records_from_ops_named(
    ops: &[crate::protocol::CatalogOp],
    fallback: Option<(Language, &str)>,
) -> Vec<crate::record::PackageRecord> {
    effects_from_ops(ops, fallback)
        .into_iter()
        .filter_map(|effect| match effect {
            LedgerEffect::Upsert { record, .. } => Some(record),
            LedgerEffect::Remove { .. } => None,
        })
        .collect()
}

/// Versioned-ledger effects for `ops`, in batch order.
///
/// Upserts and removals share one naming rule: the [`CatalogOp::UpsertPackage`]
/// in the batch, or `fallback` when the package was registered earlier.
#[must_use]
pub fn effects_from_ops(
    ops: &[crate::protocol::CatalogOp],
    fallback: Option<(Language, &str)>,
) -> Vec<LedgerEffect> {
    use std::collections::HashMap;

    use crate::{
        protocol::{CatalogOp, VersionDelta},
        record::PackageRecord,
    };
    use smol_str::SmolStr;

    let mut stems = HashMap::new();
    for op in ops {
        if let CatalogOp::UpsertPackage { stem, .. } = op {
            stems.insert(stem.stem_id, (stem.ecosystem, stem.name_canonical.clone()));
        }
    }
    let name_of = |stem_id| match stems.get(&stem_id) {
        Some((ecosystem, name)) => Some((*ecosystem, name.clone())),
        None => fallback.map(|(ecosystem, name)| (ecosystem, name.to_owned())),
    };
    let mut effects = Vec::new();
    for op in ops {
        match op {
            CatalogOp::VersionDelta {
                delta:
                    VersionDelta::Removed {
                        stem_id,
                        version_canonical,
                        ..
                    },
            } => {
                let Some((ecosystem, name)) = name_of(*stem_id) else {
                    continue;
                };
                effects.push(LedgerEffect::Remove {
                    ecosystem,
                    name,
                    version: version_canonical.clone(),
                });
            }
            _ => {
                let Some((stem_id, version, edges, license, source)) = version_view(op) else {
                    continue;
                };
                let Some((ecosystem, name)) = name_of(stem_id) else {
                    continue;
                };
                let mut record = PackageRecord::from_parts(
                    ecosystem,
                    name,
                    version,
                    None,
                    license.map(SmolStr::new),
                    Vec::new(),
                    None,
                    None,
                    false,
                    edges.wires().iter().map(edge_from_wire).collect(),
                );
                if let Some(digest) = crate::pid::observed_content(
                    source.and_then(|source| source.registry_checksum.as_deref()),
                    source.and_then(|source| source.source_rev.as_deref()),
                ) {
                    record = record.with_content(digest);
                }
                effects.push(LedgerEffect::Upsert {
                    record,
                    edges: edges.clone(),
                });
            }
        }
    }
    effects
}

fn version_view(
    op: &crate::protocol::CatalogOp,
) -> Option<(
    crate::ids::PackageStemId,
    &str,
    &crate::protocol::EdgeSnapshot,
    Option<&str>,
    Option<&crate::protocol::SourceAcquisitionWire>,
)> {
    use crate::protocol::{CatalogOp, VersionDelta};

    match op {
        CatalogOp::UpsertVersion {
            coordinates,
            edges,
            license,
            source,
            ..
        } => Some((
            coordinates.stem_id,
            coordinates.version_canonical.as_str(),
            edges,
            license.as_deref(),
            source.as_ref(),
        )),
        CatalogOp::VersionDelta {
            delta: VersionDelta::Added { version } | VersionDelta::Changed { version },
        } => Some((
            version.coordinates.stem_id,
            version.coordinates.version_canonical.as_str(),
            &version.edges,
            version.license.as_deref(),
            version.source.as_ref(),
        )),
        _ => None,
    }
}

pub(crate) fn edges_from_wires(wires: &[EdgeWire]) -> Vec<DepEdge> {
    wires.iter().map(edge_from_wire).collect()
}

fn edge_from_wire(wire: &EdgeWire) -> DepEdge {
    use smol_str::SmolStr;

    let class = crate::engine::class_of_kind(wire.kind);
    DepEdge {
        name: SmolStr::new(&wire.dep_name_canonical),
        requirement: if wire.requirement.is_empty() {
            None
        } else {
            Some(SmolStr::new(&wire.requirement))
        },
        class,
        kind: wire.kind,
        optional: false,
        dep_ecosystem: Some(wire.dep_ecosystem),
    }
}

fn kind_of(class: DepClass) -> Option<EdgeKind> {
    match class {
        DepClass::Runtime | DepClass::Optional => Some(EdgeKind::Runtime),
        DepClass::Dev | DepClass::Build => Some(EdgeKind::Build),
        DepClass::Peer => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smol_str::SmolStr;

    fn edge(name: &str, class: DepClass, requirement: Option<&str>) -> DepEdge {
        DepEdge {
            name: SmolStr::new(name),
            requirement: requirement.map(SmolStr::new),
            class,
            kind: match class {
                DepClass::Runtime | DepClass::Optional => EdgeKind::Runtime,
                DepClass::Dev | DepClass::Build | DepClass::Peer => EdgeKind::Build,
            },
            optional: false,
            dep_ecosystem: None,
        }
    }

    #[test]
    fn classes_share_one_wire_vocabulary() {
        let edges = [
            edge("libc", DepClass::Runtime, Some("^1")),
            edge("libc", DepClass::Optional, Some("^2")),
            edge("libc", DepClass::Dev, Some("^9")),
            edge("serde", DepClass::Peer, Some("1")),
            edge("cc", DepClass::Build, None),
        ];
        let wires = project_edges(Language::Rust, &edges, EdgeSource::Manifest);
        assert_eq!(wires.len(), 3);
        assert_eq!(wires[0].dep_name_canonical, "libc");
        assert_eq!(wires[0].kind, EdgeKind::Runtime);
        assert_eq!(wires[0].requirement, "^1");
        assert_eq!(wires[0].source, EdgeSource::Manifest);
        assert_eq!(wires[1].kind, EdgeKind::Build);
        assert_eq!(wires[1].requirement, "^9");
        assert_eq!(wires[2].dep_name_canonical, "cc");
        assert!(wires[2].requirement.is_empty());
        assert!(wires.iter().all(|wire| wire.resolved_stem.is_none()));
    }

    #[test]
    fn a_committed_batch_revisions_the_versioned_rows() {
        use crate::{
            engine::turso_vc::{FactWrite, VersionedCatalog},
            ids::PackageStemId,
            protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates},
        };
        use heart::PackageId;

        let stem_id = PackageStemId::from_uuid(uuid::Uuid::from_u128(7));
        let version_id = PackageId::from_uuid(uuid::Uuid::from_u128(8));
        let ops = [
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id,
                    ecosystem: Language::Rust,
                    name_struct: "pkg:cargo/memchr".into(),
                    name_canonical: "memchr".into(),
                    name_original: "memchr".into(),
                },
                repo_url: None,
            },
            CatalogOp::UpsertVersion {
                coordinates: VersionCoordinates {
                    version_id,
                    stem_id,
                    version_canonical: "2.8.3".into(),
                    version_original: "2.8.3".into(),
                },
                published_at: None,
                toolchain: None,
                license: Some("MIT".into()),
                edges: crate::protocol::EdgeSnapshot::manifest(project_edges(
                    Language::Rust,
                    &edges_for_batch(),
                    EdgeSource::Manifest,
                )),
                facets: FacetWire::default(),
                source: None,
            },
        ];
        let records = records_from_ops(&ops);
        assert_eq!(records.len(), 1);
        let mut catalog = VersionedCatalog::open().expect("open");
        assert!(matches!(
            catalog.put_record(&records[0]).expect("put"),
            FactWrite::Revised(_)
        ));
        let tip = catalog
            .materialize("rust", "memchr", "2.8.3")
            .expect("join")
            .expect("row");
        assert_eq!(tip.license.as_deref(), Some("MIT"));
        assert_eq!(tip.edges.len(), 2);
        assert_eq!(tip.edges[0].name.as_str(), "libc");
        assert_eq!(tip.edges[0].requirement.as_deref(), Some("^1"));
        assert_eq!(tip.edges[1].class, DepClass::Build);
    }

    #[test]
    fn a_removal_names_the_fallback_package_and_tombstones_its_row() {
        use crate::{
            engine::turso_vc::{FactWrite, VersionedCatalog},
            ids::PackageStemId,
            protocol::{CatalogOp, VersionDelta},
        };
        use heart::PackageId;

        let stem_id = PackageStemId::from_uuid(uuid::Uuid::from_u128(7));
        let version_id = PackageId::from_uuid(uuid::Uuid::from_u128(8));
        let record = crate::record::PackageRecord::from_parts(
            Language::Cpp,
            "example.test/repo",
            "v1.0.1",
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            vec![edge("openssl", DepClass::Runtime, Some("3"))],
        );
        let mut catalog = VersionedCatalog::open().expect("open");
        assert!(matches!(
            catalog.put_record(&record).expect("put"),
            FactWrite::Revised(_)
        ));
        let ops = [CatalogOp::VersionDelta {
            delta: VersionDelta::Removed {
                stem_id,
                version_id,
                version_canonical: "v1.0.1".into(),
            },
        }];
        let effects = effects_from_ops(&ops, Some((Language::Cpp, "example.test/repo")));
        assert_eq!(effects.len(), 1);
        let LedgerEffect::Remove {
            ecosystem,
            name,
            version,
        } = &effects[0]
        else {
            panic!("removal");
        };
        assert!(matches!(
            catalog
                .drop_version(ecosystem.as_token(), name, version)
                .expect("drop"),
            FactWrite::Revised(_)
        ));
        assert!(
            catalog
                .materialize("cpp", "example.test/repo", "v1.0.1")
                .expect("tip")
                .is_none()
        );
        assert!(catalog.scan_edges().is_empty());
        assert!(effects_from_ops(&ops, None).is_empty());
    }

    fn edges_for_batch() -> [DepEdge; 3] {
        [
            edge("libc", DepClass::Runtime, Some("^1")),
            edge("cc", DepClass::Build, None),
            edge("serde", DepClass::Peer, Some("1")),
        ]
    }
}
