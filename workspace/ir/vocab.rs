//! Shared reference vocabulary for the implementation (body) plane.
//!
//! These types are the new-plane home of three concepts that the legacy
//! `ir/syntax` tree also modelled but which do not exist in the arena IR core:
//! [`Confidence`] (the dual-tier fidelity lattice), [`ReferenceKind`] (the
//! category of a body reference), and [`RelSpan`] (a span expressed relative to
//! the owning entry's span start).
//!
//! The declaration plane already speaks [`crate::change::StableRef`]; the body
//! plane adds only these small, frozen scalars. The wire encoding of every
//! variant is its explicit `#[repr(u8)]` discriminant and must never be
//! renumbered — the values are part of the `nudox.body.v1` format.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Confidence
// ---------------------------------------------------------------------------

/// The fidelity with which a body fact was resolved.
///
/// The lattice is totally ordered from the honest dumb tier ([`Confidence::Syntactic`])
/// up to a fully typed oracle resolution ([`Confidence::Oracle`]). The ordering
/// is load-bearing: on overlapping facts the higher confidence wins the target
/// (§5.1 merge rule 3), and downstream projections gate graph edges on
/// `>= Confidence::Index` (the reference resolved to at least a concrete
/// same-package symbol).
///
/// Frozen discriminants (mirrors the legacy `ir/syntax` lattice so meaning is
/// preserved across the plane migration). Never renumber.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize,
)]
#[repr(u8)]
pub enum Confidence {
    /// Tree-sitter surface fact only: a name at a span, no resolved target.
    Syntactic = 0,
    /// Resolved by unique suffix match against a candidate set. Weak.
    Suffix = 1,
    /// Exact/alias match against the package index, or a module-scope sibling.
    /// This is the floor for a fact to become a graph edge.
    Index = 2,
    /// Bound at an `Import` (foreign `StableRef` via the moniker reflection).
    Import = 3,
    /// The language oracle resolved the reference to a precise `StableRef`.
    Oracle = 4,
}

impl Confidence {
    /// The lowest confidence at which a reference is considered graph-worthy
    /// (an edge in the reverse-position index / Trustfall adapter).
    pub const GRAPH_FLOOR: Confidence = Confidence::Index;

    /// True if a fact at this confidence should project into the graph.
    #[inline]
    pub fn is_graph_worthy(self) -> bool {
        self >= Self::GRAPH_FLOOR
    }

    /// Recover a confidence from its frozen wire discriminant.
    #[inline]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Syntactic),
            1 => Some(Self::Suffix),
            2 => Some(Self::Index),
            3 => Some(Self::Import),
            4 => Some(Self::Oracle),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ReferenceKind
// ---------------------------------------------------------------------------

/// The category of a reference-bearing fact inside a body.
///
/// Frozen discriminants; never renumber (part of `nudox.body.v1`).
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize,
)]
#[repr(u8)]
pub enum ReferenceKind {
    /// A free-function or associated-function call.
    FunctionCall = 0,
    /// A method call with a receiver.
    MethodCall = 1,
    /// A reference to a type in value position (annotation, cast, generic arg).
    TypeReference = 2,
    /// A read/use of a variable or binding.
    VariableUse = 3,
    /// A macro invocation.
    MacroInvocation = 4,
    /// A field access (`base.field`).
    FieldAccess = 5,
    /// An import brought into the body scope.
    Import = 6,
}

impl ReferenceKind {
    /// Recover a reference kind from its frozen wire discriminant.
    #[inline]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::FunctionCall),
            1 => Some(Self::MethodCall),
            2 => Some(Self::TypeReference),
            3 => Some(Self::VariableUse),
            4 => Some(Self::MacroInvocation),
            5 => Some(Self::FieldAccess),
            6 => Some(Self::Import),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// RelSpan
// ---------------------------------------------------------------------------

/// A half-open byte range `[start, end)` **relative to the owning entry's span
/// start** (§5.1 / U-3).
///
/// Storing relative offsets keeps a body edit O(delta): moving an entry down
/// the file leaves every interior span byte-identical, so the `.nb` companion
/// does not re-diff. Both bounds are UTF-8 byte offsets measured from the entry
/// declaration's `span.start`, never absolute file offsets.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize,
)]
pub struct RelSpan {
    /// Byte offset of the first byte, relative to the entry span start.
    pub start: u32,
    /// Byte offset one past the last byte, relative to the entry span start.
    pub end: u32,
}

impl RelSpan {
    /// Construct a relative span from start/end offsets.
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Length in bytes.
    #[inline]
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// True if this is a zero-width span.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// True if `self` and `other` share at least one byte.
    ///
    /// Two zero-width spans overlap only when they sit at the same offset; a
    /// zero-width span overlaps a non-empty one when it lies strictly inside it.
    #[inline]
    pub const fn overlaps(self, other: RelSpan) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// True if `self` fully contains `other`.
    #[inline]
    pub const fn contains(self, other: RelSpan) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_orders_and_gates() {
        assert!(Confidence::Syntactic < Confidence::Oracle);
        assert!(Confidence::Import < Confidence::Oracle);
        assert!(!Confidence::Syntactic.is_graph_worthy());
        assert!(!Confidence::Suffix.is_graph_worthy());
        assert!(Confidence::Index.is_graph_worthy());
        assert!(Confidence::Oracle.is_graph_worthy());
    }

    #[test]
    fn confidence_discriminants_frozen() {
        assert_eq!(Confidence::Syntactic as u8, 0);
        assert_eq!(Confidence::Oracle as u8, 4);
        assert_eq!(Confidence::from_u8(2), Some(Confidence::Index));
        assert_eq!(Confidence::from_u8(9), None);
    }

    #[test]
    fn reference_kind_discriminants_frozen() {
        assert_eq!(ReferenceKind::FunctionCall as u8, 0);
        assert_eq!(ReferenceKind::Import as u8, 6);
        assert_eq!(ReferenceKind::from_u8(1), Some(ReferenceKind::MethodCall));
        assert_eq!(ReferenceKind::from_u8(7), None);
    }

    #[test]
    fn rel_span_overlap_and_contain() {
        let a = RelSpan::new(10, 20);
        assert!(a.overlaps(RelSpan::new(15, 25)));
        assert!(!a.overlaps(RelSpan::new(20, 30))); // touching, not overlapping
        assert!(a.contains(RelSpan::new(12, 18)));
        assert!(!a.contains(RelSpan::new(12, 22)));
        assert_eq!(a.len(), 10);
    }
}
