//! Defines error behavior for compiler-ir canonical data, whose purpose is to retain exact typed canonicalization failures.
//! This module owns the typed admission, validation, and lookup failures.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Typed admission, validation, and lookup failures.

use core::num::TryFromIntError;

use compiler_ir_vocabulary::{
    AtomId, ListSpan, ProductChildRole, ProductChildren, ProductConstructorFault, ProductId,
    ProductListId,
};
use thiserror::Error;

use super::types::DataResource;

pub(super) fn count(lane: DataCountLane, actual: usize) -> Result<u32, CanonicalDataError> {
    u32::try_from(actual).map_err(|source| CanonicalDataError::Count {
        lane,
        actual,
        source,
    })
}

pub(super) fn require_scratch(
    lane: DataScratchLane,
    required: usize,
    actual: usize,
) -> Result<(), CanonicalDataError> {
    if actual < required {
        return Err(CanonicalDataError::Scratch {
            lane,
            required,
            actual,
        });
    }
    Ok(())
}

pub(super) fn require_output(
    lane: DataOutputLane,
    required: u32,
    actual: usize,
) -> Result<(), CanonicalDataError> {
    let required = usize::try_from(required).map_err(|source| CanonicalDataError::NativeCount {
        lane: match lane {
            DataOutputLane::Atoms => DataCountLane::Atoms,
            DataOutputLane::Products => DataCountLane::Products,
            DataOutputLane::Constructors => DataCountLane::Constructors,
            DataOutputLane::Lists => DataCountLane::Lists,
            DataOutputLane::Children => DataCountLane::Children,
        },
        actual: required,
        source,
    })?;
    if actual < required {
        return Err(CanonicalDataError::OutputTooSmall {
            lane,
            required,
            available: actual,
        });
    }
    Ok(())
}

/// Scratch capacity or input/output geometry lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataScratchLane {
    /// Atom sort order.
    AtomOrder,
    /// Atom map.
    AtomMap,
    /// Product sort order.
    ProductOrder,
    /// Product map.
    ProductMap,
    /// Current colors.
    Colors,
    /// Next colors.
    NextColors,
    /// Current hashes.
    Hashes,
    /// Next hashes.
    NextHashes,
    /// Product representatives.
    Representatives,
    /// Hash-cons table.
    InternSlots,
}

/// Output lane with insufficient caller capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataOutputLane {
    /// Canonical atoms.
    Atoms,
    /// Canonical products.
    Products,
    /// Canonical product constructors.
    Constructors,
    /// Canonical list spans.
    Lists,
    /// Canonical child pool.
    Children,
}

/// Input count lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataCountLane {
    /// Atom lane.
    Atoms,
    /// Product lane.
    Products,
    /// Product-aligned constructor lane.
    Constructors,
    /// List lane.
    Lists,
    /// Child lane.
    Children,
}

/// Exact typed failures from validation, caller-scratch admission, or output
/// preparation. Every coordinate-bearing variant retains the rejected value.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CanonicalDataError {
    /// An input count cannot be represented by a dense coordinate.
    #[error("{lane:?} count {actual} exceeds the dense coordinate width")]
    Count {
        /// Rejected lane.
        lane: DataCountLane,
        /// Rejected native count.
        actual: usize,
        /// Original width conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A `u32` coordinate could not be represented by this target platform.
    #[error("{lane:?} coordinate {actual} does not fit the native address width")]
    NativeCount {
        /// Affected lane.
        lane: DataCountLane,
        /// Rejected wire count.
        actual: u32,
        /// Original width conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A measured work count could not be represented.
    #[error("measured work count {actual} does not fit the native width")]
    NativeWork {
        /// Rejected native count.
        actual: usize,
        /// Original width conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A deduplicated lane count overflowed its coordinate width.
    #[error("{lane:?} canonical count overflowed its coordinate width")]
    CanonicalCountOverflow {
        /// Affected lane.
        lane: DataCountLane,
    },
    /// A scratch-derived canonical count disagreed with its preflight count.
    #[error(
        "{lane:?} canonical count changed during preparation: expected {expected}, actual {actual}"
    )]
    CanonicalCountMismatch {
        /// Affected lane.
        lane: DataCountLane,
        /// Count computed during preflight.
        expected: u32,
        /// Count observed while writing.
        actual: u32,
    },
    /// A caller scratch lane is too short.
    #[error("{lane:?} scratch needs {required} cells but only {actual} are available")]
    Scratch {
        /// Missing lane.
        lane: DataScratchLane,
        /// Required cell count.
        required: usize,
        /// Supplied cell count.
        actual: usize,
    },
    /// A caller output lane is too short.
    #[error("{lane:?} output needs {required} cells but only {available} are available")]
    OutputTooSmall {
        /// Missing lane.
        lane: DataOutputLane,
        /// Required cell count.
        required: usize,
        /// Supplied cell count.
        available: usize,
    },
    /// A product head points outside the atom lane.
    #[error("product {product:?} head {target:?} is outside atom count {atom_count}")]
    ProductHead {
        /// Owning product.
        product: ProductId,
        /// Rejected atom target.
        target: AtomId,
        /// Valid atom count.
        atom_count: u32,
    },
    /// A product list coordinate points outside the list lane.
    #[error("product {product:?} list {target:?} is outside list count {list_count}")]
    ProductList {
        /// Owning product.
        product: ProductId,
        /// Rejected list coordinate.
        target: ProductListId,
        /// Valid list count.
        list_count: u32,
    },
    /// The product and constructor lanes are not exactly aligned.
    #[error(
        "product constructor count {constructor_count} does not equal product count {product_count}"
    )]
    ConstructorCount {
        /// Product lane extent.
        product_count: u32,
        /// Constructor lane extent.
        constructor_count: u32,
    },
    /// One closed constructor payload is malformed for its product list.
    #[error("product {product:?} constructor is invalid: {fault:?}")]
    ProductConstructor {
        /// Owning product coordinate.
        product: ProductId,
        /// Exact tag/payload/arity rejection.
        fault: ProductConstructorFault,
    },
    /// A child role does not match the enclosing constructor's ordered role lane.
    #[error(
        "product {product:?} list {list:?} child {child_ordinal} role {actual:?} expected {expected:?}"
    )]
    ProductChildRole {
        /// Owning product.
        product: ProductId,
        /// Owning pooled list.
        list: ProductListId,
        /// Position within the ordered list.
        child_ordinal: usize,
        /// Required role at this constructor position.
        expected: ProductChildRole,
        /// Rejected role fact.
        actual: ProductChildRole,
    },
    /// A pooled list extends outside the caller's child pool.
    #[error("list {list:?} span {span:?} is outside child pool length {pool_length}")]
    ListExtent {
        /// Rejected list coordinate.
        list: ProductListId,
        /// Rejected span fact.
        span: ListSpan<ProductChildren>,
        /// Caller pool length.
        pool_length: usize,
    },
    /// A span component did not fit the native address width.
    #[error("list {list:?} span {span:?} does not fit native pool length {pool_length}")]
    NativeExtent {
        /// Rejected list coordinate.
        list: ProductListId,
        /// Rejected span fact.
        span: ListSpan<ProductChildren>,
        /// Caller pool length.
        pool_length: usize,
        /// Original width conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A local child points outside the recursive product lane.
    #[error(
        "product {product:?} list {list:?} child {child_ordinal} targets {target:?} outside product count {product_count}"
    )]
    ProductChild {
        /// Owning product.
        product: ProductId,
        /// Owning list.
        list: ProductListId,
        /// Position within the list.
        child_ordinal: usize,
        /// Rejected local target.
        target: ProductId,
        /// Valid product count.
        product_count: u32,
    },
    /// An output coordinate could not be represented.
    #[error("{lane:?} output length {actual} overflows")]
    OutputLength {
        /// Affected lane.
        lane: DataOutputLane,
        /// Rejected native length.
        actual: usize,
    },
    /// A malformed canonical output span was observed while lending it.
    #[error("canonical list {ordinal:?} has invalid span {span:?}: {fault:?}")]
    CanonicalListExtent {
        /// Rejected canonical list coordinate.
        ordinal: ProductListId,
        /// Rejected span.
        span: ListSpan<ProductChildren>,
        /// Exact pooled-list fault.
        fault: compiler_ir_vocabulary::PooledListError,
    },
    /// A canonical atom lookup received an out-of-range source coordinate.
    #[error("canonical atom {ordinal:?} is outside source count {count}")]
    CanonicalAtom {
        /// Rejected source coordinate.
        ordinal: AtomId,
        /// Source count.
        count: usize,
    },
    /// A canonical product lookup received an out-of-range source coordinate.
    #[error("canonical product {ordinal:?} is outside source count {count}")]
    CanonicalProduct {
        /// Rejected source coordinate.
        ordinal: ProductId,
        /// Source count.
        count: usize,
    },
    /// A canonical list lookup received an out-of-range coordinate.
    #[error("canonical list {ordinal:?} is outside canonical count {count}")]
    CanonicalList {
        /// Rejected list coordinate.
        ordinal: ProductListId,
        /// Canonical list count.
        count: usize,
    },
    /// The recursive fixed point exceeded the input-sized bound.
    #[error(
        "recursive product graph did not stabilize after {rounds} rounds for {product_count} products"
    )]
    RefinementBound {
        /// Rounds observed.
        rounds: u32,
        /// Input product count.
        product_count: u32,
    },
    /// The caller's intern table has no free slot.
    #[error("intern table is full while interning product {product:?}; capacity {capacity}")]
    InternTableFull {
        /// Product being interned.
        product: ProductId,
        /// Supplied table capacity.
        capacity: usize,
    },
    /// A caller scratch intern entry was not a valid product ordinal.
    #[error("intern entry {entry} is invalid for product {product:?}")]
    InternEntry {
        /// Product used to report the failure.
        product: ProductId,
        /// Raw table entry.
        entry: u64,
        /// Original width conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// A measured counter overflowed before output mutation.
    #[error("{resource:?} counter overflowed")]
    ResourceCounterOverflow {
        /// Counter that overflowed.
        resource: DataResource,
    },
    /// The complete conservative resource reservation cannot fit the caller's
    /// pre-run budget. This is reported before any scratch or output cell is
    /// written, so it carries a required amount rather than an observed run
    /// count.
    #[error("{resource:?} admission requires {required}, but limit is {limit}")]
    BudgetAdmission {
        /// Resource class.
        resource: DataResource,
        /// Conservative amount that must be admitted before execution.
        required: u64,
        /// Caller limit.
        limit: u64,
    },
    /// A caller resource budget was exceeded by measured work after execution
    /// began.
    #[error("{resource:?} budget {limit} was exceeded by observed {observed}")]
    BudgetExceeded {
        /// Resource class.
        resource: DataResource,
        /// Measured amount.
        observed: u64,
        /// Caller limit.
        limit: u64,
    },
}
