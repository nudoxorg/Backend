//! Execution: the shared driver, wired to a connected local session.
//!
//! Every decision about *which* round trips one command needs — a page is a
//! document plus the rows around it, an outline is a tree of keys plus the rows
//! that name them, a probe that fails becomes a note rather than a failure —
//! lives in [`backend_present::answer`], because the MCP surface must make the
//! same ones. The CLI's optional owner replacement policy only repeats
//! unpinned reads or reconciles a journaled start under its original key.

use backend_client::{ClientError, Session};
use backend_library::{
    HealthReport, PageContinuation, ReplyDto, SurfaceCommand, SurfaceReply, ViewStateRoot,
    encode_id,
};
use backend_present::{Engine, Fault, Probe, Request};

pub use backend_present::Answer;

/// One connected local daemon session, seen as the shared driver's engine.
pub struct SessionEngine<'a> {
    session: &'a mut Session,
    paths: Option<&'a backend_runtime::WorkspacePaths>,
    recovery: Option<&'a mut crate::owner_recovery::OwnerRecovery>,
}

impl<'a> SessionEngine<'a> {
    /// Borrows one connected session as an engine.
    pub const fn new(session: &'a mut Session) -> Self {
        Self {
            session,
            paths: None,
            recovery: None,
        }
    }
}

impl Engine for SessionEngine<'_> {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.session.revision().map(|revision| revision.root)
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        match &mut self.recovery {
            Some(recovery) => recovery.read(
                self.session,
                crate::owner_recovery::RecoverableRead::Health,
                Session::health,
            ),
            None => self.session.health(),
        }
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Packages => self.session.packages(),
            Probe::Index(path) => self.session.index(path),
            Probe::IndexWithExecutionIntent {
                path,
                execution_intent,
            } => self
                .session
                .index_with_execution_intent(path, execution_intent),
            Probe::Remove(path) => self.session.remove(path).map_err(nonrepeatable),
            Probe::Document(at) => self.session.document(at),
            Probe::Source(at) => self.session.source(at),
            Probe::Related(at) => self.session.related(at),
            Probe::Graph(at) => self.session.graph(at),
            Probe::Search { text, limit } => match &mut self.recovery {
                Some(recovery) => recovery.read(
                    self.session,
                    crate::owner_recovery::RecoverableRead::UnselectedSearch,
                    |session| session.search(text, limit),
                ),
                None => self.session.search(text, limit),
            },
            Probe::Names { text, limit } => self.session.names(text, limit),
            Probe::Outline(path) => self.session.outline(path),
            Probe::OutlinePage { path, limit } => self.session.outline_page(path, limit, None),
        }
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Search { text, limit } if continuation.is_none() => match &mut self.recovery {
                Some(recovery) => recovery.read(
                    self.session,
                    crate::owner_recovery::RecoverableRead::UnselectedSearch,
                    |session| session.search_page(text, limit, None),
                ),
                None => self.session.search_page(text, limit, None),
            },
            Probe::Search { text, limit } => self.session.search_page(text, limit, continuation),
            Probe::Names { text, limit } => self.session.names_page(text, limit, continuation),
            Probe::OutlinePage { path, limit } => {
                self.session.outline_page(path, limit, continuation)
            }
            _ if continuation.is_none() => self.probe(probe),
            _ => Err(ClientError::Protocol(
                "this CLI command does not support continuation pages".to_owned(),
            )),
        }
    }

    fn prepare_query_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        self.session.decode_page_continuation(token)
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let session = &mut *self.session;
        let recovery = &mut self.recovery;
        match self.paths {
            Some(paths) => {
                backend_client::surface_with_index_journal(paths, command, |command| match recovery
                    .as_deref_mut()
                {
                    Some(recovery) => recovery.surface(session, command, None),
                    None => session.surface(command),
                })
            }
            None => session.surface(command),
        }
    }

    fn index(
        &mut self,
        package: backend_library::PackageReference,
        execution_intent: backend_library::CompileExecutionIntent,
    ) -> Result<SurfaceReply, ClientError> {
        let session = &mut *self.session;
        let recovery = &mut self.recovery;
        match self.paths {
            Some(paths) => backend_client::index_with_journal_recovery(
                paths,
                package,
                execution_intent,
                |dispatch| match recovery.as_deref_mut() {
                    Some(recovery) => recovery.journal(session, dispatch, None),
                    None => session.surface(dispatch.command()),
                },
            ),
            None => session.surface(SurfaceCommand::IndexStart {
                package,
                execution_intent,
            }),
        }
    }
}

/// Executes one typed request against a connected session.
///
/// # Errors
///
/// Returns the typed fault the engine or the transport produced, with the
/// operand the caller supplied already attached.
pub fn execute(session: &mut Session, request: &Request) -> Result<Answer, Fault> {
    backend_present::answer(&mut SessionEngine::new(session), request)
}

/// Executes against the frozen runtime attachment, retaining default index
/// claims across command exits, interrupted delivery and owner restarts.
///
/// # Errors
/// Returns the typed owner, journal or transport fault.
pub fn execute_in_workspace(
    session: &mut Session,
    request: &Request,
    paths: &backend_runtime::WorkspacePaths,
) -> Result<Answer, Fault> {
    backend_present::answer(
        &mut SessionEngine {
            session,
            paths: Some(paths),
            recovery: None,
        },
        request,
    )
}

pub(crate) fn execute_with_recovery(
    session: &mut Session,
    request: &Request,
    paths: &backend_runtime::WorkspacePaths,
    recovery: &mut crate::owner_recovery::OwnerRecovery,
) -> Result<Answer, Fault> {
    backend_present::answer(
        &mut SessionEngine {
            session,
            paths: Some(paths),
            recovery: Some(recovery),
        },
        request,
    )
}

fn nonrepeatable(error: ClientError) -> ClientError {
    match error {
        ClientError::Disconnected(kind) => ClientError::MutationOutcomeUnknown(kind),
        error => error,
    }
}

/// Returns the exact revision an answer was read at, for a caption.
#[must_use]
pub fn revision_tag(session: &mut Session) -> Option<String> {
    session
        .revision()
        .ok()
        .map(|revision| encode_id(revision.root.as_bytes()))
}
