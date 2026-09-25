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
}
