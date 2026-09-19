//! Admission of bounded node page windows.

use super::{MerklePage, MerklePageBody};
use crate::ReplicationError;

/// Validates the allocation and continuation part of a page before its body
/// is inspected. Keeping this fence separate means remote adapters can share
/// the same bounded window rule without constructing a traversal cursor.
pub(super) fn validate_window(
    page: &MerklePage,
    max_items: usize,
    max_key_bytes: usize,
) -> Result<usize, ReplicationError> {
    if max_items == 0 || max_key_bytes == 0 {
        return Err(ReplicationError::InvalidLimits);
    }
    let count = match &page.body {
        MerklePageBody::Branch(children) => children.len(),
        MerklePageBody::Leaf(entries) => entries.len(),
    };
    if count > max_items {
        return Err(ReplicationError::MessageTooLarge);
    }
    let count_u32 = u32::try_from(count).map_err(|_| ReplicationError::Overflow)?;
    if let Some(next) = page.next
        && (next.offset <= page.cursor.offset
            || next.offset != page.cursor.offset.saturating_add(count_u32))
    {
        return Err(ReplicationError::InvalidWire);
    }
    Ok(count)
}
