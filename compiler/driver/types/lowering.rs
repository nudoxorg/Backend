//! Exact facts rejected while entering the bounded compiler emission lane.
//!
//! The fault and its snapshot are value types on purpose: the shared compile
//! terminal outlives every collector arena, so the rejection crosses the
//! driver boundary by copy (ordinal, name length, and the full typed cause),
//! never by borrow. Name bytes stay inspectable at the collector boundary
//! that produced them.

use compiler_ir::{ProductChildRole, ProductConstructorFault, SemanticTypeFault};

/// Exact cause for rejecting one emitted fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactFault {
    /// The emitted declaration name is empty.
    EmptyName,
    /// The bounded fact lane is full.
    Capacity,
    /// The fact's product child lane is full.
    ChildCapacity,
    /// The constructor payload disagrees with its child count.
    Constructor(ProductConstructorFault),
    /// A child role disagrees with the constructor's closed role lane.
    ChildRole {
        /// Ordered position of the offending child.
        position: usize,
        /// Role demanded by the constructor's closed lane.
        expected: ProductChildRole,
        /// Role the fact actually carried.
        actual: ProductChildRole,
    },
    /// A child targets a fact outside the already-pushed set.
    ChildTarget {
        /// Ordered position of the offending child.
        position: usize,
        /// Target ordinal the fact named.
        target: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The declared type record violates the closed lattice.
    TypeRecord(SemanticTypeFault),
    /// One type-record child violates its tag's closed child law.
    TypeChild {
        /// Ordered position of the offending child.
        position: usize,
        /// Exact closed-lattice fault.
        fault: SemanticTypeFault,
    },
    /// A type-record child targets a fact outside the already-pushed set.
    TypeChildTarget {
        /// Ordered position of the offending child.
        position: usize,
        /// Target ordinal the child named.
        target: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The type-record child lane is full.
    TypeChildCapacity,
    /// The anonymous type-row pool is full.
    TypeRowCapacity,
    /// The computed type-row lane is full.
    ComputedRowCapacity,
    /// An occurrence names an owner outside the pushed prefix.
    OccurrenceOwner {
        /// Owner ordinal the occurrence named.
        owner: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The occurrence lane is full.
    OccurrenceCapacity,
    /// A doc fragment names an owner outside the pushed prefix.
    DocOwner {
        /// Owner ordinal the fragment named.
        owner: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The documentation lane is full.
    DocCapacity,
    /// The extension-atom lane is full.
    ExtensionAtomCapacity,
    /// The type-parameter lane is full.
    TypeParameterCapacity,
    /// A pooled reference lane is full.
    RefListCapacity,
    /// A pooled reference list has too many elements.
    RefListElements,
    /// A pooled reference targets a fact outside the pushed prefix.
    RefTarget {
        /// Closed lane name the reference targeted.
        lane: &'static str,
        /// Raw ordinal the reference named.
        raw: u32,
        /// Ordinals admitted in that lane.
        fact_count: usize,
    },
}

/// Exact rejection of one emitted fact, retained by value at the shared
/// compile terminal. The fact ordinal, the rejected name's exact byte
/// length, and the full typed cause survive every boundary; the name bytes
/// themselves stay at the collector that borrowed them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FactRejection {
    /// Zero-based ordinal the fact would have occupied.
    pub fact: usize,
    /// Exact byte length of the rejected declaration name.
    pub name_len: usize,
    /// Full typed rejection cause with every operand.
    pub cause: FactFault,
}
