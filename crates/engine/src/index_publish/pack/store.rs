//! Defines pack store behavior for `backend-engine index_publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack store invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Content-addressed first-write-wins filesystem publication for immutable index packs.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::index_publish::pack::{
    IndexPackPlan,
    encode::{IndexPackStreamError, stream_index_pack},
    error::{
        IndexPackCleanup, IndexPackConflict, IndexPackEncodeError, IndexPackPathRole,
        IndexPackStoreError, IndexPackStorePhase, StoredIndexPack,
    },
    grammar::MAX_INDEX_PACK_BYTES,
    view::IndexPack,
};
use backend_semantic::index_vocabulary::IndexPackId;

const TEMPORARY_NAME_ATTEMPTS: usize = 16;
/// One byte beyond the bounded owner maximum detects a hostile short metadata report.
const REOPEN_LIMIT_SENTINEL_BYTES: u64 = 1;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One local directory that durably publishes immutable content-addressed index packs.
pub struct IndexPackStore {
    directory: PathBuf,
}

impl IndexPackStore {
    /// Creates the store directory when absent and binds all later paths to that one location.
    ///
    /// # Errors
    ///
    /// Returns the exact directory creation failure with its path role and operating-system cause.
    pub fn create(directory: impl AsRef<Path>) -> Result<Self, IndexPackStoreError> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory).map_err(|source| IndexPackStoreError::Io {
            phase: IndexPackStorePhase::CreateDirectory,
            role: IndexPackPathRole::Directory,
            path: directory.clone(),
            source,
        })?;
        Ok(Self { directory })
    }

    /// Streams a preflighted compiler-derived pack into a stable first-write-wins immutable path.
    ///
    /// The temporary inode is fully synchronized before linking it to the content-addressed final
    /// name. Directory synchronization follows visibility. A coherent duplicate is idempotent;
    /// a malformed or semantically conflicting existing path remains an exact error.
    ///
    /// # Errors
    ///
    /// Returns an exact encode, write, sync, link, duplicate-authority, or cleanup failure.
    pub fn publish(
        &self,
        plan: &IndexPackPlan<'_>,
    ) -> Result<StoredIndexPack, IndexPackStoreError> {
        let (temporary_path, mut temporary) = self.create_temporary()?;
        let identity = match stream_index_pack(plan, |bytes| temporary.write_all(bytes)) {
            Ok(identity) => identity,
            Err(IndexPackStreamError::Encode(source)) => {
                return Err(remove_after_encode_failure(temporary_path, source));
            }
            Err(IndexPackStreamError::Write(source)) => {
                return Err(remove_after_failure(
                    temporary_path,
                    IndexPackStorePhase::WriteTemporary,
                    IndexPackPathRole::Temporary,
                    source,
                ));
            }
        };
        if let Err(source) = temporary.sync_all() {
            return Err(remove_after_failure(
                temporary_path,
                IndexPackStorePhase::SyncTemporary,
                IndexPackPathRole::Temporary,
                source,
            ));
        }
        drop(temporary);
        let path = self.final_path(identity);
        match fs::hard_link(&temporary_path, &path) {
            Ok(()) => self.finish_visibility(temporary_path, identity, plan, path),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                self.finish_duplicate(temporary_path, identity, plan, path)
            }
            Err(source) => Err(remove_after_failure(
                temporary_path,
                IndexPackStorePhase::LinkFinal,
                IndexPackPathRole::Final,
                source,
            )),
        }
    }

    /// Reopens one final pack after process death and verifies the requested content identity.
    ///
    /// # Errors
    ///
    /// Returns bounded-read, operating-system, or immutable-pack validation failures with their
    /// exact path and source facts.
    pub fn open(&self, id: IndexPackId) -> Result<IndexPack<Vec<u8>>, IndexPackStoreError> {
        let path = self.final_path(id);
        let bytes = Self::read_owner(&path)?;
        IndexPack::open(bytes, id).map_err(|rejected| IndexPackStoreError::ExistingInvalid {
            path,
            source: Box::new(rejected.error),
        })
    }

    fn create_temporary(&self) -> Result<(PathBuf, File), IndexPackStoreError> {
        for _ in 0..TEMPORARY_NAME_ATTEMPTS {
            let path = self.temporary_path();
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(IndexPackStoreError::Io {
                        phase: IndexPackStorePhase::CreateTemporary,
                        role: IndexPackPathRole::Temporary,
                        path,
                        source,
                    });
                }
            }
        }
        Err(IndexPackStoreError::TemporaryNameExhausted {
            directory: self.directory.clone(),
            attempts: TEMPORARY_NAME_ATTEMPTS,
        })
    }

    fn finish_visibility(
        &self,
        temporary: PathBuf,
        identity: IndexPackId,
        plan: &IndexPackPlan<'_>,
        path: PathBuf,
    ) -> Result<StoredIndexPack, IndexPackStoreError> {
        if let Err(source) = sync_directory(&self.directory) {
            return Err(IndexPackStoreError::VisibilityUncertain {
                id: identity,
                path,
                temporary,
                source,
            });
        }
        let cleanup = cleanup_visible_temporary(temporary);
        Ok(StoredIndexPack {
            id: identity,
            generation: plan.generation,
            snapshot: plan.snapshot,
            path,
            cleanup,
        })
    }

    fn finish_duplicate(
        &self,
        temporary: PathBuf,
        identity: IndexPackId,
        plan: &IndexPackPlan<'_>,
        path: PathBuf,
    ) -> Result<StoredIndexPack, IndexPackStoreError> {
        match self.open(identity) {
            Ok(existing)
                if existing.generation == plan.generation && existing.snapshot == plan.snapshot =>
            {
                let cleanup = cleanup_visible_temporary(temporary);
                Ok(StoredIndexPack {
                    id: identity,
                    generation: plan.generation,
                    snapshot: plan.snapshot,
                    path,
                    cleanup,
                })
            }
            Ok(existing) => Self::finish_conflicting_duplicate(temporary, &existing, plan),
            Err(IndexPackStoreError::ExistingInvalid { path, source }) => {
                Self::finish_invalid_duplicate(temporary, path, source)
            }
            Err(error) => Err(remove_duplicate_after_open_failure(temporary, error)),
        }
    }

    fn finish_conflicting_duplicate(
        temporary: PathBuf,
        existing: &IndexPack<Vec<u8>>,
        plan: &IndexPackPlan<'_>,
    ) -> Result<StoredIndexPack, IndexPackStoreError> {
        let facts = Box::new(IndexPackConflict {
            existing_generation: existing.generation,
            attempted_generation: plan.generation,
            existing_snapshot: existing.snapshot,
            attempted_snapshot: plan.snapshot,
        });
        match fs::remove_file(&temporary) {
            Ok(()) => Err(IndexPackStoreError::Conflict { facts }),
            Err(cleanup) => Err(IndexPackStoreError::ConflictAndCleanup {
                facts,
                temporary,
                cleanup,
            }),
        }
    }

    fn finish_invalid_duplicate(
        temporary: PathBuf,
        path: PathBuf,
        source: Box<crate::index_publish::pack::IndexPackOpenError>,
    ) -> Result<StoredIndexPack, IndexPackStoreError> {
        match fs::remove_file(&temporary) {
            Ok(()) => Err(IndexPackStoreError::ExistingInvalid { path, source }),
            Err(cleanup) => Err(IndexPackStoreError::ExistingInvalidAndCleanup {
                path,
                source,
                temporary,
                cleanup,
            }),
        }
    }

    fn final_path(&self, id: IndexPackId) -> PathBuf {
        self.directory.join(format!("{id}.idxpack"))
    }

    fn temporary_path(&self) -> PathBuf {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.directory.join(format!(
            ".index-pack-{}-{sequence}.pending",
            std::process::id()
        ))
    }

    fn read_owner(path: &Path) -> Result<Vec<u8>, IndexPackStoreError> {
        let mut file = File::open(path).map_err(|source| read_error(path, source))?;
        let declared = file
            .metadata()
            .map_err(|source| read_error(path, source))?
            .len();
        let limit = u64::try_from(MAX_INDEX_PACK_BYTES)
            .map_err(|source| read_error(path, io::Error::other(source)))?;
        if declared > limit {
            return Err(IndexPackStoreError::ByteLimit {
                path: path.to_path_buf(),
                observed: declared,
                limit: MAX_INDEX_PACK_BYTES,
            });
        }
        let declared = usize::try_from(declared)
            .map_err(|source| read_error(path, io::Error::other(source)))?;
        let mut bytes = Vec::with_capacity(declared);
        Read::by_ref(&mut file)
            .take(limit.saturating_add(REOPEN_LIMIT_SENTINEL_BYTES))
            .read_to_end(&mut bytes)
            .map_err(|source| read_error(path, source))?;
        if bytes.len() > MAX_INDEX_PACK_BYTES {
            return Err(IndexPackStoreError::ByteLimit {
                path: path.to_path_buf(),
                observed: u64::try_from(bytes.len())
                    .map_err(|source| read_error(path, io::Error::other(source)))?,
                limit: MAX_INDEX_PACK_BYTES,
            });
        }
        Ok(bytes)
    }
}

fn read_error(path: &Path, source: io::Error) -> IndexPackStoreError {
    IndexPackStoreError::Io {
        phase: IndexPackStorePhase::ReadFinal,
        role: IndexPackPathRole::Final,
        path: path.to_path_buf(),
        source,
    }
}

fn sync_directory(directory: &Path) -> io::Result<()> {
    backend_platform::durability::open_directory(directory).and_then(|file| file.sync_all())
}

fn cleanup_visible_temporary(path: PathBuf) -> IndexPackCleanup {
    match fs::remove_file(&path) {
        Ok(()) => IndexPackCleanup::Removed,
        Err(source) => IndexPackCleanup::Retained { path, source },
    }
}

fn remove_after_failure(
    temporary: PathBuf,
    phase: IndexPackStorePhase,
    role: IndexPackPathRole,
    source: io::Error,
) -> IndexPackStoreError {
    match fs::remove_file(&temporary) {
        Ok(()) => IndexPackStoreError::Io {
            phase,
            role,
            path: temporary,
            source,
        },
        Err(cleanup) => IndexPackStoreError::IoAndCleanup {
            phase,
            role,
            path: temporary.clone(),
            source,
            temporary,
            cleanup,
        },
    }
}

fn remove_after_encode_failure(
    temporary: PathBuf,
    source: IndexPackEncodeError,
) -> IndexPackStoreError {
    match fs::remove_file(&temporary) {
        Ok(()) => IndexPackStoreError::Encode { source },
        Err(cleanup) => IndexPackStoreError::EncodeAndCleanup {
            source,
            temporary,
            cleanup,
        },
    }
}

fn remove_duplicate_after_open_failure(
    temporary: PathBuf,
    error: IndexPackStoreError,
) -> IndexPackStoreError {
    match fs::remove_file(&temporary) {
        Ok(()) => error,
        Err(cleanup) => IndexPackStoreError::DuplicateOpenAndCleanup {
            source: Box::new(error),
            temporary,
            cleanup,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io, sync::atomic::AtomicUsize};

    use super::*;

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, thiserror::Error)]
    enum StoreTestError {
        #[error("could not create sparse bounded-read fixture")]
        Io(#[from] io::Error),
        #[error("native pack limit could not become the sparse-file length type")]
        Address(#[source] core::num::TryFromIntError),
        #[error("bounded sparse-file fixture length overflowed")]
        LengthOverflow,
        #[error("bounded owner read returned an unexpected terminal")]
        Terminal,
    }

    #[test]
    fn hostile_metadata_cannot_allocate_an_unbounded_reopen_owner() -> Result<(), StoreTestError> {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "server-index-pack-size-limit-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let path = directory.join("oversized.idxpack");
        let file = File::create(&path)?;
        let observed = u64::try_from(MAX_INDEX_PACK_BYTES)
            .map_err(StoreTestError::Address)?
            .checked_add(1)
            .ok_or(StoreTestError::LengthOverflow)?;
        file.set_len(observed)?;
        let result = IndexPackStore::read_owner(&path);
        let cleanup = fs::remove_dir_all(&directory);
        match (result, cleanup) {
            (
                Err(IndexPackStoreError::ByteLimit {
                    path: rejected,
                    observed: actual,
                    limit,
                }),
                Ok(()),
            ) if rejected == path && actual == observed && limit == MAX_INDEX_PACK_BYTES => Ok(()),
            (_, Err(error)) => Err(StoreTestError::Io(error)),
            _ => Err(StoreTestError::Terminal),
        }
    }
}
