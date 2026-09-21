//! The one request sequence behind every surface's answer.
//!
//! A few commands are not one round trip. A declaration page is a document
//! *plus* the rows around it; an outline is a tree of keys *plus* the rows that
//! give those keys names. If each surface decided that sequence itself, the CLI
//! and the MCP would answer the same question with different evidence the first
//! time one of them was edited — which is exactly the class of divergence this
//! crate exists to make impossible.
//!
//! So the sequence lives here, once, expressed against [`Engine`] rather than
//! against a socket. An engine is three methods: read the revision, read the
//! readiness report, and answer one [`Probe`]. Every surface implements those
//! by forwarding to `backend_client::Session`, which is a mechanical
//! twenty-line `match` and carries no judgement; every *decision* — which
//! probes a page needs, what a failed probe means, how a reply that changed
//! shape is reported — is made here and is shared.
//!
//! This keeps the crate's purity claim true: a test drives [`answer`] with an
//! [`Engine`] built from canned library values, with no daemon, socket, clock,
//! or filesystem anywhere.
//!
//! A probe that fails does not fail the command. It becomes a note on the page,
//! because "I could not look" and "there are none" are different answers and a
//! reader is owed the difference.

use crate::assemble::{outline_tree, page_from_document, record_list, shelf_from_snapshot};
use crate::call::Request;
use crate::fault::{Affordance, Fault, Operand};
use crate::identity::{Coordinate, Identity, KeyTag};
use crate::outline::OutlineTree;
use crate::page::Page;
use crate::product::{ProductRecord, ProductView, product_view};
use crate::record::RecordList;
use crate::shelf::Shelf;
use crate::status::Status;
use backend_client::ClientError;
use backend_library::{
    CommandReply, HealthReport, IntentId, PageContinuation, ReplyDto, Row, SurfaceCommand,
    SurfaceReply, ViewSnapshot, ViewStateRoot,
};

/// Rows fetched for one page's members and relations.
const PROBE_ROWS: u16 = 200;

/// One bounded read an [`Engine`] can be asked for.
///
/// Every variant maps to exactly one `backend_client::Session` method, so an
/// adapter is a `match` with no branching of its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Probe<'a> {
    /// Every project on the shelf.
    Packages,
    /// Submit an index intent for one project path.
    Index(&'a str),
    /// Submit a remove intent for one project path.
    Remove(&'a str),
    /// One declaration's document.
    Document(&'a str),
    /// One declaration's captured source.
    Source(&'a str),
    /// One declaration's incoming and outgoing neighbourhood.
    Related(&'a str),
    /// One declaration's outgoing neighbourhood.
    Graph(&'a str),
    /// A bounded search page.
    Search {
        /// Query text.
        text: &'a str,
        /// Page bound.
        limit: u16,
    },
    /// A bounded name resolution page.
    Names {
        /// Name text.
        text: &'a str,
        /// Page bound.
        limit: u16,
    },
    /// One package's containment tree, as keys.
    Outline(&'a str),
    /// One package's flat outline rows, so keys can be named.
    OutlinePage {
        /// Project path.
        path: &'a str,
        /// Page bound.
        limit: u16,
    },
}

impl Probe<'_> {
    /// Returns the operand a failure of this probe is about.
    #[must_use]
    pub fn operand(self) -> Operand {
        match self {
            Self::Packages => Operand::Whole,
            Self::Index(path)
            | Self::Remove(path)
            | Self::Outline(path)
            | Self::OutlinePage { path, .. } => Operand::Path(path.to_owned()),
            Self::Document(at) | Self::Source(at) | Self::Related(at) | Self::Graph(at) => {
                Operand::Coordinate(Coordinate::new(at))
            }
            Self::Search { text, .. } | Self::Names { text, .. } => Operand::Text(text.to_owned()),
        }
    }
}

/// Whatever a surface talks to in order to read an admitted reply.
///
/// The one implementation in production forwards to a connected local daemon
/// session; the implementations in tests return canned library values. Nothing
/// in this crate knows which it is holding.
pub trait Engine {
    /// Reads the current immutable product revision.
    ///
    /// # Errors
    ///
    /// Returns the transport or admission failure the endpoint produced.
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError>;

    /// Reads the bounded readiness report.
    ///
    /// # Errors
    ///
    /// Returns the transport or admission failure the endpoint produced.
    fn health(&mut self) -> Result<HealthReport, ClientError>;

    /// Answers one bounded read.
    ///
    /// # Errors
    ///
    /// Returns the transport or admission failure the endpoint produced.
    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError>;

    /// Answers a bounded probe continuation.
    ///
    /// Engines that do not expose continuation transport may still answer a
    /// first page through the default implementation, but a supplied cursor
    /// is rejected rather than silently restarted at page one.
    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if continuation.is_some() {
            return Err(ClientError::Protocol(
                "this engine does not support continuation pages".to_owned(),
            ));
        }
        self.probe(probe)
    }

    /// Executes one durable product operation.
    ///
    /// # Errors
    ///
    /// Returns the transport or admission failure the endpoint produced.
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError>;
}

/// One answer, in the shared presentation vocabulary.
#[derive(Clone, Debug)]
pub enum Answer {
    /// One declaration page.
    Page(Box<Page>),
    /// One bounded result page.
    Records(Box<RecordList>),
    /// The shelf.
    Shelf(Box<Shelf>),
    /// One named outline tree.
    Outline(Box<OutlineTree>),
    /// The engine's whole state.
    Status(Box<Status>),
    /// One durable product answer.
    Product(Box<ProductView>),
}

impl Answer {
    /// Returns the stable lowercase tag naming which answer this is.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Page(_) => "page",
            Self::Records(_) => "records",
            Self::Shelf(_) => "shelf",
            Self::Outline(_) => "outline",
            Self::Status(_) => "status",
            Self::Product(_) => "product",
        }
    }

    /// Returns a typed continuation carried by a bounded records answer.
    #[must_use]
    pub fn continuation(&self) -> Option<PageContinuation> {
        match self {
            Self::Records(records) => records.continuation(),
            _ => None,
        }
    }
}

/// Executes one typed request against a connected engine.
///
/// # Errors
///
/// Returns the typed fault the engine or the transport produced, with the
/// operand the caller supplied already attached.
pub fn answer(engine: &mut dyn Engine, request: &Request) -> Result<Answer, Fault> {
    match request {
        Request::Shelf => shelf(engine),
        Request::Status => status(engine),
        Request::Index(path) => intent(engine, path, true),
        Request::Remove(path) => intent(engine, path, false),
        Request::Page(at) => page(engine, at).map(Box::new).map(Answer::Page),
        Request::Source(at) => source(engine, at),
        Request::Neighbourhood {
            coordinate,
            incoming,
        } => neighbourhood(engine, coordinate, *incoming),
        Request::Search { text, limit } => {
            let snapshot = snapshot(engine, Probe::Search { text, limit: *limit }, "search")?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::Resolve { text, limit } => {
            let snapshot = snapshot(engine, Probe::Names { text, limit: *limit }, "resolve")?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::Outline(path) => outline(engine, path),
        Request::Surface(command) => surface(engine, command),
    }
}

/// Answers a request at an owner-issued continuation, preserving the same
/// projection and admission path as the first page.
pub fn answer_paged(
    engine: &mut dyn Engine,
    request: &Request,
    continuation: Option<PageContinuation>,
) -> Result<Answer, Fault> {
    match request {
        Request::Search { text, limit } => {
            let snapshot = snapshot_page(
                engine,
                Probe::Search {
                    text,
                    limit: *limit,
                },
                continuation,
                "search",
            )?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::Resolve { text, limit } => {
            let snapshot = snapshot_page(
                engine,
                Probe::Names {
                    text,
                    limit: *limit,
                },
                continuation,
                "resolve",
            )?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        _ if continuation.is_some() => Err(Fault::usage(
            "cursor",
            "this command does not expose continuation pages",
        )),
        _ => answer(engine, request),
    }
}

fn shelf(engine: &mut dyn Engine) -> Result<Answer, Fault> {
    let revision = engine
        .revision()
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    let reply = read(engine, Probe::Packages)?;
    let CommandReply::Packages(snapshot) = reply.reply else {
        return Err(shape("packages"));
    };
    Ok(Answer::Shelf(Box::new(shelf_from_snapshot(
        &snapshot,
        KeyTag::from_key(revision.as_bytes()),
    ))))
}

/// Reads the whole engine state, without naming the caller's project.
///
/// [`Status`] can carry a [`crate::ProjectRef`], and it is deliberately not
/// given one here. A surface's idea of "the active project" is a path it
/// discovered for itself — the CLI keeps the workspace's spelling, the MCP
/// canonicalises it — so putting it in the answer would make two surfaces
/// disagree, byte for byte, about a fact neither of them read from the engine.
/// Which projects exist is what the shelf answers, and it answers it with the
/// exact coordinates the engine published.
fn status(engine: &mut dyn Engine) -> Result<Answer, Fault> {
    let report = engine
        .health()
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    Ok(Answer::Status(Box::new(Status::from_report(&report, None))))
}

fn intent(engine: &mut dyn Engine, path: &str, add: bool) -> Result<Answer, Fault> {
    let reply = read(
        engine,
        if add {
            Probe::Index(path)
        } else {
            Probe::Remove(path)
        },
    )?;
    let (heading, accepted) = match reply.reply {
        CommandReply::Added(id) => ("index", id),
        CommandReply::Removed(id) => ("remove", id),
        _ => return Err(shape("index")),
    };
    Ok(Answer::Product(Box::new(accepted_view(
        heading, path, accepted,
    ))))
}

fn accepted_view(heading: &str, path: &str, intent: IntentId) -> ProductView {
    ProductView::assembled(
        heading,
        vec![ProductRecord::new(
            format!("{heading} request accepted"),
            Some(path.to_owned()),
            vec![
                format!("intent {}", KeyTag::from_key(intent.as_bytes())),
                "pending".to_owned(),
                "read `backend health` for readiness".to_owned(),
            ],
        )],
    )
}

fn page(engine: &mut dyn Engine, coordinate: &str) -> Result<Page, Fault> {
    let reply = read(engine, Probe::Document(coordinate))?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("document"));
    };
    let mut notes = Vec::with_capacity(2);
    let relations = probe_rows(engine, coordinate, &mut notes);
    let members = probe_members(engine, coordinate, &mut notes);
    Ok(page_from_document(
        coordinate, &document, &members, &relations, notes,
    ))
}

fn source(engine: &mut dyn Engine, coordinate: &str) -> Result<Answer, Fault> {
    let reply = read(engine, Probe::Source(coordinate))?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("source"));
    };
    Ok(Answer::Page(Box::new(page_from_document(
        coordinate,
        &document,
        &[],
        &[],
        Vec::new(),
    ))))
}

fn neighbourhood(engine: &mut dyn Engine, at: &str, incoming: bool) -> Result<Answer, Fault> {
    let reply = read(
        engine,
        if incoming {
            Probe::Related(at)
        } else {
            Probe::Graph(at)
        },
    )?;
    let CommandReply::Graph(snapshot) = reply.reply else {
        return Err(shape("graph"));
    };
    Ok(Answer::Records(Box::new(record_list(at, &snapshot))))
}

fn outline(engine: &mut dyn Engine, path: &str) -> Result<Answer, Fault> {
    let reply = read(engine, Probe::Outline(path))?;
    let CommandReply::Outline(tree) = reply.reply else {
        return Err(shape("outline"));
    };
    let rows = outline_rows(engine, path).map_err(|error| {
        Fault::from_client_error(&error, Operand::Path(path.to_owned()))
    })?;
    Ok(Answer::Outline(Box::new(outline_tree(path, &tree, &rows))))
}

fn surface(engine: &mut dyn Engine, command: &SurfaceCommand) -> Result<Answer, Fault> {
    let reply = engine
        .surface(command.clone())
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    Ok(Answer::Product(Box::new(product_view(&reply))))
}

fn snapshot(
    engine: &mut dyn Engine,
    probe: Probe<'_>,
    what: &str,
) -> Result<ViewSnapshot, Fault> {
    let reply = read(engine, probe)?;
    match reply.reply {
        CommandReply::Search(snapshot) | CommandReply::Names(snapshot) => Ok(snapshot),
        _ => Err(shape(what)),
    }
}

fn snapshot_page(
    engine: &mut dyn Engine,
    probe: Probe<'_>,
    continuation: Option<PageContinuation>,
    what: &str,
) -> Result<ViewSnapshot, Fault> {
    let reply = read_page(engine, probe, continuation)?;
    match reply.reply {
        CommandReply::Search(snapshot) | CommandReply::Names(snapshot) => Ok(snapshot),
        _ => Err(shape(what)),
    }
}

fn read_page(
    engine: &mut dyn Engine,
    probe: Probe<'_>,
    continuation: Option<PageContinuation>,
) -> Result<ReplyDto, Fault> {
    let operand = probe.operand();
    engine
        .probe_page(probe, continuation)
        .map_err(|error| Fault::from_client_error(&error, operand))
}

fn read(engine: &mut dyn Engine, probe: Probe<'_>) -> Result<ReplyDto, Fault> {
    let operand = probe.operand();
    engine
        .probe(probe)
        .map_err(|error| Fault::from_client_error(&error, operand))
        .map_err(|fault| next_step(fault, probe))
}

/// Offers the step a reader can actually take after an address missed.
///
/// The engine reports that nothing is published at a coordinate; it has no
/// opinion about what to do instead, so [`Fault::from_command_failure`] leaves
/// the affordance empty. But a reader who asked for a coordinate that does not
/// exist almost always mistyped a name or is holding one from an older
/// revision, and the answer to both is the same search. The name comes out of
/// the coordinate they supplied, so this suggests their own word back to them
/// rather than inventing a query.
fn next_step(fault: Fault, probe: Probe<'_>) -> Fault {
    let (Probe::Document(at) | Probe::Source(at) | Probe::Related(at) | Probe::Graph(at)) = probe
    else {
        return fault;
    };
    if fault.slug() != crate::fault::FaultSlug::NotFound
        || !matches!(fault.affordance(), Affordance::None)
    {
        return fault;
    }
    let name = Identity::parse(at).name().to_owned();
    if name.is_empty() {
        return fault;
    }
    fault.with_affordance(Affordance::search(name))
}

/// Fetches the rows around one declaration, recording a note if it cannot.
fn probe_rows(engine: &mut dyn Engine, coordinate: &str, notes: &mut Vec<Fault>) -> Vec<Row> {
    match engine.probe(Probe::Related(coordinate)).map(|reply| reply.reply) {
        Ok(CommandReply::Graph(snapshot)) => snapshot.root.rows().to_vec(),
        Ok(_) => {
            notes.push(shape("related"));
            Vec::new()
        }
        Err(error) => {
            notes.push(
                Fault::from_client_error(
                    &error,
                    Operand::Coordinate(Coordinate::new(coordinate)),
                )
                .with_affordance(Affordance::UseCommand {
                    name: "related",
                    args: vec![coordinate.to_owned()].into_boxed_slice(),
                }),
            );
            Vec::new()
        }
    }
}

/// Fetches the owning package's flat outline so members can be named.
fn probe_members(engine: &mut dyn Engine, coordinate: &str, notes: &mut Vec<Fault>) -> Vec<Row> {
    let Some(project) = Identity::parse(coordinate).project().cloned() else {
        return Vec::new();
    };
    match outline_rows(engine, project.root()) {
        Ok(rows) => rows,
        Err(error) => {
            notes.push(
                Fault::from_client_error(&error, Operand::Path(project.root().to_owned()))
                    .with_affordance(Affordance::UseCommand {
                        name: "outline",
                        args: vec![project.root().to_owned()].into_boxed_slice(),
                    }),
            );
            Vec::new()
        }
    }
}

fn outline_rows(engine: &mut dyn Engine, path: &str) -> Result<Vec<Row>, ClientError> {
    let reply = engine.probe(Probe::OutlinePage {
        path,
        limit: PROBE_ROWS,
    })?;
    match reply.reply {
        CommandReply::ProjectionPage(page) => Ok(page.snapshot.root.rows().to_vec()),
        CommandReply::Packages(snapshot)
        | CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => Ok(snapshot.root.rows().to_vec()),
        _ => Err(ClientError::Protocol(
            "outline page reply changed shape".to_owned(),
        )),
    }
}

fn shape(what: &str) -> Fault {
    Fault::from_client_error(
        &ClientError::Protocol(format!("the {what} reply changed shape")),
        Operand::Text(what.to_owned()),
    )
}
