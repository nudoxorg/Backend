//! Compatibility facade for the core-image model invariant.
//!
//! The portable grammar is intentionally split by ownership: `wire` owns
//! fixed cells and directory layout, `fault` owns closed diagnostics, and
//! `decode` owns semantic reconstruction after byte-level validation. This
//! facade keeps sibling modules from depending on that split as an API.

pub(crate) use super::decode::{decode_entity, decode_image_facts};
pub(crate) use super::fault::{
    CoreAuthorityFault, CoreAuthorityPlane, CoreSemanticImageFault,
    CoreSemanticImageField,
};
pub(crate) use super::wire::{
    get_u16, get_u32, put_u16, put_u32, read_array, CoreImageLayout, DirectoryKind,
    ATOM_ROW_BYTES, DIRECTORY_BYTES, DIRECTORY_COUNT, DIRECTORY_COUNT_U16, ENTITY_ROW_BYTES,
    HEADER_BYTES, MAGIC, NONE, SCHEMA,
};
