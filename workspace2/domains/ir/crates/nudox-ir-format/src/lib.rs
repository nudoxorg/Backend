#![no_std]

mod model;
mod prepared;
mod view;
mod wire;

pub use model::{EntityFault, EntityRecord, EntityType, PrimitiveType, TypeNode, TypeNodeFault};
pub use prepared::{LayoutStep, PrepareError, PreparedFragment, WriteError};
pub use view::{
    DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind, TypeNodeCursor,
    WireField,
};

pub const FRAGMENT_MAGIC: [u8; 4] = *b"NXIR";
pub const FRAGMENT_SCHEMA: u16 = 1;
