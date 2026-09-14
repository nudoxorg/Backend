//! Execution: the shared driver, wired to a connected local session.
//!
//! Every decision about *which* round trips one command needs — a page is a
//! document plus the rows around it, an outline is a tree of keys plus the rows
//! that name them, a probe that fails becomes a note rather than a failure —
//! lives in [`backend_present::answer`], because the MCP surface must make the
//! same ones. What is left here is the mechanical half: forwarding one
//! [`Probe`] to the matching `Session` method. There is no branching in it, so
//! there is nothing for the two surfaces to disagree about.

use backend_client::{ClientError, Session};
use backend_library::{
    HealthReport, ReplyDto, SurfaceCommand, SurfaceReply, ViewStateRoot, encode_id,
};
use backend_present::{Engine, Fault, Probe, Request};

pub use backend_present::Answer;

/// One connected local daemon session, seen as the shared driver's engine.
pub struct SessionEngine<'a>(&'a mut Session);

impl<'a> SessionEngine<'a> {
    /// Borrows one connected session as an engine.
    pub const fn new(session: &'a mut Session) -> Self {
        Self(session)
    }
}

impl Engine for SessionEngine<'_> {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.0.revision().map(|revision| revision.root)
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        self.0.health()
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Packages => self.0.packages(),
            Probe::Index(path) => self.0.index(path),
            Probe::Remove(path) => self.0.remove(path),
            Probe::Document(at) => self.0.document(at),
            Probe::Source(at) => self.0.source(at),
            Probe::Related(at) => self.0.related(at),
            Probe::Graph(at) => self.0.graph(at),
            Probe::Search { text, limit } => self.0.search(text, limit),
            Probe::Names { text, limit } => self.0.names(text, limit),
            Probe::Outline(path) => self.0.outline(path),
            Probe::OutlinePage { path, limit } => self.0.outline_page(path, limit, None),
        }
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        self.0.surface(command)
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

/// Returns the exact revision an answer was read at, for a caption.
#[must_use]
pub fn revision_tag(session: &mut Session) -> Option<String> {
    session
        .revision()
        .ok()
        .map(|revision| encode_id(revision.root.as_bytes()))
}
