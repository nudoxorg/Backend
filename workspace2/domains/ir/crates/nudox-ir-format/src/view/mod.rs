mod cursor;
mod directory;
mod error;
mod validate;

pub use cursor::{EntityCursor, TypeNodeCursor};
pub use error::{DirectoryFault, FragmentError, WireField};

pub use crate::wire::SectionKind;

pub struct FragmentView<'fragment> {
    pub(super) envelope: &'fragment [u8],
    pub(super) entities: &'fragment [u8],
    pub(super) type_nodes: &'fragment [u8],
}

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
    }
}
