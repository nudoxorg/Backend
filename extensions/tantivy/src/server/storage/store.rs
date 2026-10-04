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
    DirectoryPublication, MovedAside, NamespaceFence, NamespaceFenceKind, PrivateNamespace,
    StagePreparationFailure, TransientDenial, stage_preparation_cleanup_io_error,
    backend_is_transient_denial, failed_stage_preparation_with, io_is_transient_denial,
    retry_while_denied, STAGE_LEASE_FILE_PREFIX,
};
use backend_semantic::index_core::{
    IndexSnapshot, IndexSnapshotId, LexicalOperation, LexicalSegment, LexicalSegmentId,
};

const MAX_SEGMENT_STORE_NAMESPACE_ENTRIES: usize = 65_536;

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
    /// Existing valid content is reused without rebuilding. A corrupt or incomplete final
    /// directory is quarantined only after a complete replacement has been prepared. Temporary
    /// directories never use the final content-addressed name, so a pre-publish interruption
    /// cannot create durable authority. A fixed, bounded cooperative namespace fence covers
    /// admission, quarantine, publication, and reopen. The stage build runs outside that fence
    /// under its own active lease. A same-user process that bypasses the fence remains outside the
    /// guarantee.
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
        self.project_with_stage_hook(segment, || {})
    }

    fn project_with_stage_hook(
        &self,
        segment: LexicalSegment<'_>,
        stage_ready: impl FnMut(),
    ) -> Result<TantivySegment, TantivySegmentStoreError> {
        self.project_with_stage_hook_and_entry_limit(
            segment,
            stage_ready,
            MAX_SEGMENT_STORE_NAMESPACE_ENTRIES,
        )
    }

    fn project_with_stage_hook_and_entry_limit(
        &self,
        segment: LexicalSegment<'_>,
        mut stage_ready: impl FnMut(),
        maximum_entries: usize,
    ) -> Result<TantivySegment, TantivySegmentStoreError> {
        let namespace = publication::open_recipe_namespace(&self.root)
            .map_err(|source| io_error(StorePhase::CreateRoot, &self.root, source))?;
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .map_err(|source| io_error(StorePhase::CreateRoot, namespace.path(), source))?;
        namespace
            .verify_path()
            .map_err(|source| io_error(StorePhase::CreateRoot, namespace.path(), source))?;
        let final_name = publication::segment_name(segment.id);
        let final_dir = publication::segment_path(&self.root, segment.id);
        let mut quarantine = None;
        if path_is_real_directory(&final_dir)
            .map_err(|source| io_error(StorePhase::Reopen, &final_dir, source))?
        {
            match open_segment_verified(&namespace, segment.id, &fence) {
                Ok(opened) => {
                    fence
                        .verify_for(&namespace)
                        .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
                    return Ok(opened);
                }
                Err(error) if !error.rebuildable() => return Err(error),
                Err(_) => {}
            }
        }
        retry_while_denied(|| {
            sweep_abandoned_temp_stages(&namespace, &fence, maximum_entries)
        })
        .map_err(|source| io_error(StorePhase::CreateTemporary, namespace.path(), source))?;
        drop(fence);

        // A scanner or indexer that briefly holds a just-written file can make
        // the operating system refuse the build; a fresh stage is attempted
        // again before the failure is reported.
        let temp_stage = retry_while_denied(|| {
            let fence = namespace
                .acquire_fence(NamespaceFenceKind::SegmentStore)
                .map_err(|source| {
                    StagePreparationFailure::for_attempt(io_error(
                        StorePhase::CreateTemporary,
                        namespace.path(),
                        source,
                    ))
                })?;
            let stage = publication::new_temp_stage(&namespace, segment.id, &fence)
                .map_err(StagePreparationFailure::for_attempt)?;
            drop(fence);
            stage_ready();
            if let Err(source) = stage.verify_path() {
                return Err(failed_stage_preparation_with(
                    stage,
                    io_error(StorePhase::CreateTemporary, &final_dir, source),
                    |operation, cleanup| {
                        io_error(
                            StorePhase::CreateTemporary,
                            &final_dir,
                            stage_preparation_cleanup_io_error(operation, cleanup),
                        )
                    },
                ));
            }
            match publication::build_projection(stage.path(), segment) {
                Ok(()) => match stage.sync().and_then(|()| stage.verify_path()) {
                    Ok(()) => Ok(stage),
                    Err(source) => Err(failed_stage_preparation_with(
                        stage,
                        io_error(StorePhase::CreateTemporary, &final_dir, source),
                        |operation, cleanup| {
                            io_error(
                                StorePhase::CreateTemporary,
                                &final_dir,
                                stage_preparation_cleanup_io_error(operation, cleanup),
                            )
                        },
                    )),
                },
                Err(error) => Err(failed_stage_preparation_with(stage, error, |operation, cleanup| {
                    io_error(
                        StorePhase::CreateTemporary,
                        &final_dir,
                        stage_preparation_cleanup_io_error(operation, cleanup),
                    )
                })),
            }
        })
        .map_err(StagePreparationFailure::into_error)?;
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .map_err(|source| io_error(StorePhase::Publish, namespace.path(), source))?;
        if path_entry_exists(&final_dir)
            .map_err(|source| io_error(StorePhase::Reopen, &final_dir, source))?
        {
            if !path_is_real_directory(&final_dir)
                .map_err(|source| io_error(StorePhase::Reopen, &final_dir, source))?
            {
                quarantine = self.quarantine(&namespace, &final_name, segment.id, &fence)?;
            } else {
                match open_segment_verified(&namespace, segment.id, &fence) {
                    Ok(opened) => {
                        temp_stage.discard_under(&fence).map_err(|source| {
                            io_error(
                                StorePhase::CreateTemporary,
                                &final_dir,
                                source.into_io_error(),
                            )
                        })?;
                        return Ok(opened);
                    }
                    Err(error) if !error.rebuildable() => {
                        temp_stage.discard_under(&fence).map_err(|source| {
                            io_error(
                                StorePhase::CreateTemporary,
                                &final_dir,
                                source.into_io_error(),
                            )
                        })?;
                        return Err(error);
                    }
                    Err(_) => {}
                }
                quarantine = self.quarantine(&namespace, &final_name, segment.id, &fence)?;
            }
        }
        let publication = temp_stage
            .publish(&final_name, &fence)
            .map_err(|source| io_error(StorePhase::Publish, &final_dir, source.into_io_error()))?;
        match publication {
            DirectoryPublication::Published => finish_open(
                open_segment_verified(&namespace, segment.id, &fence),
                &namespace,
                &fence,
                quarantine,
            ),
            // Another publisher named this exact segment first; its directory
            // is validated like any other existing final directory.
            DirectoryPublication::AlreadyPresent => finish_open(
                open_segment_verified(&namespace, segment.id, &fence),
                &namespace,
                &fence,
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
        fence: &NamespaceFence,
    ) -> Result<Option<String>, TantivySegmentStoreError> {
        namespace
            .verify_path()
            .map_err(|source| io_error(StorePhase::Quarantine, namespace.path(), source))?;
        let name = publication::quarantine_name(id);
        match namespace.move_aside(entry, &name, fence) {
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
        namespace
            .verify_path()
            .map_err(|source| io_error(StorePhase::CreateRoot, namespace.path(), source))?;
        let name = publication::segment_name(id);
        match namespace.open_private_dir(&name) {
            Ok(directory) => drop(directory),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                fence
                    .verify_for(&namespace)
                    .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
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
        let opened = open_segment_verified(&namespace, id, &fence);
        fence
            .verify_for(&namespace)
            .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
        opened
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

/// Reclaims only stages emitted by `new_temp_stage`. Their marker is held by
/// the builder through the expensive write phase; all admission and deletion
/// stays beneath the already-held fixed segment-store fence.
fn sweep_abandoned_temp_stages(
    namespace: &PrivateNamespace,
    fence: &NamespaceFence,
    maximum_entries: usize,
) -> io::Result<()> {
    let entries = namespace.entries(maximum_entries, fence)?;
    for entry in &entries {
        if let Some(name) = entry.name.to_str()
            && is_segment_store_stage_name(name)
        {
            let _ = namespace.sweep_stage(name, fence)?;
        }
    }
    for entry in &entries {
        if let Some(lease_name) = entry.name.to_str()
            && let Some(stage_name) = lease_name.strip_prefix(STAGE_LEASE_FILE_PREFIX)
            && is_segment_store_stage_name(stage_name)
        {
            let _ = namespace.sweep_orphan_stage_lease(lease_name, stage_name, fence)?;
        }
    }
    fence.verify_for(namespace)
}

fn is_segment_store_stage_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix('.') else {
        return false;
    };
    let Some((segment, owner)) = suffix.split_once(".tmp-") else {
        return false;
    };
    let Some((process, stage)) = owner.split_once('-') else {
        return false;
    };
    segment.len() == 32
        && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !process.is_empty()
        && process.bytes().all(|byte| byte.is_ascii_digit())
        && !stage.is_empty()
        && stage.bytes().all(|byte| byte.is_ascii_digit())
}

fn open_segment_verified(
    namespace: &PrivateNamespace,
    id: LexicalSegmentId,
    fence: &NamespaceFence,
) -> Result<TantivySegment, TantivySegmentStoreError> {
    fence
        .verify_for(namespace)
        .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
    let name = publication::segment_name(id);
    let path = namespace.path().join(&name);
    namespace
        .verify_child_path(&name)
        .map_err(|source| io_error(StorePhase::Reopen, &path, source))?;
    fence
        .verify_for(namespace)
        .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
    let opened = publication::open_segment(&path, id);
    namespace
        .verify_child_path(&name)
        .map_err(|source| io_error(StorePhase::Reopen, &path, source))?;
    opened
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
    fence: &NamespaceFence,
) -> Result<(), TantivySegmentStoreError> {
    let path = namespace.path().join(name);
    retry_while_denied(|| {
        namespace
            .remove_entry(name, fence)
            .map_err(|source| io_error(StorePhase::Quarantine, &path, source))
    })
}

fn finish_open(
    result: Result<TantivySegment, TantivySegmentStoreError>,
    namespace: &PrivateNamespace,
    fence: &NamespaceFence,
    quarantine: Option<String>,
) -> Result<TantivySegment, TantivySegmentStoreError> {
    fence
        .verify_for(namespace)
        .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
    if result.is_ok()
        && let Some(name) = quarantine
    {
        remove_quarantine_entry(namespace, &name, fence)?;
    }
    fence
        .verify_for(namespace)
        .map_err(|source| io_error(StorePhase::Reopen, namespace.path(), source))?;
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
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .expect("store gate");
        // The entry was removed by someone else first: the goal is met.
        remove_quarantine_entry(&namespace, ".segment.corrupt-1-1", &fence)
            .expect("already gone");
        drop(fence);
        drop(namespace);
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn a_quarantined_directory_and_a_quarantined_file_are_both_removed() {
        let parent = scratch("both");
        let namespace =
            PrivateNamespace::open_child(&parent, "private-v1").expect("private namespace");
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .expect("store gate");
        let directory = namespace.path().join(".a.corrupt-1-2");
        fs::create_dir_all(directory.join("tantivy")).expect("quarantined directory");
        fs::write(directory.join("tantivy").join("meta.json"), b"{}").expect("content");
        let file = namespace.path().join(".b.corrupt-1-3");
        fs::write(&file, b"stray").expect("quarantined file");

        remove_quarantine_entry(&namespace, ".a.corrupt-1-2", &fence)
            .expect("directory removed");
        remove_quarantine_entry(&namespace, ".b.corrupt-1-3", &fence)
            .expect("file removed");
        assert!(!directory.exists() && !file.exists());
        drop(fence);
        drop(namespace);
        fs::remove_dir_all(parent).expect("cleanup");
    }
}

#[cfg(test)]
mod namespace_fence_tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use backend_semantic::index_core::{
        EntityArtifactIdentity, EntityDocumentId, LexicalRow, LexicalScore, LexicalSegment,
    };
    use backend_semantic::ir::EntityId;
    use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    fn scratch(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nudox-tantivy-fence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ))
    }

    fn segment(term: &'static [u8], entity: u32) -> LexicalSegment<'static> {
        let artifact = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"tantivy-fence-test",
        );
        let rows = Box::leak(Box::new([LexicalRow::new(
            term,
            EntityDocumentId {
                artifact: EntityArtifactIdentity::Compact(artifact),
                entity: EntityId::new(entity),
            },
            LexicalScore::from(1),
        )]));
        LexicalSegment::new(rows).expect("valid test segment")
    }

    fn overlap_projects(segments: [LexicalSegment<'static>; 2], label: &str) {
        let root = scratch(label);
        let store = TantivySegmentStore::open(&root).expect("store");
        let (ready_tx, ready_rx) = mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        for segment in segments {
            let store = store.clone();
            let ready_tx = ready_tx.clone();
            let release = Arc::clone(&release);
            workers.push(std::thread::spawn(move || {
                store.project_with_stage_hook(segment, || {
                    ready_tx.send(()).expect("report prepared stage");
                    while !release.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                })
            }));
        }
        drop(ready_tx);
        let first_ready = ready_rx.recv_timeout(Duration::from_secs(3)).is_ok();
        let both_ready = first_ready && ready_rx.recv_timeout(Duration::from_secs(3)).is_ok();
        release.store(true, Ordering::Release);
        for worker in workers {
            assert!(worker.join().expect("projection worker").is_ok());
        }
        assert!(
            both_ready,
            "both builders must reach their stage outside the gate"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn independent_segment_builds_overlap_outside_the_namespace_fence() {
        let first = segment(b"first", 1);
        let second = segment(b"second", 2);
        assert_ne!(first.id, second.id);
        overlap_projects([first, second], "independent");
    }

    #[test]
    fn same_segment_builders_race_at_one_final_winner_admission() {
        let same = segment(b"same", 3);
        overlap_projects([same, same], "same-segment-winner");
    }

    #[test]
    fn warm_valid_reuse_bypasses_over_limit_stage_maintenance() {
        let root = scratch("warm-reuse-before-stage-maintenance");
        let store = TantivySegmentStore::open(&root).expect("store");
        let warm_segment = segment(b"warm", 7);
        drop(store.project(warm_segment).expect("seed valid projection"));

        let namespace = publication::open_recipe_namespace(&root).expect("recipe namespace");
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .expect("store gate");
        let abandoned_segment = segment(b"abandoned", 8);
        let abandoned = publication::new_temp_stage(&namespace, abandoned_segment.id, &fence)
            .expect("stage before simulated owner exit");
        let abandoned_path = abandoned.path().to_path_buf();
        let abandoned_marker = namespace
            .path()
            .join(crate::publish::stage_lease_file_name(abandoned.name()));
        drop(fence);
        drop(abandoned);

        // A tiny injected maintenance ceiling produces the same bounded
        // FileTooLarge refusal as a namespace with more than 65,536 entries.
        // Warm authority must not depend on stale-stage housekeeping.
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .expect("store gate for capacity witness");
        let capacity = namespace
            .entries(1, &fence)
            .expect_err("namespace contains more than one direct child");
        assert_eq!(capacity.kind(), std::io::ErrorKind::FileTooLarge);
        drop(fence);

        let reopened = store
            .project_with_stage_hook_and_entry_limit(warm_segment, || {}, 1)
            .expect("valid warm projection is admitted despite maintenance capacity");
        assert_eq!(reopened.id(), warm_segment.id);
        assert!(abandoned_path.is_dir(), "warm reuse must not run the sweep");
        assert!(abandoned_marker.is_file(), "maintenance preserves its lease record");

        drop(reopened);
        drop(namespace);
        drop(store);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn store_sweep_skips_live_stages_and_ignores_unowned_names() {
        let root = scratch("store-stage-sweep");
        let store = TantivySegmentStore::open(&root).expect("store");
        let namespace = publication::open_recipe_namespace(&root).expect("recipe namespace");
        let stale_identity = segment(b"live-stage", 4);
        let fence = namespace
            .acquire_fence(NamespaceFenceKind::SegmentStore)
            .expect("store gate");
        let active_stage = publication::new_temp_stage(&namespace, stale_identity.id, &fence)
            .expect("active stage");
        let active_path = active_stage.path().to_path_buf();
        let active_marker = namespace
            .path()
            .join(crate::publish::stage_lease_file_name(active_stage.name()));
        drop(fence);

        let unrelated = namespace.path().join(".operator-owned-scratch");
        fs::create_dir(&unrelated).expect("unrelated directory");
        fs::write(unrelated.join("keep"), b"not our stage grammar").expect("sentinel");
        assert!(!is_segment_store_stage_name(".operator-owned-scratch"));

        let first = segment(b"first-project", 5);
        drop(
            store
                .project(first)
                .expect("project while other stage is live"),
        );
        assert!(
            active_path.is_dir(),
            "live stage must survive admission sweep"
        );
        assert!(active_marker.is_file(), "live marker must remain named");

        drop(active_stage);
        let second = segment(b"second-project", 6);
        drop(
            store
                .project(second)
                .expect("project and sweep abandoned stage"),
        );
        assert!(!active_path.exists(), "unlocked stale stage is reclaimed");
        assert!(
            !active_marker.exists(),
            "its exact lease marker is reclaimed"
        );
        assert_eq!(
            fs::read(unrelated.join("keep")).expect("unrelated survives"),
            b"not our stage grammar"
        );
        drop(namespace);
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
}
