#![no_std]

mod model;
mod prepared;
mod view;
mod wire;

pub use model::{PrimitiveType, TypeNode, TypeNodeFault};
pub use prepared::{PrepareError, PreparedFragment, WriteError};
pub use view::{
    DirectoryFault, EntityCursor, FragmentError, FragmentView, KnownSection, TypeNodeCursor,
};

pub const FRAGMENT_MAGIC: [u8; 4] = *b"NXIR";
pub const FRAGMENT_SCHEMA: u16 = 1;
