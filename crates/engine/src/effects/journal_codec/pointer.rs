//! Durable snapshot pointer and atomic filesystem primitives.

use super::super::state::EffectError;
use super::snapshot::EffectSnapshotReceipt;
use crate::fault::{Boundary, Faults};
use crate::journal::JournalError;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const SNAPSHOT_POINTER_MAGIC: [u8; 8] = *b"BEFSPTR2";
pub(super) const SNAPSHOT_POINTER_BYTES: usize = 8 + 8 + 8 + 32 + 32;
pub(super) static NEXT_SNAPSHOT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(super) fn containing_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(super) fn snapshot_paths(path: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let file_name = path
        .file_name()
        .map_or_else(|| OsString::from("journal"), OsString::from);
    let mut snapshot_name = file_name.clone();
    snapshot_name.push(".snapshot");
    let mut pointer_name = file_name;
    pointer_name.push(".snapshot.pointer");
    let parent = containing_directory(path);
    (parent.join(snapshot_name), parent.join(pointer_name))
}

pub(super) fn encode_snapshot_pointer(
    receipt: EffectSnapshotReceipt,
) -> [u8; SNAPSHOT_POINTER_BYTES] {
    let mut bytes = [0u8; SNAPSHOT_POINTER_BYTES];
    bytes[..8].copy_from_slice(&SNAPSHOT_POINTER_MAGIC);
    bytes[8..16].copy_from_slice(&receipt.offset.to_be_bytes());
    bytes[16..24].copy_from_slice(&receipt.sequence.to_be_bytes());
    bytes[24..56].copy_from_slice(&receipt.chain);
    bytes[56..88].copy_from_slice(&receipt.record);
    bytes
}

pub(super) fn decode_snapshot_pointer(bytes: &[u8]) -> Result<EffectSnapshotReceipt, JournalError> {
    if bytes.len() != SNAPSHOT_POINTER_BYTES || bytes[..8] != SNAPSHOT_POINTER_MAGIC {
        return Err(JournalError::Corrupt("snapshot pointer"));
    }
    Ok(EffectSnapshotReceipt {
        offset: u64::from_be_bytes(
            bytes[8..16]
                .try_into()
                .map_err(|_| JournalError::Corrupt("snapshot pointer"))?,
        ),
        sequence: u64::from_be_bytes(
            bytes[16..24]
                .try_into()
                .map_err(|_| JournalError::Corrupt("snapshot pointer"))?,
        ),
        chain: bytes[24..56]
            .try_into()
            .map_err(|_| JournalError::Corrupt("snapshot pointer"))?,
        record: bytes[56..88]
            .try_into()
            .map_err(|_| JournalError::Corrupt("snapshot pointer"))?,
    })
}

pub(super) fn read_snapshot_pointer(
    path: &Path,
) -> Result<Option<EffectSnapshotReceipt>, EffectError> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(EffectError::Persistence(error.to_string())),
    };
    if length
        != u64::try_from(SNAPSHOT_POINTER_BYTES)
            .map_err(|_| EffectError::Journal(JournalError::Bounds))?
    {
        return Err(EffectError::Journal(JournalError::Corrupt(
            "snapshot pointer",
        )));
    }
    let bytes = fs::read(path).map_err(|error| EffectError::Persistence(error.to_string()))?;
    decode_snapshot_pointer(&bytes)
        .map(Some)
        .map_err(EffectError::Journal)
}

fn temporary_pointer_path(path: &Path) -> std::path::PathBuf {
    let nonce = NEXT_SNAPSHOT_TEMP.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map_or_else(|| OsString::from("snapshot.pointer"), OsString::from);
    let mut temporary = file_name;
    temporary.push(format!(".tmp.{}.{}", std::process::id(), nonce));
    containing_directory(path).join(temporary)
}

pub(super) fn write_snapshot_pointer(
    path: &Path,
    receipt: EffectSnapshotReceipt,
    faults: &Faults,
) -> Result<(), EffectError> {
    let temporary = temporary_pointer_path(path);
    let bytes = encode_snapshot_pointer(receipt);
    let mut created = false;
    let result = (|| -> Result<(), EffectError> {
        faults
            .trip(Boundary::TempCreate)
            .map_err(EffectError::Injected)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| EffectError::Persistence(error.to_string()))?;
        created = true;
        faults
            .trip(Boundary::TempWrite)
            .map_err(EffectError::Injected)?;
        file.write_all(&bytes)
            .map_err(|error| EffectError::Persistence(error.to_string()))?;
        faults
            .trip(Boundary::FileSync)
            .map_err(EffectError::Injected)?;
        file.sync_all()
            .map_err(|error| EffectError::Persistence(error.to_string()))?;
        faults
            .trip(Boundary::Rename)
            .map_err(EffectError::Injected)?;
        fs::rename(&temporary, path)
            .map_err(|error| EffectError::Persistence(error.to_string()))?;
        let parent = containing_directory(path);
        faults
            .trip(Boundary::DirSync)
            .map_err(EffectError::Injected)?;
        backend_platform::durability::open_directory(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| EffectError::Persistence(error.to_string()))?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temporary);
    }
    result
}
