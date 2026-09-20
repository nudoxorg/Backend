//! Diagnostic sidecar and exact store-head observation helpers.

use super::{
    Boundary, DIAGNOSTIC_FILE, DIAGNOSTIC_MAGIC, Faults, File, FileStore, MAX_STORE_BYTES,
    OpenOptions, Path, PreparedTransition, WorkspaceError, WorkspaceHead, fs,
    verify_workspace_pack,
};
use std::io::Write;

pub(crate) fn write_diagnostic(
    directory: &Path,
    head: &WorkspaceHead,
    faults: &Faults,
) -> Result<(), WorkspaceError> {
    let mut bytes = Vec::with_capacity(DIAGNOSTIC_MAGIC.len() + 8 + 8 + 32 * 3);
    bytes.extend_from_slice(DIAGNOSTIC_MAGIC);
    bytes.extend_from_slice(&head.sequence().to_be_bytes());
    bytes.extend_from_slice(&head.owner_epoch().to_be_bytes());
    bytes.extend_from_slice(&head.root().to_bytes());
    bytes.extend_from_slice(&head.commit().id().to_bytes());
    bytes.extend_from_slice(head.closure().manifest().id().as_bytes());
    atomic_write(&directory.join(DIAGNOSTIC_FILE), &bytes, faults)
}

pub(crate) fn store_head_matches(
    store: &FileStore,
    transition: &PreparedTransition,
    sequence: u64,
) -> Result<bool, WorkspaceError> {
    let Some(selected) = store.head().map_err(WorkspaceError::store)? else {
        return Ok(false);
    };
    let descriptor = selected.descriptor();
    if descriptor.target() != transition.target().to_bytes()
        || descriptor.target_generation() != sequence
        || descriptor.closure().as_bytes() != transition.closure().manifest().id().as_bytes()
    {
        return Ok(false);
    }
    let Some(binding) = descriptor.workspace() else {
        return Ok(false);
    };
    let root = transition.target().to_bytes();
    if binding.root() != &root
        || binding.closure().as_bytes() != transition.closure().manifest().id().as_bytes()
    {
        return Ok(false);
    }
    verify_workspace_pack(store, transition, &descriptor, MAX_STORE_BYTES).map(|()| true)
}

fn atomic_write(path: &Path, bytes: &[u8], faults: &Faults) -> Result<(), WorkspaceError> {
    let parent = path.parent().ok_or(WorkspaceError::Bounds)?;
    fs::create_dir_all(parent).map_err(WorkspaceError::io)?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("head"),
        std::process::id()
    ));
    faults
        .trip(Boundary::TempCreate)
        .map_err(WorkspaceError::Injected)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)
        .map_err(WorkspaceError::io)?;
    faults
        .trip(Boundary::TempWrite)
        .map_err(WorkspaceError::Injected)?;
    file.write_all(bytes).map_err(WorkspaceError::io)?;
    faults
        .trip(Boundary::FileSync)
        .map_err(WorkspaceError::Injected)?;
    file.sync_all().map_err(WorkspaceError::io)?;
    faults
        .trip(Boundary::Rename)
        .map_err(WorkspaceError::Injected)?;
    fs::rename(&tmp, path).map_err(WorkspaceError::io)?;
    faults
        .trip(Boundary::DirSync)
        .map_err(WorkspaceError::Injected)?;
    sync_directory(parent)
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(WorkspaceError::io)
}
