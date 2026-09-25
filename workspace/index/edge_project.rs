//! One projection from a manifest [`DepEdge`] onto a catalog [`EdgeWire`].
//!
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

/// Package records for the versions in `ops`.
///
/// A version is named by the [`CatalogOp::UpsertPackage`] in the same batch.
/// A version whose stem is absent from the batch is skipped. Wire kinds fold
/// back onto [`DepClass`]: runtime and recipe stay runtime, build stays build,
/// and every other catalog mechanism is build.
#[must_use]
pub fn records_from_ops(ops: &[crate::protocol::CatalogOp]) -> Vec<crate::record::PackageRecord> {
    use std::collections::HashMap;

    use crate::{protocol::CatalogOp, record::PackageRecord};
    use smol_str::SmolStr;

    let mut stems = HashMap::new();
    for op in ops {
        if let CatalogOp::UpsertPackage { stem, .. } = op {
            stems.insert(stem.stem_id, (stem.ecosystem, stem.name_canonical.clone()));
        }
    }
    let mut records = Vec::new();
    for op in ops {
        let CatalogOp::UpsertVersion {
            coordinates,
            edges,
            license,
            ..
        } = op
        else {
            continue;
        };
        let Some((ecosystem, name)) = stems.get(&coordinates.stem_id) else {
            continue;
        };
        let dep_edges = edges.iter().map(edge_from_wire).collect();
        records.push(PackageRecord::from_parts(
            *ecosystem,
            name.clone(),
            coordinates.version_canonical.clone(),
            None,
            license.as_ref().map(|value| SmolStr::new(value)),
            Vec::new(),
            None,
            None,
            false,
            dep_edges,
        ));
    }
    records
}

fn edge_from_wire(wire: &EdgeWire) -> DepEdge {
    use smol_str::SmolStr;

    let class = match wire.kind {
        EdgeKind::Runtime | EdgeKind::Recipe => DepClass::Runtime,
        EdgeKind::Build
        | EdgeKind::FindPackage
        | EdgeKind::PkgConfig
        | EdgeKind::Submodule
        | EdgeKind::FetchContent
        | EdgeKind::Wrap
        | EdgeKind::BazelDep
        | EdgeKind::Vendored => DepClass::Build,
    };
    DepEdge {
        name: SmolStr::new(&wire.dep_name_canonical),
        requirement: if wire.requirement.is_empty() {
            None
        } else {
            Some(SmolStr::new(&wire.requirement))
        },
        class,
        optional: false,
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
            optional: false,
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
                edges: project_edges(Language::Rust, &edges_for_batch(), EdgeSource::Manifest),
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

    fn edges_for_batch() -> [DepEdge; 3] {
        [
            edge("libc", DepClass::Runtime, Some("^1")),
            edge("cc", DepClass::Build, None),
            edge("serde", DepClass::Peer, Some("1")),
        ]
    }
}
