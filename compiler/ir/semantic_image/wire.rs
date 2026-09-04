//! Explicit little-endian grammar cells and directory layout.

use core::fmt;

use super::fault::{CoreSemanticImageFault, CoreSemanticImageField};

pub(crate) const MAGIC: [u8; 4] = *b"NXSI";
pub(crate) const SCHEMA: u16 = 1;
pub(crate) const HEADER_BYTES: usize = 176;
pub(crate) const DIRECTORY_BYTES: usize = 16;
pub(crate) const DIRECTORY_COUNT: usize = 3;
pub(crate) const DIRECTORY_COUNT_U16: u16 = 3;
pub(crate) const ATOM_ROW_BYTES: usize = 8;
pub(crate) const ENTITY_ROW_BYTES: usize = 120;
pub(crate) const NONE: u32 = u32::MAX;

/// One canonical directory lane in a core image.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryKind {
    Atoms = 1,
    AtomBytes = 2,
    Entities = 3,
}

impl DirectoryKind {
    pub(crate) const ALL: [Self; DIRECTORY_COUNT] = [Self::Atoms, Self::AtomBytes, Self::Entities];

    pub(crate) const fn code(self) -> u16 {
        match self {
            Self::Atoms => 1,
            Self::AtomBytes => 2,
            Self::Entities => 3,
        }
    }

}

impl fmt::Display for DirectoryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Borrowed validated offsets for the three subordinate core lanes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CoreImageLayout {
    pub(crate) atom_rows: usize,
    pub(crate) atom_bytes: usize,
    pub(crate) entity_rows: usize,
    pub(crate) atoms: usize,
    pub(crate) bytes: usize,
    pub(crate) entities: usize,
}

#[inline]
pub(crate) fn get_u16(bytes: &[u8], offset: usize, field: CoreSemanticImageField) -> Result<u16, CoreSemanticImageFault> {
    let array = read_array::<2>(bytes, offset, field)?;
    Ok(u16::from_le_bytes(array))
}

#[inline]
pub(crate) fn get_u32(bytes: &[u8], offset: usize, field: CoreSemanticImageField) -> Result<u32, CoreSemanticImageFault> {
    let array = read_array::<4>(bytes, offset, field)?;
    Ok(u32::from_le_bytes(array))
}

#[inline]
pub(crate) fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    field: CoreSemanticImageField,
) -> Result<[u8; N], CoreSemanticImageFault> {
    let end = offset.checked_add(N).ok_or(CoreSemanticImageFault::Truncated { field, offset })?;
    let slice = bytes.get(offset..end).ok_or(CoreSemanticImageFault::Truncated { field, offset })?;
    <[u8; N]>::try_from(slice).map_err(|_| CoreSemanticImageFault::Truncated { field, offset })
}

#[inline]
pub(crate) fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
