//! Filesystem durability for immutable packs and selected roots.
//!
//! The filesystem layer has three independently checkable boundaries:
//!
//! * immutable pack and closure files are admitted before they are linked;
//! * a publication is a `Prepared -> Durable -> Published` typestate
//!   transition whose journal records bind the exact base, target, layout,
//!   pack, closure, and transaction identity;
//! * recovery validates the complete journal hash chain, pairs every publish
//!   with its exact prepare, and repairs only a truncated final frame.
//!
//! The map helpers are a compatibility facade over these primitives.  Engine
//! owners can retain the typed [`ClosureManifest`] and [`WorkspaceClosure`]
//! objects while choosing their own workspace journal or publication policy.

use super::{
    ClosureId, ClosureManifest, Hash, LayoutId, ObjectEdge, ObjectId, Pack, PackId, RawRelation,
    RelationAdmissionRegistry, StateRoot, StoreError, TypedObject, WorkspaceBinding,
    WorkspaceClosure, decode_pack,
};
use backend_version::WorkspaceRoot;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::atomic::AtomicU64,
    sync::{Arc, Mutex},
};

mod gc;
mod layout;
mod nodes;
mod objects;
mod publication;
mod publication_api;
mod recovery;
mod store_api;

pub use gc::{GcLimits, GcReport, GcRoot, GcRoots};
pub use layout::{
    FileStore, PublicationAuthorityError, PublicationBase, PublicationDescriptor, SelectedHead,
    StorePublicationAuthority, TransactionId,
};
pub use nodes::{
    DurableManifest, DurableManifestPage, DurableTree, ManifestReadStats, OwnedRelationNodeLoader,
    RelationNodeChild, RelationNodeRead, RelationNodeWriteStats, TreeReadStats, TreeWriteStats,
};
pub use objects::ObjectWriteReceipt;
pub use publication::{
    CheckedWorkspacePublication, FileDurable, FilePrepared, FilePublished, WorkspaceFileDurable,
    WorkspaceFilePrepared, WorkspaceFilePublished,
};

#[cfg(test)]
pub(crate) fn set_test_fault(point: u8) {
    recovery::set_test_fault(point);
}

use self::objects::{
    decode_object, decode_pack_file, encode_object, encode_pack_file, write_immutable,
    write_immutable_file_with_status, write_immutable_with_status,
};
use self::recovery::{base_matches, descriptor_matches_base};
pub(super) const PACK_MAGIC: &[u8] = b"LUNA_PACK_V1\0";
pub(super) const JOURNAL_MAGIC: &[u8] = b"LUNA_J2\0";
pub(super) const HEAD_MAGIC: &[u8] = b"LUNA_HEAD2\0";
pub(super) const OBJECT_MAGIC: &[u8] = b"LUNA_OBJECT_V1\0";
pub(super) const JOURNAL_VERSION: u8 = 1;
pub(super) const PREPARED_TAG: u8 = 0;
pub(super) const PUBLISHED_TAG: u8 = 1;
pub(super) const JOURNAL_RECORD_BYTES: usize = JOURNAL_MAGIC.len()
    + 1 // format version
    + 1 // record tag
    + 8 // sequence
    + 8 // paired prepared sequence (zero for prepared records)
    + 32 // predecessor checksum
    + 32 // transaction identity
    + 1 // base root present
    + 32 // base root
    + 32 // target root
    + 32 // layout identity
    + 32 // pack identity
    + 32 // closure identity
    + 1 // workspace binding present
    + 32 // workspace root
    + 32 // workspace closure identity
    + 32 // workspace binding proof
    + 8 // base generation
    + 8 // target generation
    + 32; // checksum
pub(super) const HEAD_RECEIPT_BYTES: usize = 8 + 32 + 8 + 32;
pub(super) const HEAD_BYTES: usize = HEAD_MAGIC.len() + JOURNAL_RECORD_BYTES + HEAD_RECEIPT_BYTES;
pub(super) const ENVELOPE_MULTIPLIER: usize = 4;
pub(super) const ENVELOPE_OVERHEAD: usize = 4096;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<(), StoreError> {
    output.extend_from_slice(
        &u32::try_from(value)
            .map_err(|_| StoreError::Bounds)?
            .to_le_bytes(),
    );
    Ok(())
}

fn put_u64(output: &mut Vec<u8>, value: usize) -> Result<(), StoreError> {
    output.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| StoreError::Bounds)?
            .to_le_bytes(),
    );
    Ok(())
}

fn read_byte(bytes: &[u8], at: &mut usize) -> Result<u8, StoreError> {
    let value = *bytes.get(*at).ok_or(StoreError::Corrupt)?;
    *at = at.checked_add(1).ok_or(StoreError::Bounds)?;
    Ok(value)
}

fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32, StoreError> {
    let end = at.checked_add(4).ok_or(StoreError::Bounds)?;
    let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?;
    *at = end;
    value
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| StoreError::Corrupt)
}

fn read_u64(bytes: &[u8], at: &mut usize) -> Result<u64, StoreError> {
    let end = at.checked_add(8).ok_or(StoreError::Bounds)?;
    let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?;
    *at = end;
    value
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| StoreError::Corrupt)
}

fn read_hash(bytes: &[u8], at: &mut usize) -> Result<Hash, StoreError> {
    let end = at.checked_add(32).ok_or(StoreError::Bounds)?;
    let value = bytes.get(*at..end).ok_or(StoreError::Corrupt)?;
    *at = end;
    value.try_into().map_err(|_| StoreError::Corrupt)
}

fn envelope_limit(max_pack_bytes: usize) -> Result<usize, StoreError> {
    max_pack_bytes
        .checked_mul(ENVELOPE_MULTIPLIER)
        .and_then(|length| length.checked_add(ENVELOPE_OVERHEAD))
        .ok_or(StoreError::Bounds)
}

fn relation_object_limit() -> Result<usize, StoreError> {
    backend_version::DEFAULT_CUT_POLICY
        .max_encoded_bytes()
        .checked_add(OBJECT_MAGIC.len())
        .and_then(|length| length.checked_add(32 + 1 + 2 + 1 + 32 + 32 + 8))
        .ok_or(StoreError::Bounds)
}

fn hex(bytes: &Hash) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error(&error))
}

fn io_error(error: &std::io::Error) -> StoreError {
    StoreError::Io(error.to_string())
}

fn map_read_error(error: &std::io::Error) -> StoreError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StoreError::Corrupt
    } else {
        io_error(error)
    }
}
