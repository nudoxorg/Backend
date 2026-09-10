//! Defines graph behavior for `interface-search`, whose purpose is to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! This module owns the graph invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Relation-graph requests and terminals over the Trustfall lane.

use compiler_ir::{Confidence, LinkKind};
use interface_documents::{Direction, Symbol, Target};
use interface_identity::{Address, ContentKey};

use crate::{Coverage, ResultLimit};

/// Deepest traversal any surface may request.
pub const MAX_GRAPH_DEPTH: u8 = 4;

/// Every link kind in display order.
pub const LINK_KINDS: [LinkKind; 11] = [
    LinkKind::Calls,
    LinkKind::MethodCall,
    LinkKind::TypeReference,
    LinkKind::Reads,
    LinkKind::Writes,
    LinkKind::Imports,
    LinkKind::Implements,
    LinkKind::Overrides,
    LinkKind::Reexports,
    LinkKind::Inherits,
    LinkKind::Documents,
];

/// One readable relation label, shared verbatim by every surface.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RelationLabel(&'static str);

impl RelationLabel {
    /// Returns the exact label text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::fmt::Display for RelationLabel {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.0)
    }
}

/// Readable label for one relation role, shared by every surface.
///
/// Outgoing reads as what the page does (`calls`); incoming reads as what is done to it
/// (`called-by`). The outgoing label is also the accepted spelling on input.
#[must_use]
pub const fn relation_label(kind: LinkKind, direction: Direction) -> RelationLabel {
    RelationLabel(match (kind, direction) {
        (LinkKind::Calls, Direction::Outgoing) => "calls",
        (LinkKind::Calls, Direction::Incoming) => "called-by",
        (LinkKind::MethodCall, Direction::Outgoing) => "invokes",
        (LinkKind::MethodCall, Direction::Incoming) => "invoked-by",
        (LinkKind::TypeReference, Direction::Outgoing) => "references",
        (LinkKind::TypeReference, Direction::Incoming) => "referenced-by",
        (LinkKind::Reads, Direction::Outgoing) => "reads",
        (LinkKind::Reads, Direction::Incoming) => "read-by",
        (LinkKind::Writes, Direction::Outgoing) => "writes",
        (LinkKind::Writes, Direction::Incoming) => "written-by",
        (LinkKind::Imports, Direction::Outgoing) => "imports",
        (LinkKind::Imports, Direction::Incoming) => "imported-by",
        (LinkKind::Implements, Direction::Outgoing) => "implements",
        (LinkKind::Implements, Direction::Incoming) => "implemented-by",
        (LinkKind::Overrides, Direction::Outgoing) => "overrides",
        (LinkKind::Overrides, Direction::Incoming) => "overridden-by",
        (LinkKind::Reexports, Direction::Outgoing) => "reexports",
        (LinkKind::Reexports, Direction::Incoming) => "reexported-by",
        (LinkKind::Inherits, Direction::Outgoing) => "inherits",
        (LinkKind::Inherits, Direction::Incoming) => "inherited-by",
        (LinkKind::Documents, Direction::Outgoing) => "documents",
        (LinkKind::Documents, Direction::Incoming) => "documented-by",
    })
}

/// Parses a relation spelling in either direction.
#[must_use]
pub fn parse_relation(text: &str) -> Option<(LinkKind, Direction)> {
    LINK_KINDS.into_iter().find_map(|kind| {
        [Direction::Outgoing, Direction::Incoming]
            .into_iter()
            .find(|direction| relation_label(kind, *direction).as_str() == text)
            .map(|direction| (kind, direction))
    })
}

/// Bounded traversal depth.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Depth(u8);

impl Depth {
    /// One hop.
    pub const ONE: Self = Self(1);

    /// Clamps into the accepted range; zero becomes one.
    #[must_use]
    pub const fn clamped(requested: u8) -> Self {
        if requested == 0 {
            Self(1)
        } else if requested > MAX_GRAPH_DEPTH {
            Self(MAX_GRAPH_DEPTH)
        } else {
            Self(requested)
        }
    }

    /// Accepted depth.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Where a traversal starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphSource {
    /// A resolvable address.
    Address(Address),
    /// A bare key.
    Key(ContentKey),
}

/// One traversal request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphRequest {
    /// Start node.
    pub source: GraphSource,
    /// Restrict to one link kind; `None` traverses every kind.
    pub kind: Option<LinkKind>,
    /// Traversal direction.
    pub direction: Direction,
    /// Hops.
    pub depth: Depth,
    /// Edge budget.
    pub limit: ResultLimit,
}

/// One traversed edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphEdge {
    /// Edge source.
    pub from: Symbol,
    /// Edge target, local or external.
    pub to: Target,
    /// Canonical link kind.
    pub kind: LinkKind,
    /// Strongest observed confidence.
    pub confidence: Confidence,
    /// Hop count from the request source, starting at one.
    pub hop: Depth,
}

/// One complete traversal answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphTerminal {
    /// Resolved start node.
    pub source: Symbol,
    /// Edges in traversal order.
    pub edges: Box<[GraphEdge]>,
    /// Coverage claim for the graph lane.
    pub coverage: Coverage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_relation_label_round_trips() {
        for kind in LINK_KINDS {
            for direction in [Direction::Outgoing, Direction::Incoming] {
                assert_eq!(
                    parse_relation(relation_label(kind, direction).as_str()),
                    Some((kind, direction))
                );
            }
        }
        assert_eq!(
            parse_relation("calls"),
            Some((LinkKind::Calls, Direction::Outgoing))
        );
    }
}
