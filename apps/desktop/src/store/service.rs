//! One typed request/outcome seam over the shared client session.
//! Every store reaches the local service through this and nothing else.
//! Requests run on a background thread; outcomes are plain owned values.
//!
//! A `Session` is `&mut self` throughout, holds a socket with a thirty-second
//! timeout, and performs a freshness round trip before each read, so it cannot
//! be shared between the render thread and a worker. Rather than hide that
//! behind a lock, this module makes each request a self-contained unit of
//! work: connect, ask, admit the reply's shape, return owned values, hang up.
//! The cost is one socket connect per request; the benefit is that no store
//! can ever block a frame.

use backend_client::{ClientError, Session};
use backend_library::{
    CommandReply, Coverage, Document, HealthReport, Outline, PackageReference, QueryLimit, Row,
    SemanticVersionRecord, SurfaceCommand, SurfaceReply, SymbolKey,
};
use std::path::{Path, PathBuf};

/// One unit of work for the local service.
#[derive(Clone, Debug)]
pub(crate) enum Request {
    /// Search declaration text and names.
    Search {
        /// Query text.
        text: String,
        /// Bounded page size.
        limit: QueryLimit,
    },
    /// Resolve a canonical name.
    Names {
        /// Name text or prefix.
        text: String,
        /// Bounded page size.
        limit: QueryLimit,
    },
    /// Read one declaration document by coordinate.
    Document {
        /// Exact coordinate.
        coordinate: String,
    },
    /// Read one declaration document by stable identity.
    DocumentSymbol {
        /// Admitted declaration key.
        symbol: SymbolKey,
    },
    /// Read one declaration's captured source.
    Source {
        /// Exact coordinate.
        coordinate: String,
    },
    /// Read one package outline.
    Outline {
        /// Project coordinate.
        coordinate: String,
    },
    /// Read graph neighbours for one declaration.
    Graph {
        /// Admitted declaration key.
        symbol: SymbolKey,
    },
    /// Read declarations related to one declaration.
    Related {
        /// Admitted declaration key.
        symbol: SymbolKey,
    },
    /// Compile, publish, and index one project or package.
    Index {
        /// Project path or package coordinate.
        coordinate: String,
    },
    /// Take one project or package off the shelf.
    Remove {
        /// Project path or package coordinate.
        coordinate: String,
    },
    /// Read truthful health, coverage, and capability state.
    Health,
    /// Execute one durable product-surface operation.
    Surface {
        /// The closed surface command.
        command: Box<SurfaceCommand>,
    },
    /// Read immutable compiler generation history for one package.
    SemanticVersions {
        /// Package reference.
        package: Box<PackageReference>,
    },
}

impl Request {
    /// Returns the operand a fault should blame when this request fails.
    pub(crate) fn operand_spelling(&self) -> String {
        match self {
            Self::Search { text, .. } | Self::Names { text, .. } => text.clone(),
            Self::Document { coordinate }
            | Self::Source { coordinate }
            | Self::Outline { coordinate }
            | Self::Index { coordinate }
            | Self::Remove { coordinate } => coordinate.clone(),
            Self::DocumentSymbol { symbol } | Self::Graph { symbol } | Self::Related { symbol } => {
                backend_library::encode_id(symbol.as_bytes())
            }
            Self::Health => "engine health".to_owned(),
            Self::Surface { command } => format!("{:?}", command.id()),
            Self::SemanticVersions { package } => package.as_str().to_owned(),
        }
    }
}

/// A page of rows with the coverage the service vouched for.
#[derive(Clone, Debug)]
pub(crate) struct RowPage {
    rows: Vec<Row>,
    coverage: Vec<Coverage>,
}

impl RowPage {
    /// Returns the rows in producer order.
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Returns the honest lane coverage behind these rows.
    pub(crate) fn coverage(&self) -> &[Coverage] {
        &self.coverage
    }

    /// Consumes the page and returns its rows.
    pub(crate) fn into_rows(self) -> Vec<Row> {
        self.rows
    }
}

/// What one request produced.
#[derive(Clone, Debug)]
pub(crate) enum Outcome {
    /// Rows with their coverage.
    Rows(RowPage),
    /// One declaration document.
    Document(Box<Document>),
    /// One package outline.
    Outline(Box<Outline>),
    /// A durable intent was accepted.
    Accepted,
    /// Bounded health, coverage, and capability state.
    Health(Box<HealthReport>),
    /// A durable product-surface result.
    Surface(Box<SurfaceReply>),
    /// Immutable compiler generations.
    Versions(Vec<SemanticVersionRecord>),
}

/// Runs one request against the local service endpoint.
///
/// # Errors
/// Returns the client's own typed failure; callers project it into a fault.
pub(crate) fn run(endpoint: &Path, request: &Request) -> Result<Outcome, ClientError> {
    let mut session = Session::connect(endpoint)?;
    dispatch(&mut session, request)
}

fn dispatch(session: &mut Session, request: &Request) -> Result<Outcome, ClientError> {
    match request {
        Request::Search { text, limit } => rows(session.search(text, limit.get())?),
        Request::Names { text, limit } => rows(session.names(text, limit.get())?),
        Request::Document { coordinate } => document(session.document(coordinate)?),
        Request::DocumentSymbol { symbol } => document(session.document_symbol(*symbol)?),
        Request::Source { coordinate } => document(session.source(coordinate)?),
        Request::Outline { coordinate } => outline(session.outline(coordinate)?),
        Request::Graph { symbol } => rows(session.graph_symbol(*symbol)?),
        Request::Related { symbol } => rows(session.related_symbol(*symbol)?),
        Request::Index { coordinate } => accepted(session.index(coordinate)?),
        Request::Remove { coordinate } => accepted(session.remove(coordinate)?),
        Request::Health => Ok(Outcome::Health(Box::new(session.health()?))),
        Request::Surface { command } => Ok(Outcome::Surface(Box::new(
            session.surface(command.as_ref().clone())?,
        ))),
        Request::SemanticVersions { package } => Ok(Outcome::Versions(
            session
                .semantic_versions(package.as_ref().clone())?
                .into_vec(),
        )),
    }
}

fn rows(reply: backend_library::ReplyDto) -> Result<Outcome, ClientError> {
    let (CommandReply::Search(snapshot)
    | CommandReply::Names(snapshot)
    | CommandReply::Graph(snapshot)
    | CommandReply::Packages(snapshot)) = reply.reply
    else {
        return Err(shape("row"));
    };
    Ok(Outcome::Rows(RowPage {
        rows: snapshot.root.rows().to_vec(),
        coverage: snapshot.root.coverage().to_vec(),
    }))
}

fn document(reply: backend_library::ReplyDto) -> Result<Outcome, ClientError> {
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("document"));
    };
    Ok(Outcome::Document(Box::new(document)))
}

fn outline(reply: backend_library::ReplyDto) -> Result<Outcome, ClientError> {
    let CommandReply::Outline(outline) = reply.reply else {
        return Err(shape("outline"));
    };
    Ok(Outcome::Outline(Box::new(outline)))
}

fn accepted(reply: backend_library::ReplyDto) -> Result<Outcome, ClientError> {
    match reply.reply {
        CommandReply::Added(_) | CommandReply::Removed(_) => Ok(Outcome::Accepted),
        _ => Err(shape("intent")),
    }
}

fn shape(expected: &str) -> ClientError {
    ClientError::Protocol(format!("desktop {expected} reply changed shape"))
}

/// Where the local service lives, retained by every store that talks to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Endpoint(PathBuf);

impl Endpoint {
    /// Retains one endpoint path.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// Returns the endpoint path.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// Returns the endpoint path as the text a fault operand shows.
    pub(crate) fn spelling(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
