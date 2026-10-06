//! Borrowed, project-scoped TSZ query session for native IR projection.
//!
//! A session is created from one fully checked [`TszProject`] and borrows its
//! exact merged program, checker options, resolved library closure, and
//! exact project context. Production sessions borrow the execution
//! checkpoint that governed parsing, binding, merging, and checking. The
//! vendored TSZ session owns the project-wide
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

use super::{TszAuthorityError, TszProject};
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
    file_indexes: HashMap<&'project str, usize>,
}

impl TszProject {
    /// Opens a budgeted native query session for exact semantic projection.
    ///
    /// The same checkpoint must have governed this project's update. The
    /// returned session retains the exact checked project and shared query
    /// context; each file checker remains stack-local to its callback.
    ///
    /// # Errors
    /// Returns a typed failure when the project lacks an exact resolution
    /// witness or the shared execution checkpoint has stopped.
    pub fn checked_query_session<'project>(
        &'project self,
        checkpoint: &'project dyn ExecutionCheckpoint,
    ) -> Result<TszProjectQuerySession<'project>, TszProjectQuerySessionError> {
        TszProjectQuerySession::new(self, checkpoint)
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
    #[cfg(feature = "tsz-semantic-session-test-support")]
    pub fn checked_query_session_unmetered_for_test(
        &self,
    ) -> Result<TszProjectQuerySession<'_>, TszProjectQuerySessionError> {
        TszProjectQuerySession::new(self, &UNMETERED_TEST_CHECKPOINT)
    }
}

#[cfg(feature = "tsz-semantic-session-test-support")]
struct UnmeteredTestCheckpoint;

#[cfg(feature = "tsz-semantic-session-test-support")]
impl ExecutionCheckpoint for UnmeteredTestCheckpoint {
    fn checkpoint(&self, _work_units: u64) -> Result<(), tsz_common::ProjectExecutionStop> {
        Ok(())
    }
}

#[cfg(feature = "tsz-semantic-session-test-support")]
static UNMETERED_TEST_CHECKPOINT: UnmeteredTestCheckpoint = UnmeteredTestCheckpoint;

impl<'project> TszProjectQuerySession<'project> {
    fn new(
        project: &'project TszProject,
        checkpoint: &'project dyn ExecutionCheckpoint,
    ) -> Result<Self, TszProjectQuerySessionError> {
        let checker_session = tsz::parallel::ProjectCheckerSession::new(
            &project.program,
            &project.options.checker,
            &project.lib_files,
            project.options.semantic_options,
            checkpoint,
        )?;
        let file_indexes = project
            .program
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.file_name.as_str(), index))
            .collect();
        Ok(Self {
            project,
            checker_session,
            file_indexes,
        })
    }

    /// Borrows the exact checked project backing this session.
    #[must_use]
    pub const fn project(&self) -> &'project TszProject {
        self.project
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
        self.checker_session
            .with_file_checker_and_types(file_index, consume)
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
