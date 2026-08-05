//! Occurrence and reference vocabulary for the declaration plane.
//!
//! These types are the nudox-ir home of three concepts shared with the body
//! plane in `workspace/ir/vocab.rs`:
//! [`Confidence`] (the dual-tier fidelity lattice), [`ReferenceKind`] (the
//! category of a reference), and [`RelSpan`] (a span expressed relative to
//! the owning entry's span start). Together they compose [`Occurrence`], a
//! single reference fact.
//!
//! The wire encoding of every enum variant is its explicit `#[repr(u8)]`
//! discriminant and must never be renumbered — the values are frozen as part
//! of the `nudox.ir.v1` format.

use serde::{Deserialize, Serialize};

use crate::change::StableRef;

// ---------------------------------------------------------------------------
// Confidence
// ---------------------------------------------------------------------------

/// The fidelity with which a reference fact was resolved.
///
/// The lattice is totally ordered from the honest dumb tier
/// ([`Confidence::Syntactic`]) up to a fully typed oracle resolution
/// ([`Confidence::Oracle`]). The ordering is load-bearing: on overlapping
/// facts the higher confidence wins the target (§5.1 merge rule 3), and
/// downstream projections gate graph edges on `>= Confidence::Index` (the
/// reference resolved to at least a concrete same-package symbol).
///
/// Frozen discriminants (mirrors the `workspace/ir` body-plane lattice so
/// meaning is preserved across the plane migration). Never renumber.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
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

/// The category of a reference-bearing fact.
///
/// Frozen discriminants; never renumber (part of the nudox-ir wire format).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
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
    /// An import brought into scope.
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

/// A half-open byte range `[start, end)` **relative to the owning entry's
/// span start** (§5.1 / U-3).
///
/// Storing relative offsets keeps a body edit O(delta): moving an entry down
/// the file leaves every interior span byte-identical. Both bounds are UTF-8
/// byte offsets measured from the entry declaration's `span.start`, never
/// absolute file offsets.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
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
    /// zero-width span overlaps a non-empty one when it lies strictly inside
    /// it.
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

// ---------------------------------------------------------------------------
// Occurrence
// ---------------------------------------------------------------------------

/// A single reference fact: the owning entry (implied by the containing map)
/// references `target` via `kind` at `span`, resolved at `confidence`.
///
/// The owner is NOT a field; it is implied by whichever map or list holds this
/// value, keeping the representation compact.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Occurrence {
    /// The referenced symbol.
    pub target: StableRef,
    /// The category of this reference.
    pub kind: ReferenceKind,
    /// The fidelity of the resolution.
    pub confidence: Confidence,
    /// The byte range of the reference site, relative to the owning entry's
    /// span start.
    pub span: RelSpan,
    // FIXME: workspace/ir Occurrence carries a `body_hash: ContentBlake3` field
    // linking back to the companion `.nb` body file. Omitted here because the
    // body-hash infrastructure (CompanionBody / `.nb` encoding) is not yet
    // present in nudox-ir; add when the implementation plane lands.
}

impl Occurrence {
    /// Construct an occurrence.
    #[inline]
    pub fn new(
        target: StableRef,
        kind: ReferenceKind,
        confidence: Confidence,
        span: RelSpan,
    ) -> Self {
        Self {
            target,
            kind,
            confidence,
            span,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

    fn make_stable_ref() -> StableRef {
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo"));
        let intro = IntroId::from_raw([0xabu8; 32]);
        StableRef::new(lineage, intro)
    }

    #[test]
    fn vocab_occurrence_roundtrip_and_lattice() {
        // Lattice ordering is load-bearing.
        assert!(Confidence::Oracle > Confidence::Syntactic);
        assert!(Confidence::Index > Confidence::Suffix);
        assert!(!Confidence::Syntactic.is_graph_worthy());
        assert!(Confidence::Oracle.is_graph_worthy());

        // RelSpan overlap semantics.
        assert!(RelSpan { start: 2, end: 5 }.overlaps(RelSpan { start: 4, end: 9 }));
        assert!(!RelSpan { start: 2, end: 5 }.overlaps(RelSpan { start: 5, end: 9 })); // touching, not overlapping

        // Occurrence serde round-trip.
        let occ = Occurrence::new(
            make_stable_ref(),
            ReferenceKind::FunctionCall,
            Confidence::Oracle,
            RelSpan::new(2, 5),
        );
        let json = serde_json::to_string(&occ).expect("serialize Occurrence");
        let back: Occurrence = serde_json::from_str(&json).expect("deserialize Occurrence");
        assert_eq!(occ, back, "Occurrence serde round-trip must be identity");
    }
}
