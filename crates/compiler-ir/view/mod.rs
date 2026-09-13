//! Defines view behavior for `compiler-ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
mod cursor;
mod directory;
mod error;
mod validate;

pub use cursor::{Atom, AtomCursor, EntityCursor, TypeNodeCursor};
pub use error::{DirectoryFault, FragmentError, OccurrenceFault, SemanticDataFault, WireField};

pub(crate) use validate::validate_fragment_layout;

pub use crate::wire::SectionKind;

pub struct FragmentView<'fragment> {
    pub(super) envelope: &'fragment [u8],
    pub(super) entities: &'fragment [u8],
    pub(super) type_nodes: &'fragment [u8],
    pub(super) atoms: &'fragment [u8],
    pub(super) atom_bytes: &'fragment [u8],
    pub(super) semantic_data_lane: Option<&'fragment [u8]>,
    pub(super) occurrence_lane: Option<&'fragment [u8]>,
    pub(super) type_fact_lane: Option<&'fragment [u8]>,
    pub(super) documentation_lane: Option<&'fragment [u8]>,
    pub(super) language_extension_lane: Option<&'fragment [u8]>,
    pub(super) extension_pool_lane: Option<&'fragment [u8]>,
    /// Typed source fact validated from the fragment's required identity lane.
    pub source: crate::SourceIdentity,
    /// Typed recipe facts validated from the fragment's required recipe lane.
    pub recipe: crate::RecipeFact,
    pub(crate) layout: crate::wire::FragmentLayout,
}

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
    }
}
