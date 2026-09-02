//! The `compiler-ir` crate exists to encode, validate, map, and borrow canonical compiler IR fragments.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "mmap")]
extern crate std;

#[cfg(target_pointer_width = "16")]
compile_error!("compiler-ir requires at least a 32-bit address space");

mod canonical_data;
#[cfg(feature = "mmap")]
mod mapping;
mod model;
mod prepared;
mod range;
mod semantic_facts;
mod type_facts;
mod view;
mod wire;

pub use canonical_data::{
    CanonicalDataError, CanonicalDataGraph, DataCanonicalization, DataCountLane, DataFacts,
    DataOutput, DataOutputLane, DataResource, DataResourceBudget, DataScratch, DataScratchLane,
    canonicalize_data_with_budget,
};
#[cfg(feature = "mmap")]
pub use mapping::{
    MappedFragment, MappedFragmentError, MappedFragmentIoPhase, MappedFragmentView,
    open_fragment_mmap,
};
pub use model::{
    AtomFault, AtomInput, EntityFault, EntityKind, EntityKindCodeError, EntityNameFault,
    EntityRecord, EntityRecordFault, EntityType, PrimitiveType, RecipeFact, RecipeFactFault,
    SourceIdentity, SourceIdentityFault, TypeNode, TypeNodeFault,
};
pub use prepared::{LayoutStep, PrepareError, PreparedFragment, WriteError};
pub use range::{
    FragmentRange, FragmentRangeManifest, FragmentRangeManifestError, FragmentRangeManifestView,
    FragmentRangeRequest, FragmentRangeVerifyError, VerifiedFragmentRange,
    VerifiedFragmentRangeView,
};
pub use semantic_facts::{
    DecodedOccurrence, OccurrenceCursor, OccurrenceFault, OccurrenceInput, OccurrenceLane,
};
pub use type_facts::{DecodedTypeFact, TypeFactCursor, TypeFactFault, TypeFactInput, TypeFactLane};
pub use view::OccurrenceFault as OccurrenceViewFault;
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    SemanticDataFault, TypeNodeCursor, WireField,
};

pub use wire::{FRAGMENT_MAGIC, FRAGMENT_SCHEMA};
