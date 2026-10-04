//! Durable, content-addressed Tantivy projections for individual lexical segments.
//!
//! The Tantivy directory is an acceleration structure only.  The adjacent canonical sidecar is
//! reopened and checked against the segment identity before the directory is admitted, and query
//! composition uses those facts for update and tombstone semantics.  This keeps backend scores
//! and document ordinals outside the authority boundary.

use super::{
    model::{
        MAX_DURABLE_OPERATIONS, MAX_DURABLE_QUERY_BYTES, StorePhase, TantivyCandidate,
        TantivySegment, TantivySegmentHit, TantivySegmentStore, TantivySegmentStoreError,
        TantivySnapshot,
    },
    publication, query,
};

use std::{
    cmp::Ordering,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
};

use crate::publish::{
    DirectoryPublication, MovedAside, PrivateNamespace, StagePreparationFailure, TransientDenial,
    backend_is_transient_denial, failed_stage_preparation_with, io_is_transient_denial,
    retry_while_denied,
};
use backend_semantic::index_core::{
    IndexSnapshot, IndexSnapshotId, LexicalOperation, LexicalSegment, LexicalSegmentId,
};

impl<'segment> TantivySnapshot<'_, 'segment> {
    /// Returns the immutable snapshot identity retained by this proof.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Searches canonical facts through the already validated newest-first selection.
    ///
    /// # Errors
    ///
    /// Returns a typed backend, canonical-validation, capacity, or cancellation error.
    #[allow(
        clippy::indexing_slicing,
        reason = "query composition preflights every caller-owned range"
    )]
    pub fn search<'output>(
        &self,
        operation: LexicalOperation<'_>,
        requested_limit: usize,
        output: &'output mut [Option<TantivySegmentHit<'segment>>],
        candidates: &mut [Option<TantivyCandidate<'segment>>],
    ) -> Result<usize, TantivySegmentStoreError> {
        query::search_operation(
            self.snapshot,
            self.segments,
            operation,
            requested_limit,
            output,
            candidates,
            None,
        )
    }

    /// Performs preflight and post-query cancellation samples around the bounded blocking call.
    ///
    /// # Errors
    ///
    /// Returns a typed backend, canonical-validation, capacity, or cancellation error.
    pub fn search_cancelable<'output>(
        &self,
        operation: LexicalOperation<'_>,
        requested_limit: usize,
        output: &'output mut [Option<TantivySegmentHit<'segment>>],
        candidates: &mut [Option<TantivyCandidate<'segment>>],
        cancelled: &AtomicBool,
    ) -> Result<usize, TantivySegmentStoreError> {
        if cancelled.load(AtomicOrdering::Acquire) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        let result = query::search_operation(
            self.snapshot,
            self.segments,
            operation,
            requested_limit,
            output,
            candidates,
            Some(cancelled),
        )?;
        if cancelled.load(AtomicOrdering::Acquire) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        Ok(result)
    }

    /// Searches a caller-owned list of exact/prefix operations, retaining one deterministic
    /// result per live document. The operation list is the closed grammar; no backend parser is
    /// authoritative.
    ///
    /// # Errors
    ///
    /// Returns a typed preflight, backend, canonical-validation, or capacity error without
    /// mutating the output on a rejected operation.
    #[allow(
        clippy::indexing_slicing,
        reason = "all bounded accumulator ranges are checked before mutation"
    )]
    pub fn search_terms<'output>(
        &self,
        operations: &[LexicalOperation<'_>],
        requested_limit: usize,
        output: &'output mut [Option<TantivySegmentHit<'segment>>],
        scratch: &mut [Option<TantivySegmentHit<'segment>>],
        candidates: &mut [Option<TantivyCandidate<'segment>>],
    ) -> Result<usize, TantivySegmentStoreError> {
        if requested_limit > output.len() {
            return Err(TantivySegmentStoreError::OutputCapacity {
                required: requested_limit,
                available: output.len(),
            });
        }
        if operations.len() > MAX_DURABLE_OPERATIONS {
            return Err(TantivySegmentStoreError::QueryTermLimit {
                limit: MAX_DURABLE_OPERATIONS,
                observed: operations.len(),
            });
        }
        let query_bytes = operations
            .iter()
            .try_fold(0_usize, |total, operation| {
                total.checked_add(operation.term.len())
            })
            .ok_or(TantivySegmentStoreError::CountOverflow)?;
        if query_bytes > MAX_DURABLE_QUERY_BYTES {
            return Err(TantivySegmentStoreError::QueryBytesLimit {
                limit: MAX_DURABLE_QUERY_BYTES,
                observed: query_bytes,
            });
        }
        let scratch_required = requested_limit
            .checked_mul(2)
            .ok_or(TantivySegmentStoreError::CountOverflow)?;
        if scratch.len() < scratch_required {
            return Err(TantivySegmentStoreError::OutputCapacity {
                required: scratch_required,
                available: scratch.len(),
            });
        }
        let (accumulator, operation_scratch) = scratch.split_at_mut(requested_limit);
        for slot in accumulator.iter_mut() {
            *slot = None;
        }
        for operation in operations {
            let count = query::search_operation(
                self.snapshot,
                self.segments,
                *operation,
                requested_limit,
                operation_scratch,
                candidates,
                None,
            )?;
            let mut total = accumulator.iter().flatten().count();
            for hit in operation_scratch[..count].iter().flatten().copied() {
                if let Some(existing_index) = accumulator[..total].iter().position(|existing| {
                    existing.is_some_and(|existing| {
                        existing.provenance.document == hit.provenance.document
                    })
                }) {
                    let existing = accumulator[existing_index]
                        .ok_or(TantivySegmentStoreError::CompositionCapacity)?;
                    if query::compare_hits(&hit, &existing).is_lt() {
                        accumulator[existing_index] = Some(hit);
                    }
                    continue;
                }
                let position = match accumulator[..total].iter().position(|existing| {
                    existing.is_some_and(|existing| query::compare_hits(&hit, &existing).is_lt())
                }) {
                    Some(position) => position,
                    None => total,
                };
                if position < requested_limit {
                    let new_count = (total + 1).min(requested_limit);
                    for destination in (position + 1..new_count).rev() {
                        accumulator[destination] = accumulator[destination - 1];
                    }
                    accumulator[position] = Some(hit);
                    total = new_count;
                }
            }
            accumulator[..total].sort_unstable_by(|left, right| match (left, right) {
                (Some(left), Some(right)) => query::compare_hits(left, right),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            });
        }
        let total = accumulator.iter().flatten().count();
        for (destination, source) in output.iter_mut().zip(accumulator.iter()).take(total) {
            *destination = *source;
        }
        for slot in output.iter_mut().skip(total) {
            *slot = None;
        }
        Ok(total)
    }
}

impl TantivySegmentStore {
    /// Creates (or opens) a store rooted at the caller-selected directory.
    ///
    /// # Errors
    ///
    /// Returns a typed filesystem error when the root or versioned recipe directory cannot be
    /// created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, TantivySegmentStoreError> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|source| io_error(StorePhase::CreateRoot, &root, source))?;
        publication::ensure_recipe_root(&root)
            .map_err(|source| io_error(StorePhase::CreateRoot, &root, source))?;
        Ok(Self { root })
    }

    /// Returns the explicit store root without changing ownership of it.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Builds, atomically publishes, and reopens a projection for one validated segment.
    ///
    /// Existing valid content is reused without rebuilding.  A corrupt or incomplete final
    /// directory is quarantined only after a complete replacement has been prepared.  Temporary
    /// directories never use the final content-addressed name, so a pre-publish interruption
    /// cannot create durable authority.
    ///
    /// # Errors
    ///
    /// Returns a typed publication, validation, backend, or canonical admission error.
    #[allow(
        clippy::if_not_else,
        reason = "directory validation and non-directory quarantine have distinct typed paths"
    )]
    pub fn project(
        &self,
        segment: LexicalSegment<'_>,
    ) -> Result<TantivySegment, TantivySegmentStoreError> {
        let namespace = publication::open_recipe_namespace(&self.root)
            .map_err(|source| io_error(StorePhase::CreateRoot, &self.root, source))?;
        let final_name = publication::segment_name(segment.id);
        let final_dir = publication::segment_path(&self.root, segment.id);
        let mut quarantine = None;
        if path_is_real_directory(&final_dir)
            .map_err(|source| io_error(StorePhase::Reopen, &final_dir, source))?
        {
            match publication::open_segment(&final_dir, segment.id) {
                Ok(opened) => return Ok(opened),
                Err(error) if !error.rebuildable() => return Err(error),
                Err(_) => {}
            }
        }

        // A scanner or indexer that briefly holds a just-written file can make
        // the operating system refuse the build; a fresh stage is attempted
        // again before the failure is reported.
        let temp_stage = retry_while_denied(|| {
            let stage = publication::new_temp_stage(&namespace, segment.id)
                .map_err(StagePreparationFailure::for_attempt)?;
            match publication::build_projection(stage.path(), segment) {
                Ok(()) => Ok(stage),
                Err(error) => Err(failed_stage_preparation_with(stage, error, |cleanup| {
                    io_error(StorePhase::CreateTemporary, &final_dir, cleanup)
                })),
            }
        })
        .map_err(StagePreparationFailure::into_error)?;
        if path_entry_exists(&final_dir)
            .map_err(|source| io_error(StorePhase::Reopen, &final_dir, source))?
        {
            if !path_is_real_directory(&final_dir)
                .map_err(|source| io_error(StorePhase::Reopen, &final_dir, source))?
            {
                quarantine = self.quarantine(&namespace, &final_name, segment.id)?;
            } else {
                match publication::open_segment(&final_dir, segment.id) {
                    Ok(opened) => {
                        temp_stage.discard().map_err(|source| {
                            io_error(StorePhase::CreateTemporary, &final_dir, source)
                        })?;
                        return Ok(opened);
                    }
                    Err(error) if !error.rebuildable() => {
                        temp_stage.discard().map_err(|source| {
                            io_error(StorePhase::CreateTemporary, &final_dir, source)
                        })?;
                        return Err(error);
                    }
                    Err(_) => {}
                }
                quarantine = self.quarantine(&namespace, &final_name, segment.id)?;
            }
        }
        let publication = temp_stage
            .publish(&final_name)
            .map_err(|source| io_error(StorePhase::Publish, &final_dir, source.into_io_error()))?;
        match publication {
            DirectoryPublication::Published => finish_open(
                publication::open_segment(&final_dir, segment.id),
                &namespace,
                quarantine,
            ),
            // Another publisher named this exact segment first; its directory
            // is validated like any other existing final directory.
            DirectoryPublication::AlreadyPresent => finish_open(
                publication::open_segment(&final_dir, segment.id),
                &namespace,
                quarantine,
            ),
        }
    }

    /// Moves a corrupt or foreign final entry aside, returning where it went.
    /// A replacement stage that cannot proceed is discarded.
    fn quarantine(
        &self,
        namespace: &PrivateNamespace,
        entry: &str,
        id: LexicalSegmentId,
    ) -> Result<Option<String>, TantivySegmentStoreError> {
        let name = publication::quarantine_name(id);
        match namespace.move_aside(entry, &name) {
            Ok(MovedAside::Moved) => Ok(Some(name)),
            Ok(MovedAside::Vanished) => Ok(None),
            Err(source) => Err(io_error(
                StorePhase::Quarantine,
                &namespace.path().join(entry),
                source,
            )),
        }
    }

    /// Builds with cancellation samples at the publication boundary.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`Self::project`] or cancellation when requested.
    pub fn project_cancelable(
        &self,
        segment: LexicalSegment<'_>,
        cancelled: &AtomicBool,
    ) -> Result<TantivySegment, TantivySegmentStoreError> {
        if cancelled.load(AtomicOrdering::Acquire) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        let result = self.project(segment);
        if cancelled.load(AtomicOrdering::Acquire) {
            return Err(TantivySegmentStoreError::Cancelled);
        }
        result
    }

    /// Reopens an already published projection and validates its canonical sidecar.
    ///
    /// # Errors
    ///
    /// Returns a typed missing, corruption, identity, filesystem, or backend error.
    pub fn reopen(&self, id: LexicalSegmentId) -> Result<TantivySegment, TantivySegmentStoreError> {
        let namespace = publication::open_recipe_namespace(&self.root)
            .map_err(|source| io_error(StorePhase::CreateRoot, &self.root, source))?;
        let name = publication::segment_name(id);
        match namespace.open_private_dir(&name) {
            Ok(directory) => drop(directory),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(TantivySegmentStoreError::Missing { id });
            }
            Err(source) => {
                return Err(io_error(
                    StorePhase::Reopen,
                    &namespace.path().join(&name),
                    source,
                ));
            }
        }
        let path = publication::segment_path(&self.root, id);
        publication::open_segment(&path, id)
    }

    /// Proves that opened segment directories cover one validated snapshot selection.
    ///
    /// # Errors
    ///
    /// Returns a typed selection/count/order error when the opened segments do not exactly match
    /// the pinned newest-first snapshot.
    pub fn compose<'selection, 'segment>(
        &self,
        snapshot: IndexSnapshot<'_>,
        segments: &'selection [&'segment TantivySegment],
    ) -> Result<TantivySnapshot<'selection, 'segment>, TantivySegmentStoreError> {
        if segments.len() != snapshot.lexical.len() {
            return Err(TantivySegmentStoreError::SnapshotSelection {
                expected: snapshot.lexical.len(),
                observed: segments.len(),
            });
        }
        for (position, segment) in segments.iter().enumerate() {
            if snapshot.lexical.get(position).copied() != Some(segment.id) {
                return Err(TantivySegmentStoreError::SnapshotSegmentOrder {
                    position,
                    expected: snapshot.lexical.get(position).copied(),
                    observed: segment.id,
                });
            }
        }
        Ok(TantivySnapshot {
            snapshot: snapshot.id,
            segments,
        })
    }
}

pub(crate) fn io_error(
    phase: StorePhase,
    path: &Path,
    source: io::Error,
) -> TantivySegmentStoreError {
    TantivySegmentStoreError::Io {
        phase,
        path: path.to_path_buf(),
        source,
    }
}
pub(crate) fn backend_error(
    phase: StorePhase,
    path: &Path,
    source: tantivy::TantivyError,
) -> TantivySegmentStoreError {
    TantivySegmentStoreError::Tantivy {
        phase,
        path: path.to_path_buf(),
        source,
    }
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn path_is_real_directory(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Removes a quarantined entry. The goal is that it is gone, so one that has
/// already vanished is success, and a scanner briefly holding a file inside it
/// is waited out like any other transient denial.
fn remove_quarantine_entry(
    namespace: &PrivateNamespace,
    name: &str,
) -> Result<(), TantivySegmentStoreError> {
    let path = namespace.path().join(name);
    retry_while_denied(|| {
        namespace
            .remove_entry(name)
            .map_err(|source| io_error(StorePhase::Quarantine, &path, source))
    })
}

fn finish_open(
    result: Result<TantivySegment, TantivySegmentStoreError>,
    namespace: &PrivateNamespace,
    quarantine: Option<String>,
) -> Result<TantivySegment, TantivySegmentStoreError> {
    if result.is_ok()
        && let Some(name) = quarantine
    {
        remove_quarantine_entry(namespace, &name)?;
    }
    result
}

impl TransientDenial for TantivySegmentStoreError {
    fn is_transient_denial(&self) -> bool {
        match self {
            Self::Io { source, .. } => io_is_transient_denial(source),
            Self::Tantivy { source, .. } => backend_is_transient_denial(source),
            _ => false,
        }
    }
}

#[cfg(test)]
mod quarantine_tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nudox-tantivy-quarantine-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    #[test]
    fn removing_a_quarantine_entry_that_is_already_gone_succeeds() {
        let parent = scratch("gone");
        let namespace =
            PrivateNamespace::open_child(&parent, "private-v1").expect("private namespace");
        // The entry was removed by someone else first: the goal is met.
        remove_quarantine_entry(&namespace, ".segment.corrupt-1-1").expect("already gone");
        drop(namespace);
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn a_quarantined_directory_and_a_quarantined_file_are_both_removed() {
        let parent = scratch("both");
        let namespace =
            PrivateNamespace::open_child(&parent, "private-v1").expect("private namespace");
        let directory = namespace.path().join(".a.corrupt-1-2");
        fs::create_dir_all(directory.join("tantivy")).expect("quarantined directory");
        fs::write(directory.join("tantivy").join("meta.json"), b"{}").expect("content");
        let file = namespace.path().join(".b.corrupt-1-3");
        fs::write(&file, b"stray").expect("quarantined file");

        remove_quarantine_entry(&namespace, ".a.corrupt-1-2").expect("directory removed");
        remove_quarantine_entry(&namespace, ".b.corrupt-1-3").expect("file removed");
        assert!(!directory.exists() && !file.exists());
        drop(namespace);
        fs::remove_dir_all(parent).expect("cleanup");
    }
}
