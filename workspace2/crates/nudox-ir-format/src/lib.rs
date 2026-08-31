#![no_std]

#[cfg(target_pointer_width = "16")]
compile_error!("nudox-ir-format requires at least a 32-bit address space");

mod model;
mod prepared;
mod range;
mod view;
mod wire;

pub use model::{
    AtomFault, AtomInput, EntityFault, EntityKind, EntityNameFault, EntityRecord,
    EntityRecordFault, EntityType, PrimitiveType, RecipeFact, RecipeFactFault, SourceIdentity,
    SourceIdentityFault, TypeNode, TypeNodeFault,
};
pub use prepared::{LayoutStep, PrepareError, PreparedFragment, WriteError};
pub use range::{
    FragmentRange, FragmentRangeManifest, FragmentRangeManifestError, FragmentRangeManifestView,
    FragmentRangeRequest, FragmentRangeVerifyError, VerifiedFragmentRange,
    VerifiedFragmentRangeView,
};
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    TypeNodeCursor, WireField,
};

pub const FRAGMENT_MAGIC: [u8; 4] = *b"NXIR";
pub const FRAGMENT_SCHEMA: u16 = 1;
