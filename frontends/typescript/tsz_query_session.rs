//! Borrowed, project-scoped TSZ query session for native IR projection.
//!
//! A session is created from one fully checked [`TszProject`] and borrows its
//! exact merged program, checker options, resolved library closure, and
//! exact project context. Production sessions borrow the execution checkpoint
//! admitted by the most recent successful update. A changed update shares its
//! budget through parsing, binding, merging, checking, and queries. An exact
//! cache update may admit a new query checkpoint while retaining the fully
//! checked image without repeating those stages. The vendored TSZ session owns
//! the project-wide
//! binders, arenas, resolution outcomes, symbol-to-file map, library contexts,
//! and shared query cache.
//! Each file checker is created on the stack from that shared context and is
//! lent only for the duration of a higher-ranked callback. The callback should
//! finish projecting its facts before it returns.
//!
//! `TszTypeId` is a copyable value, so the callback signature cannot prevent a
//! caller from copying one out. Callers must keep such IDs local to the
//! callback and return owned compiler facts rather than TSZ handles.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{TszAuthorityError, TszFileWorkExhaustion, TszProject};
use backend_semantic::ir::{NativeProgramSourceManifest, typescript_program_identity};
use backend_version::{ContentId, SourceFactDomain};
use tsz::tsz_solver::construction::TypeDatabase;
use tsz_common::ExecutionCheckpoint;

/// Typed failure to open a native query session for a project.
pub type TszProjectQuerySessionError = tsz::parallel::ProjectCheckerSessionError;

/// One query session tied to the exact lifetime of a checked project.
///
/// Keep one session for the package-lowering loop and borrow it for each
/// source file. It retains project query caches and resolution context,
/// but no file checker survives an individual callback.
pub struct TszProjectQuerySession<'project> {
    project: &'project TszProject,
    checker_session: tsz::parallel::ProjectCheckerSession<'project>,
    /// The exact checkpoint already authenticated by this session's constructor.
    /// Retained only to forward boundary observations; never a fresh budget.
    checkpoint: &'project dyn ExecutionCheckpoint,
    file_indexes: HashMap<&'project str, usize>,
    native_sources: NativeSourceIdentities,
    /// The first file query that spent its per-file work allowance.
    file_work_exhausted: Mutex<Option<TszFileWorkExhaustion>>,
}

#[derive(Clone, Copy)]
struct NativeSourceIdentity {
    digest: [u8; 32],
    byte_len: usize,
}

struct NativeSourceIdentities {
    program: Option<[u8; 32]>,
    files: Vec<Option<NativeSourceIdentity>>,
    #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
    hash_work: (usize, u64),
}

impl NativeSourceIdentities {
    fn capture(
        project: &TszProject,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<Self, TszProjectQuerySessionError> {
        let mut files = Vec::with_capacity(project.program().files.len());
        let mut manifest = Vec::with_capacity(project.program().files.len());
        let mut complete = true;
        #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
        let mut hash_work = (0, 0_u64);
        for file in &project.program().files {
            checkpoint
                .checkpoint(0)
                .map_err(TszProjectQuerySessionError::ExecutionStopped)?;
            let source = file.arena.get_source_file_at(file.source_file);
            let identity = source.map(|source| {
                #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
                {
                    hash_work.0 += 1;
                    hash_work.1 = hash_work.1.saturating_add(source.text.len() as u64);
                }
                NativeSourceIdentity {
                    digest: *ContentId::<SourceFactDomain>::from_canonical_bytes(
                        source.text.as_bytes(),
                    ),
                    byte_len: source.text.len(),
                }
            });
            if let Some(identity) = identity {
                manifest.push((file.file_name.clone(), identity.digest));
            } else {
                complete = false;
            }
            files.push(identity);
            checkpoint
                .checkpoint(0)
                .map_err(TszProjectQuerySessionError::ExecutionStopped)?;
        }
        let program = complete
            .then(|| typescript_program_identity(&manifest))
            .flatten();
        checkpoint
            .checkpoint(0)
            .map_err(TszProjectQuerySessionError::ExecutionStopped)?;
        Ok(Self {
            program,
            files,
            #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
            hash_work,
        })
    }
}

impl std::fmt::Debug for TszProjectQuerySession<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TszProjectQuerySession")
            .field("file_count", &self.file_indexes.len())
            .finish_non_exhaustive()
    }
}

impl TszProject {
    /// Opens a budgeted native query session for exact semantic projection.
    ///
    /// Use the checkpoint admitted by the most recent successful update.
    /// An exact update may reuse the fully checked image under a new query
    /// scope; a changed update shares its budget through checking and queries.
    /// The session retains that admitted scope and exact shared context, while
    /// each file checker remains stack-local to its callback.
    ///
    /// # Errors
    /// Returns a typed failure when the project lacks an exact resolution
    /// witness or the shared execution checkpoint has stopped.
    pub fn checked_query_session<'project>(
        &'project self,
        checkpoint: &'project dyn ExecutionCheckpoint,
    ) -> Result<TszProjectQuerySession<'project>, TszProjectQuerySessionError> {
        TszProjectQuerySession::new(self, checkpoint, None)
    }

    /// Like [`Self::checked_query_session`], but every file query is bounded by
    /// its own work allowance of `allowance` units.
    ///
    /// A query that spends its allowance fails with
    /// [`TszProjectQuerySessionError::FileWorkExhausted`], drops its partial
    /// output, and is recorded for [`TszProjectQuerySession::take_file_work_exhaustion`].
    /// The interrupted query may have written partial state into the checked
    /// image's program-wide tables, so after that error neither the session nor
    /// the project may serve further queries: rebuild the project first.
    ///
    /// # Errors
    /// As [`Self::checked_query_session`].
    pub fn checked_query_session_with_file_work<'project>(
        &'project self,
        checkpoint: &'project dyn ExecutionCheckpoint,
        allowance: u64,
    ) -> Result<TszProjectQuerySession<'project>, TszProjectQuerySessionError> {
        TszProjectQuerySession::new(self, checkpoint, Some(allowance))
    }

    /// Opens an unmetered native query session for semantic regression tests.
    ///
    /// The project must carry an explicit compiler-owned module-resolution
    /// outcome map. TSZ refuses to open a session when that witness is absent,
    /// rather than falling back to guessed filename resolution.
    ///
    /// This method is gated by `tsz-semantic-session-test-support` and is not
    /// valid for production compiler use. Production uses
    /// [`Self::checked_query_session`] with the project's shared checkpoint.
    ///
    /// # Errors
    /// Returns [`TszProjectQuerySessionError::MissingProjectModuleResolutions`]
    /// when the merged program has no explicit project-resolution witness.
    #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
    pub fn checked_query_session_unmetered_for_test(
        &self,
    ) -> Result<TszProjectQuerySession<'_>, TszProjectQuerySessionError> {
        TszProjectQuerySession::new(self, &UNMETERED_TEST_CHECKPOINT, None)
    }
}

#[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
struct UnmeteredTestCheckpoint;

#[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
impl ExecutionCheckpoint for UnmeteredTestCheckpoint {
    fn checkpoint(&self, _work_units: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
        Ok(())
    }
}

#[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
static UNMETERED_TEST_CHECKPOINT: UnmeteredTestCheckpoint = UnmeteredTestCheckpoint;

impl<'project> TszProjectQuerySession<'project> {
    fn new(
        project: &'project TszProject,
        checkpoint: &'project dyn ExecutionCheckpoint,
        file_work_allowance: Option<u64>,
    ) -> Result<Self, TszProjectQuerySessionError> {
        // Only the query scope admitted by the most recent successful update
        // may enter this request. A later exact update can intentionally admit
        // a new scope while retaining the same fully checked immutable image.
        if project.program.is_usable()
            && project.query_budget.as_ref().is_some_and(|identity| {
                checkpoint.execution_identity().as_ref() != Some(identity)
            })
        {
            return Err(TszProjectQuerySessionError::ExecutionCheckpointMismatch);
        }
        let mut checker_session = project.program.query_session(checkpoint)?;
        if let Some(allowance) = file_work_allowance {
            checker_session = checker_session.with_file_work_allowance(allowance);
        }
        let file_indexes = project
            .program
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.file_name.as_str(), index))
            .collect();
        let native_sources = NativeSourceIdentities::capture(project, checkpoint)?;
        Ok(Self {
            project,
            checker_session,
            checkpoint,
            file_indexes,
            native_sources,
            file_work_exhausted: Mutex::new(None),
        })
    }

    /// Forwards a projection boundary to this session's already-admitted
    /// checkpoint. This observes only: it never polls, charges work, enters the
    /// compiler, or exposes semantic authority. The native query frame owns the
    /// exact file context for projection subpasses on this same worker.
    pub fn observe_projection(&self, event: tsz_common::ProjectExecutionEvent<'_>) {
        self.checkpoint.observe(event);
    }

    /// Borrows the exact checked project backing this session.
    #[must_use]
    pub const fn project(&self) -> &'project TszProject {
        self.project
    }

    /// Source-manifest identity captured once from this immutable native program.
    /// This uses the existing TSZ source-coordinate codec, never a forest ID.
    #[must_use]
    pub const fn native_program_identity(&self) -> Option<[u8; 32]> {
        self.native_sources.program
    }

    /// Exact content identity only when the supplied compile bytes match the
    /// immutable native source at this merged-program index. Callers must
    /// still authenticate this session's project before publishing facts.
    #[must_use]
    pub fn native_source_identity(&self, file_index: usize, bytes: &[u8]) -> Option<[u8; 32]> {
        let file = self.project.program().files.get(file_index)?;
        let source = file.arena.get_source_file_at(file.source_file)?;
        if source.text.as_bytes() != bytes {
            return None;
        }
        self.native_sources
            .files
            .get(file_index)
            .copied()
            .flatten()
            .map(|source| source.digest)
    }

    /// Returns the cached identity for this exact immutable program file.
    ///
    /// Native declaration coordinates already borrow their target directly from
    /// this session's checked program. Pointer identity authenticates that owner
    /// without rehashing or comparing its full source text for every occurrence.
    /// A file cloned from another program, even with identical text and path,
    /// cannot borrow this session's source identity.
    #[must_use]
    pub fn native_source_identity_for_bound_file(
        &self,
        file_index: usize,
        file: &tsz::parallel::BoundFile,
    ) -> Option<[u8; 32]> {
        let expected = self.project.program().files.get(file_index)?;
        if !std::ptr::eq(expected, file) {
            return None;
        }
        self.native_sources
            .files
            .get(file_index)
            .copied()
            .flatten()
            .map(|source| source.digest)
    }

    /// Authenticates and adopts the compiler's already captured leaf manifest.
    /// Every native path, content digest and byte length must match this
    /// session; a forest envelope or another revision cannot supply its ID.
    /// This checks source membership; toolchain/configuration admission remains
    /// with the compiler owner.
    ///
    /// # Errors
    /// Returns an exact source refusal or a project-session mismatch.
    pub fn authenticate_native_program_sources(
        mut self,
        manifest: &NativeProgramSourceManifest,
    ) -> Result<Self, TszAuthorityError> {
        if manifest.sources().len() != self.native_sources.files.len()
            || self.native_sources.program != Some(manifest.program())
        {
            return Err(TszAuthorityError::ProjectSessionMismatch);
        }
        for row in manifest.sources() {
            let index = self.file_index(row.program_path)?;
            let Some(Some(source)) = self.native_sources.files.get(index) else {
                return Err(TszAuthorityError::MissingFile(index));
            };
            if source.digest != *row.source.identity
                || source.byte_len != row.source.byte_len as usize
            {
                return Err(TszAuthorityError::SourceMismatch {
                    path: row.program_path.to_owned(),
                });
            }
        }
        self.native_sources.program = Some(manifest.program());
        Ok(self)
    }

    /// Actual source hashing calls and bytes spent constructing this session's
    /// identity cache. Available only to native semantic regression fixtures.
    #[cfg(any(test, feature = "tsz-semantic-session-test-support"))]
    #[must_use]
    pub const fn native_identity_hash_work_for_test(&self) -> (usize, u64) {
        self.native_sources.hash_work
    }

    /// Opens one file-local checker from the shared project context and lends
    /// its native types only for the callback's duration.
    ///
    /// This query path does not repeat `check_source_file`. It creates a
    /// stack-local checker from the retained project's exact binders, arenas,
    /// library closure, semantic options, and compiler-owned module-resolution
    /// outcomes, then computes lazy declaration and flow-sensitive queries in
    /// that context. The project diagnostics are not treated as proof that a
    /// lazy query has already been computed.
    ///
    /// `TszTypeId` values are copyable and may be returned by value, so callers
    /// must not retain them beyond this callback. Native lowering should emit
    /// only owned `FactSet` rows inside the callback.
    ///
    /// # Errors
    /// Returns a typed checker-session error for an invalid merged file index.
    pub fn with_file_checker_and_types<Output>(
        &self,
        file_index: usize,
        consume: impl for<'checker> FnOnce(
            &mut super::TszCheckerState<'checker>,
            &super::TszBinderState,
            &tsz::parallel::BoundFile,
            &dyn TypeDatabase,
        ) -> Output,
    ) -> Result<Output, TszProjectQuerySessionError> {
        let result = self
            .checker_session
            .with_file_checker_and_types(file_index, consume);
        if let Err(TszProjectQuerySessionError::FileWorkExhausted {
            file_index,
            allowance,
        }) = &result
        {
            let path = self
                .project
                .program
                .files
                .get(*file_index)
                .map_or_else(String::new, |file| file.file_name.clone());
            let mut recorded = self
                .file_work_exhausted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recorded.get_or_insert(TszFileWorkExhaustion {
                file_index: *file_index,
                path,
                allowance: *allowance,
            });
        }
        result
    }

    /// Takes the first file query that spent its per-file work allowance, if any.
    ///
    /// This is the only way to tell a file-local exhaustion from a program-wide
    /// stop when a lowering step swallowed or re-labelled the query error.
    #[must_use]
    pub fn take_file_work_exhaustion(&self) -> Option<TszFileWorkExhaustion> {
        self.file_work_exhausted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Resolves an exact path to its merged program file index.
    ///
    /// This method uses the path-to-index map captured once from the exact
    /// merged program. It does not consult the filesystem or guess extensions.
    ///
    /// # Errors
    /// Returns a typed authority error if the path is absent from the project.
    pub fn file_index(&self, source_path: &str) -> Result<usize, TszAuthorityError> {
        self.file_indexes
            .get(source_path)
            .copied()
            .ok_or_else(|| TszAuthorityError::MissingSource(source_path.to_owned()))
    }
}
