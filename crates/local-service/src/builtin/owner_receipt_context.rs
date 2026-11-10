//! Private configured receipt authority bound to the actual leased store.
//! This context does not derive, reload, serialize, or log the owner key.
use super::BuiltinModelError;
use backend_engine::{FileStore, WorkspaceDirectoryAdmission, WorkspaceDirectoryWitness};
use backend_platform::directory::DirectoryCapability;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub(super) struct OwnerReceiptContext {
    key: [u8; 32],
    configured_workspace: PathBuf,
    bound: OnceLock<BoundOwner>,
}
struct BoundOwner {
    admission: WorkspaceDirectoryWitness,
    objects: DirectoryCapability,
    store: FileStore,
    canonical_workspace: PathBuf,
    workspace_digest: [u8; 32],
}
impl std::fmt::Debug for OwnerReceiptContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerReceiptContext")
            .finish_non_exhaustive()
    }
}
fn error(message: &str) -> BuiltinModelError {
    BuiltinModelError(message.to_owned())
}
fn canonical_workspace(path: &Path) -> Result<PathBuf, BuiltinModelError> {
    let value = path
        .canonicalize()
        .map_err(|_| error("configured receipt workspace unavailable"))?;
    let text = value
        .to_str()
        .ok_or_else(|| error("receipt workspace is not UTF-8"))?;
    if text.is_empty() || text.len() > 4096 {
        return Err(error("invalid receipt workspace path"));
    }
    Ok(value)
}
fn workspace_digest(path: &Path) -> Result<[u8; 32], BuiltinModelError> {
    let text = path
        .to_str()
        .ok_or_else(|| error("receipt workspace is not UTF-8"))?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.owner-receipt.workspace-path.v1\0");
    hash.update(&(text.len() as u64).to_be_bytes());
    hash.update(text.as_bytes());
    Ok(*hash.finalize().as_bytes())
}
impl OwnerReceiptContext {
    pub(super) fn new(key: [u8; 32], configured_workspace: PathBuf) -> Self {
        Self {
            key,
            configured_workspace,
            bound: OnceLock::new(),
        }
    }
    /// Bind only after the real owner lease and FileStore have been opened.
    pub(super) fn bind(
        &self,
        admission: &WorkspaceDirectoryAdmission,
        store: &FileStore,
    ) -> Result<(), BuiltinModelError> {
        if self.bound.get().is_some() {
            return Err(error("receipt authority is already bound"));
        }
        let canonical = canonical_workspace(&self.configured_workspace)?;
        let store_parent = store
            .root()
            .parent()
            .ok_or_else(|| error("receipt store has no workspace"))?;
        if canonical_workspace(store_parent)? != canonical
            || store.root().file_name() != Some(std::ffi::OsStr::new("objects"))
        {
            return Err(error("receipt authority belongs to another workspace"));
        }
        let objects = admission
            .verify_existing("objects")
            .map_err(|_| error("receipt owner lease is unavailable"))?;
        store
            .verify_owner_root(&objects)
            .map_err(|_| error("receipt owner and store differ"))?;
        let configured_objects = DirectoryCapability::open(&canonical.join("objects"))
            .map_err(|_| error("configured receipt store unavailable"))?;
        if !objects
            .is_same_source_directory(&configured_objects)
            .map_err(|_| error("receipt directory comparison failed"))?
        {
            return Err(error("receipt configured workspace differs from owner"));
        }
        let bound = BoundOwner {
            admission: admission.downgrade(),
            objects,
            store: store.clone(),
            workspace_digest: workspace_digest(&canonical)?,
            canonical_workspace: canonical,
        };
        // Complete all checks before installing the one-shot binding so a
        // failed attempt does not poison a later retry.
        bound.admission.with_existing(Path::new("objects"), |current| {
            if !bound.objects.is_same_source_directory(current)? {
                return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "owner directory changed"));
            }
            bound.store.verify_owner_root(current).map_err(|_| std::io::Error::new(std::io::ErrorKind::PermissionDenied, "store directory changed"))
        }).map_err(|_| error("receipt owner binding changed"))?;
        self.bound.set(bound).map_err(|_| error("receipt authority is already bound"))
    }
    /// Compare a supplied store's retained handle to the lease-bound object
    /// directory. Call only inside `with_bound` and check before and after IO.
    pub(super) fn verify_store(&self, store: &FileStore) -> Result<(), BuiltinModelError> {
        let bound = self.bound.get().ok_or_else(|| error("receipt authority is not bound"))?;
        store.verify_owner_root(&bound.objects).map_err(|_| error("receipt store identity changed"))
    }
    /// Run one receipt operation while the original owner lease remains held.
    /// Pathname-based store operations are checked on both sides; same-user
    /// namespace replacement is not made atomic.
    pub(super) fn with_bound<T>(&self, operation: impl FnOnce(&[u8; 32], [u8; 32]) -> Result<T, BuiltinModelError>) -> Result<T, BuiltinModelError> {
        let bound = self.bound.get().ok_or_else(|| error("receipt authority is not bound"))?;
        bound.admission.with_existing(Path::new("objects"), |current| {
            let verify = || -> Result<(), BuiltinModelError> {
                if canonical_workspace(&self.configured_workspace)? != bound.canonical_workspace {
                    return Err(error("receipt workspace changed"));
                }
                if !bound.objects.is_same_source_directory(current).map_err(|_| error("receipt owner directory changed"))? {
                    return Err(error("receipt owner directory changed"));
                }
                bound.objects.verify_path(&bound.canonical_workspace.join("objects")).map_err(|_| error("receipt store path changed"))?;
                bound.store.verify_owner_root(&bound.objects).map_err(|_| error("receipt store identity changed"))?;
                Ok(())
            };
            verify().map_err(std::io::Error::other)?;
            let result = operation(&self.key, bound.workspace_digest);
            verify().map_err(std::io::Error::other)?;
            result.map_err(std::io::Error::other)
        }).map_err(|_| error("receipt owner context unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::OwnerLease;

    #[test]
    fn owner_context_rejects_foreign_store_and_does_not_keep_lease() {
        let parent = tempfile::tempdir().expect("private test parent");
        let workspace = parent.path().join("workspace");
        let other = parent.path().join("other");
        let lease = OwnerLease::acquire(&workspace).expect("owner");
        let admission = lease.directory_admission();
        admission.admit("objects").expect("objects");
        let store = FileStore::open(workspace.join("objects"), 1024 * 1024).expect("store");
        let foreign_lease = OwnerLease::acquire(&other).expect("foreign owner");
        let foreign_admission = foreign_lease.directory_admission();
        foreign_admission.admit("objects").expect("foreign objects");
        let foreign = FileStore::open(other.join("objects"), 1024 * 1024).expect("foreign store");
        let context = OwnerReceiptContext::new([42; 32], workspace.clone());
        assert!(context.bind(&admission, &foreign).is_err());
        context.bind(&admission, &store).expect("retry correct binding");
        assert!(context.bind(&admission, &store).is_err());
        assert!(context.with_bound(|_, _| context.verify_store(&foreign)).is_err());
        assert!(context.with_bound(|_, _| context.verify_store(&store)).is_ok());
        assert!(context.with_bound(|key, _| { assert_eq!(key, &[42; 32]); Ok(()) }).is_ok());
        #[cfg(unix)]
        {
        let moved = parent.path().join("relocated");
        std::fs::rename(&workspace, &moved).expect("relocate workspace");
        assert!(context.with_bound(|_, _| Ok(())).is_err());
        std::fs::rename(&moved, &workspace).expect("restore workspace");
        assert!(context.with_bound(|_, _| Ok(())).is_ok());
        let original_objects = workspace.join("objects-original");
        std::fs::rename(workspace.join("objects"), &original_objects).expect("move pinned objects");
        admission.admit("objects").expect("replacement objects");
        assert!(context.with_bound(|_, _| Ok(())).is_err());
        std::fs::remove_dir(workspace.join("objects")).expect("remove replacement");
        std::fs::rename(&original_objects, workspace.join("objects")).expect("restore pinned objects");
        assert!(context.with_bound(|_, _| Ok(())).is_ok());
        }
        drop(admission);
        drop(lease);
        let reopened = OwnerLease::reclaim(&workspace).expect("context does not retain lease");
        assert!(context.with_bound(|_, _| Ok(())).is_err());
        drop(reopened);
    }
}
