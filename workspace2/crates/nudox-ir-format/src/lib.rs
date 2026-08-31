#![no_std]

mod model;
mod prepared;
mod view;
mod wire;

pub use model::{
    AtomFault, AtomInput, EntityFault, EntityKind, EntityNameFault, EntityRecord,
    EntityRecordFault, EntityType, PrimitiveType, SourceIdentity, SourceIdentityFault, TypeNode,
    TypeNodeFault,
};
pub use prepared::{LayoutStep, PrepareError, PreparedFragment, WriteError};
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    TypeNodeCursor, WireField,
};

pub const FRAGMENT_MAGIC: [u8; 4] = *b"NXIR";
pub const FRAGMENT_SCHEMA: u16 = 1;
