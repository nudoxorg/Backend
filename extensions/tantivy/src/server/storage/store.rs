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
        let final_dir = publication::segment_path(&self.root, segment.id);
        let mut quarantine = None;
        if final_dir.is_dir() {
            match publication::open_segment(&final_dir, segment.id) {
                Ok(opened) => return Ok(opened),
                Err(error) if !error.rebuildable() => return Err(error),
                Err(_) => {}
            }
        }

        let temp_dir = publication::new_temp_dir(&self.root, segment.id)?;
        let build_result = publication::build_projection(&temp_dir, segment);
        if let Err(error) = build_result {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
        if final_dir.exists() {
            if !final_dir.is_dir() {
                let path = publication::quarantine_path(&self.root, segment.id);
                match fs::rename(&final_dir, &path) {
                    Ok(()) => quarantine = Some(path),
                    Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => {
                        let _ = fs::remove_dir_all(&temp_dir);
                        return Err(io_error(StorePhase::Quarantine, &final_dir, source));
                    }
                }
            } else {
                match publication::open_segment(&final_dir, segment.id) {
                    Ok(opened) => {
                        let _ = fs::remove_dir_all(&temp_dir);
                        return Ok(opened);
                    }
                    Err(error) if !error.rebuildable() => {
                        let _ = fs::remove_dir_all(&temp_dir);
                        return Err(error);
                    }
                    Err(_) => {}
                }
                let path = publication::quarantine_path(&self.root, segment.id);
                if let Err(source) = fs::rename(&final_dir, &path) {
                    if source.kind() != io::ErrorKind::NotFound {
                        let _ = fs::remove_dir_all(&temp_dir);
                        return Err(io_error(StorePhase::Quarantine, &final_dir, source));
                    }
                } else {
                    quarantine = Some(path);
                }
            }
        }
        match fs::rename(&temp_dir, &final_dir) {
            Ok(()) => {
                publication::sync_recipe_root(&self.root)
                    .map_err(|source| io_error(StorePhase::Publish, &self.root, source))?;
                finish_open(
                    publication::open_segment(&final_dir, segment.id),
                    quarantine,
                )
            }
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                let _ = fs::remove_dir_all(&temp_dir);
                finish_open(
                    publication::open_segment(&final_dir, segment.id),
                    quarantine,
                )
            }
            Err(source) => {
                let _ = fs::remove_dir_all(&temp_dir);
                Err(io_error(StorePhase::Publish, &final_dir, source))
            }
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
        let path = publication::segment_path(&self.root, id);
        if !path.exists() {
            return Err(TantivySegmentStoreError::Missing { id });
        }
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

fn remove_quarantine_path(path: &Path) -> Result<(), TantivySegmentStoreError> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|source| io_error(StorePhase::Quarantine, path, source))
}

fn finish_open(
    result: Result<TantivySegment, TantivySegmentStoreError>,
    quarantine: Option<PathBuf>,
) -> Result<TantivySegment, TantivySegmentStoreError> {
    if result.is_ok()
        && let Some(path) = quarantine
    {
        remove_quarantine_path(&path)?;
    }
    result
}
