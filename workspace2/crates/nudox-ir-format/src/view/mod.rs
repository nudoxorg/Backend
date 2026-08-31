mod cursor;
mod directory;
mod error;
mod validate;

pub use cursor::{Atom, AtomCursor, EntityCursor, TypeNodeCursor};
pub use error::{DirectoryFault, FragmentError, WireField};

pub use crate::wire::SectionKind;

pub struct FragmentView<'fragment> {
    pub(super) envelope: &'fragment [u8],
    pub(super) entities: &'fragment [u8],
    pub(super) type_nodes: &'fragment [u8],
    pub(super) atoms: &'fragment [u8],
    pub(super) atom_bytes: &'fragment [u8],
    pub(super) source_identity: crate::SourceIdentity,
}

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
    }
}
