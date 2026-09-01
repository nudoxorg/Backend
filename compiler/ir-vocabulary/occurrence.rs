//! Occurrence-plane vocabulary: the reference-fidelity lattice, the closed
//! reference categories, and the owner-relative byte span of one reference
//! fact. Ported from the measured old system (`nudox.ir.v1` froze these
//! discriminants; the 446,947-declaration parity corpus is expressed in
//! them), reshaped into borrowed, allocation-free records.

/// The fidelity with which a reference fact was resolved.
///
/// The lattice is totally ordered from the honest surface tier
/// ([`Confidence::Syntactic`]) up to a fully typed oracle resolution
/// ([`Confidence::Oracle`]). The ordering is load-bearing: on overlapping
/// facts the higher confidence wins the target, and downstream graph
/// projections gate edges on [`Confidence::GRAPH_FLOOR`].
///
/// Frozen discriminants; never renumber.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Confidence {
    /// Surface fact only: a name at a span, no resolved target.
    Syntactic = 0,
    /// Resolved by unique suffix match against a candidate set. Weak.
    Suffix = 1,
    /// Exact/alias match against the package index, or a module-scope
    /// sibling. This is the floor for a fact to become a graph edge.
    Index = 2,
    /// Bound at an import (a foreign key resolved through the import table).
    Import = 3,
    /// The language oracle resolved the reference to a precise target.
    Oracle = 4,
}

impl Confidence {
    /// The lowest confidence at which a reference is considered graph-worthy.
    pub const GRAPH_FLOOR: Confidence = Confidence::Index;

    /// True if a fact at this confidence should project into the graph.
    #[must_use]
    pub const fn is_graph_worthy(self) -> bool {
        matches!(self, Self::Index | Self::Import | Self::Oracle)
    }
}

/// Exact confidence-code rejection retaining the observed byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfidenceCodeError {
    /// Rejected confidence byte.
    pub actual: u8,
}

impl From<Confidence> for u8 {
    /// Encodes the stable wire discriminant.
    fn from(value: Confidence) -> Self {
        match value {
            Confidence::Syntactic => 0,
            Confidence::Suffix => 1,
            Confidence::Index => 2,
            Confidence::Import => 3,
            Confidence::Oracle => 4,
        }
    }
}

impl TryFrom<u8> for Confidence {
    type Error = ConfidenceCodeError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u8) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Syntactic),
            1 => Ok(Self::Suffix),
            2 => Ok(Self::Index),
            3 => Ok(Self::Import),
            4 => Ok(Self::Oracle),
            actual => Err(ConfidenceCodeError { actual }),
        }
    }
}

/// The category of a reference-bearing fact.
///
/// Frozen discriminants; never renumber.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReferenceKind {
    /// A free-function or associated-function call.
    FunctionCall = 0,
    /// A method call with a receiver.
    MethodCall = 1,
    /// A reference to a type in value position (annotation, cast, generic
    /// argument).
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

/// Exact reference-kind-code rejection retaining the observed byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceKindCodeError {
    /// Rejected reference-kind byte.
    pub actual: u8,
}

impl From<ReferenceKind> for u8 {
    /// Encodes the stable wire discriminant.
    fn from(value: ReferenceKind) -> Self {
        match value {
            ReferenceKind::FunctionCall => 0,
            ReferenceKind::MethodCall => 1,
            ReferenceKind::TypeReference => 2,
            ReferenceKind::VariableUse => 3,
            ReferenceKind::MacroInvocation => 4,
            ReferenceKind::FieldAccess => 5,
            ReferenceKind::Import => 6,
        }
    }
}

impl TryFrom<u8> for ReferenceKind {
    type Error = ReferenceKindCodeError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u8) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::FunctionCall),
            1 => Ok(Self::MethodCall),
            2 => Ok(Self::TypeReference),
            3 => Ok(Self::VariableUse),
            4 => Ok(Self::MacroInvocation),
            5 => Ok(Self::FieldAccess),
            6 => Ok(Self::Import),
            actual => Err(ReferenceKindCodeError { actual }),
        }
    }
}

/// A half-open byte range `[start, end)` relative to the owning declaration's
/// span start.
///
/// Storing owner-relative offsets keeps a declaration move O(delta): moving a
/// declaration down the file leaves every interior span byte-identical. Both
/// bounds are UTF-8 byte offsets measured from the owning declaration's span
/// start, never absolute file offsets.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RelSpan {
    /// Byte offset of the first byte, relative to the owner span start.
    pub start: u32,
    /// Byte offset one past the last byte, relative to the owner span start.
    pub end: u32,
}

/// Exact relative-span rejection retaining both observed operands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelSpanFault {
    /// The half-open range is inverted; `start` must not exceed `end`.
    Inverted {
        /// Observed first bound.
        start: u32,
        /// Observed second bound.
        end: u32,
    },
}

impl RelSpan {
    /// Creates a relative span, proving the half-open range is ordered.
    pub const fn new(start: u32, end: u32) -> Result<Self, RelSpanFault> {
        if start > end {
            return Err(RelSpanFault::Inverted { start, end });
        }
        Ok(Self { start, end })
    }

    /// Creates a relative span whose ordering the caller has already proven.
    ///
    /// Prefer [`RelSpan::new`] at admission boundaries; this constructor is
    /// for trusted projection from an already-validated wire section.
    #[must_use]
    pub const fn new_trusted(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Length in bytes.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// True if this is a zero-width span.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// True if `self` and `other` share at least one byte.
    ///
    /// Strict half-open semantics: touching spans do not overlap, a
    /// zero-width span overlaps a non-empty span exactly when it lies
    /// strictly inside it, and two zero-width spans never overlap — not
    /// even identical spans at the same offset. (The old system's doc
    /// claimed same-offset zero-width spans overlap while its own strict
    /// formula said otherwise; the formula is what its merge rules
    /// executed, so it is what this port freezes.)
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// True if `self` fully contains `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}
