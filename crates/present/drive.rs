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

use crate::assemble::{
    outline_tree, page_from_document, page_from_document_with_graph_relations, record_list,
    shelf_from_snapshot, shelf_from_snapshot_with_health,
};
use crate::call::Request;
use crate::fault::{Affordance, Candidate, Fault, MAX_FAULT_CANDIDATES, Operand};
use crate::identity::{Coordinate, Identity, IdentityShape, KeyTag, LineNumber, PackagePath};
use crate::outline::OutlineTree;
use crate::page::Page;
use crate::product::{ProductRecord, ProductView};
use crate::record::{Record, RecordList};
use crate::shelf::Shelf;
use crate::status::Status;
use backend_client::ClientError;
use backend_library::{
    CommandReply, CompileExecutionIntent, GraphRelation, HealthReport, IndexSearchCursor, IntentId,
    PageContinuation, ProductText, ReplyDto, Row, SurfaceCommand, SurfaceReply, ViewSnapshot,
    ViewStateRoot,
};

/// One owner-issued continuation family supported by agent-facing surfaces.
///
/// Page continuations are authenticated by the shared catalog owner. Index
/// search cursors are opaque strings owned by the durable product surface and
/// must pass through that owner unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContinuationCursor {
    /// A presentation page continuation such as search, resolve, or graph.
    Page(PageContinuation),
    /// An opaque durable index-search continuation.
    IndexSearch(IndexSearchCursor),
}

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
    /// Submit an index intent for one project path using the normal interactive default.
    Index(&'a str),
    /// Submit an index intent with a caller-selected execution class.
    IndexWithExecutionIntent {
        /// Project root to add.
        path: &'a str,
        /// Requested compilation execution class.
        execution_intent: CompileExecutionIntent,
    },
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
            | Self::IndexWithExecutionIntent { path, .. }
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

    /// Reads the bounded readiness report after owner, request correlation and
    /// health proof admission. Implementations must reject malformed or
    /// unadmitted reports instead of returning a successful health response.
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

    /// Re-admits an imported query proof against the current authenticated owner.
    fn prepare_query_continuation(
        &mut self,
        _token: &str,
    ) -> Result<PageContinuation, ClientError> {
        Err(ClientError::Protocol(
            "engine does not support portable query proofs".to_owned(),
        ))
    }

    /// Executes one durable product operation.
    ///
    /// # Errors
    ///
    /// Returns the transport or admission failure the endpoint produced.
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError>;

    /// Accepts or reconciles an index request. Product adapters bind this to
    /// the shared client's durable caller journal; in-memory engines may use
    /// the existing immediate ticket contract without filesystem state.
    fn index(
        &mut self,
        package: backend_library::PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<SurfaceReply, ClientError> {
        self.surface(SurfaceCommand::IndexStart {
            package,
            execution_intent,
        })
    }
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
    /// Returns the shared failure carried by an otherwise admitted answer.
    /// The full answer remains available, including exact owner receipts.
    #[must_use]
    pub fn fault(&self) -> Option<&Fault> {
        match self {
            Self::Product(product) => product.fault(),
            _ => None,
        }
    }

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
            Self::Outline(outline) => outline.continuation(),
            _ => None,
        }
    }

    /// Returns the owner-issued cursor family carried by this answer.
    #[must_use]
    pub fn cursor(&self) -> Option<ContinuationCursor> {
        match self {
            Self::Records(records) => records.continuation().map(ContinuationCursor::Page),
            Self::Outline(outline) => outline.continuation().map(ContinuationCursor::Page),
            Self::Product(product) => product.cursor_family().cloned(),
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
        Request::ResumeQuery { query, cursor } => {
            if !matches!(
                query.as_ref(),
                Request::Search { .. } | Request::Resolve { .. }
            ) {
                return Err(Fault::usage(
                    "cursor",
                    "only search and names have portable query continuations",
                ));
            }
            let continuation = engine.prepare_query_continuation(cursor).map_err(|error| {
                Fault::from_client_error(&error, Operand::Argument("cursor".to_owned()))
            })?;
            answer_paged(engine, query, Some(continuation))
        }
        Request::Shelf => shelf(engine),
        Request::Status => status(engine),
        Request::Index(path) => index(engine, path, CompileExecutionIntent::Interactive),
        Request::IndexWithExecutionIntent {
            path,
            execution_intent,
        } => index(engine, path, *execution_intent),
        Request::Remove(path) => remove(engine, path),
        Request::Page(at) => page(engine, at).map(Box::new).map(Answer::Page),
        Request::Source(at) => source(engine, at),
        Request::Neighbourhood {
            coordinate,
            incoming,
        } => neighbourhood(engine, coordinate, *incoming),
        Request::Search { text, limit } => {
            let reply = read(
                engine,
                Probe::Search {
                    text,
                    limit: *limit,
                },
            )?;
            let (snapshot, semantic_search) = search_snapshot(reply, "search")?;
            let list = record_list(text, &snapshot);
            let list = if let Some(status) = semantic_search {
                list.with_semantic_search_status(status)
            } else {
                list
            };
            let list = if list.is_empty() {
                list.with_empty_reason(
                    "no indexed declarations matched this query text; try a declaration name, signature fragment, or documentation keyword",
                )
            } else {
                list
            };
            Ok(Answer::Records(Box::new(list)))
        }
        Request::Resolve { text, limit } => {
            let snapshot = snapshot(
                engine,
                Probe::Names {
                    text,
                    limit: *limit,
                },
                "resolve",
            )?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::Outline(path) => outline(engine, path),
        Request::OutlinePage { .. } => answer_paged(engine, request, None),
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
            let reply = read_page(
                engine,
                Probe::Search {
                    text,
                    limit: *limit,
                },
                continuation,
            )?;
            let (snapshot, semantic_search) = search_snapshot(reply, "search")?;
            let list = record_list(text, &snapshot);
            let list = if let Some(status) = semantic_search {
                list.with_semantic_search_status(status)
            } else {
                list
            };
            let list = if list.is_empty() {
                list.with_empty_reason(
                    "no indexed declarations matched this query text; try a declaration name, signature fragment, or documentation keyword",
                )
            } else {
                list
            };
            Ok(Answer::Records(Box::new(list)))
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
        Request::OutlinePage { path, limit } => {
            let reply = read_page(
                engine,
                Probe::OutlinePage {
                    path,
                    limit: *limit,
                },
                continuation,
            )?;
            let CommandReply::ProjectionPage(page) = reply.reply else {
                return Err(shape("outline-page"));
            };
            if page.terminal
                != page
                    .snapshot
                    .next
                    .map_or(backend_library::PageTerminal::Complete, |cursor| {
                        backend_library::PageTerminal::More(PageContinuation::from_cursor(cursor))
                    })
            {
                return Err(shape("outline-page terminal"));
            }
            crate::outline::page_tree(path, &page.snapshot, *limit)
                .map(Box::new)
                .map(Answer::Outline)
                .map_err(|error| Fault::from_client_error(&error, Operand::Path(path.clone())))
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
    let tag = KeyTag::from_key(revision.as_bytes());
    // The packages reply carries the lane coverage of its revision but only
    // package rows, so it cannot say how many rows the owner published. The
    // health report can. Read it so `packages` and `status` print the same
    // count; if it is unavailable or describes another revision, the shelf
    // reports the count as unread rather than inventing one.
    Ok(Answer::Shelf(Box::new(match engine.health() {
        Ok(health) => shelf_from_snapshot_with_health(&snapshot, tag, &health),
        Err(_) => shelf_from_snapshot(&snapshot, tag),
    })))
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
    Ok(Answer::Status(Box::new(Status::from_health_reply(
        &report, None,
    ))))
}

fn index(
    engine: &mut dyn Engine,
    path: &str,
    execution_intent: CompileExecutionIntent,
) -> Result<Answer, Fault> {
    let package = backend_library::PackageReference::parse(path.to_owned())
        .map_err(|error| Fault::admission(error, Operand::Path(path.to_owned())))?;
    let reply = engine
        .index(package, execution_intent)
        .map_err(|error| Fault::from_client_error(&error, Operand::Path(path.to_owned())))?;
    Ok(Answer::Product(Box::new(crate::product_view(&reply))))
}

fn remove(engine: &mut dyn Engine, path: &str) -> Result<Answer, Fault> {
    let reply = read(engine, Probe::Remove(path))?;
    let CommandReply::Removed(accepted) = reply.reply else {
        return Err(shape("remove"));
    };
    Ok(Answer::Product(Box::new(accepted_view(
        "remove", path, accepted,
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
                "read `nudox health` for the current readiness observation".to_owned(),
            ],
        )],
    )
}

fn page(engine: &mut dyn Engine, text: &str) -> Result<Page, Fault> {
    let coordinate = declaration_operand(engine, text, "show")?;
    let reply = read_declaration(engine, Probe::Document(&coordinate), &coordinate, "show")?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("document"));
    };
    // Either spelling of a declaration both lanes published answers with the
    // same page, addressed by its canonical (structural) coordinate.
    let canonical = canonical_coordinate(&coordinate, &document);
    let mut notes = Vec::with_capacity(2);
    let relation_rows = probe_rows(engine, &canonical, &mut notes);
    let members = probe_members(engine, &canonical, &document, &mut notes);
    Ok(page_from_document_with_graph_relations(
        &canonical,
        &document,
        &members,
        &relation_rows.rows,
        relation_rows.graph_relations.as_deref().unwrap_or(&[]),
        notes,
    ))
}

fn source(engine: &mut dyn Engine, text: &str) -> Result<Answer, Fault> {
    let coordinate = declaration_operand(engine, text, "source")?;
    let reply = read_declaration(engine, Probe::Source(&coordinate), &coordinate, "source")?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("source"));
    };
    let canonical = canonical_coordinate(&coordinate, &document);
    let page = page_from_document(&canonical, &document, &[], &[], Vec::new());
    // Unlike a document page, this request asks specifically for source.
    // Retain the exact typed reason when no captured text can be supplied.
    if let Some(fault) = page.source().fault() {
        return Err(fault.clone());
    }
    Ok(Answer::Page(Box::new(page)))
}

/// Resolves the declaration operand of a coordinate-taking command to its one
/// canonical, complete coordinate.
///
/// A complete coordinate in either spelling of a declaration both lanes
/// published (`<project>::<path>:<line>::<name>` or
/// `<project>::semantic::<id>::<name>`) resolves to the structural one; a name
/// or partial address resolves through the same resolver as `resolve`. This is
/// the one place that decides, so a surface that pages a declaration's
/// neighbourhood itself asks here first and cannot disagree with `show`.
///
/// # Errors
///
/// Returns an `ambiguous` fault listing the candidates when several
/// declarations match, a `not-found` fault with the nearest declarations when
/// none does, or the typed fault the engine produced.
pub fn resolve_declaration(
    engine: &mut dyn Engine,
    text: &str,
    command: &'static str,
) -> Result<String, Fault> {
    let coordinate = declaration_operand(engine, text, command)?;
    Ok(canonical_operand(engine, coordinate))
}

fn neighbourhood(engine: &mut dyn Engine, text: &str, incoming: bool) -> Result<Answer, Fault> {
    let command = if incoming { "related" } else { "graph" };
    let at = resolve_declaration(engine, text, command)?;
    let reply = read_declaration(
        engine,
        if incoming {
            Probe::Related(&at)
        } else {
            Probe::Graph(&at)
        },
        &at,
        command,
    )?;
    let at = at.as_str();
    let CommandReply::Graph(snapshot) = reply.reply else {
        return Err(shape("graph"));
    };
    let list = crate::assemble::neighbour_list(at, &snapshot);
    let list = if list.is_empty() {
        list.with_empty_reason("no graph edges at this coordinate; the authority recorded none")
    } else {
        list
    };
    Ok(Answer::Records(Box::new(list)))
}

fn outline(engine: &mut dyn Engine, path: &str) -> Result<Answer, Fault> {
    let reply = read(engine, Probe::Outline(path))?;
    let CommandReply::Outline(tree) = reply.reply else {
        return Err(shape("outline"));
    };
    let snapshot = outline_snapshot(engine, path)
        .and_then(|snapshot| {
            check_outline_projection(backend_library::package_key(path), &tree, &snapshot)?;
            Ok(snapshot)
        })
        .map_err(|error| Fault::from_client_error(&error, Operand::Path(path.to_owned())))?;
    Ok(Answer::Outline(Box::new(outline_tree(
        path,
        &tree,
        snapshot.root.rows(),
    ))))
}

fn surface(engine: &mut dyn Engine, command: &SurfaceCommand) -> Result<Answer, Fault> {
    let command = resolve_surface_operands(engine, command)?;
    let reply = engine
        .surface(command.clone())
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    Ok(Answer::Product(Box::new(crate::product_view_for_command(
        &command, &reply,
    ))))
}

/// Names rows the resolver reads to decide whether text names one declaration.
const NAME_ROWS: u16 = 24;

/// Resolves the declaration operands of the commands that take a coordinate
/// (`references`, `read`) exactly as `show`, `graph` and the rest do, so every
/// command accepts the same spellings of one declaration.
fn resolve_surface_operands(
    engine: &mut dyn Engine,
    command: &SurfaceCommand,
) -> Result<SurfaceCommand, Fault> {
    let admit = |text: String| {
        ProductText::new(text.as_str())
            .map_err(|error| Fault::admission(error, Operand::Argument(text)))
    };
    match command {
        SurfaceCommand::References { target } => {
            let coordinate = declaration_operand(engine, target.as_str(), "references")?;
            let coordinate = canonical_operand(engine, coordinate);
            if coordinate == target.as_str() {
                return Ok(command.clone());
            }
            Ok(SurfaceCommand::References {
                target: admit(coordinate)?,
            })
        }
        SurfaceCommand::Read { locators } => {
            let mut resolved: Vec<ProductText> = Vec::with_capacity(locators.len());
            for locator in locators.iter() {
                let coordinate = declaration_operand(engine, locator.as_str(), "read")?;
                let coordinate = canonical_operand(engine, coordinate);
                // Two spellings of one declaration are read once.
                if resolved.iter().any(|seen| seen.as_str() == coordinate) {
                    continue;
                }
                resolved.push(if coordinate == locator.as_str() {
                    locator.clone()
                } else {
                    admit(coordinate)?
                });
            }
            Ok(SurfaceCommand::Read {
                locators: resolved.into_boxed_slice(),
            })
        }
        other => Ok(other.clone()),
    }
}

/// Whether `text` is spelled as an engine coordinate: rooted at an absolute
/// project path (or a registry package). The owner answers for it, exactly as
/// it names a declaration or refuses it. Anything else is a name or partial
/// address for the resolver.
fn is_complete_coordinate(text: &str) -> bool {
    text.starts_with('/')
        || text.starts_with("pkg:")
        || matches!(
            text.as_bytes(),
            [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic()
        )
}

/// The operand of a coordinate-taking command, as a complete coordinate.
///
/// A complete coordinate passes through untouched. Any other text goes to the
/// same resolver `resolve` runs, so a bare name cannot fail where `resolve`
/// would have found the declaration.
fn declaration_operand(
    engine: &mut dyn Engine,
    text: &str,
    command: &'static str,
) -> Result<String, Fault> {
    if is_complete_coordinate(text) {
        return Ok(text.to_owned());
    }
    resolve_name(engine, text, command)
}

/// Splits an operand like `src/lib/scheduler.ts:21::generateSchedule` into
/// the name to resolve and the file (and line) that must hold the match.
struct NameOperand<'text> {
    name: &'text str,
    file: Option<&'text str>,
    line: Option<u32>,
}

impl<'text> NameOperand<'text> {
    fn parse(text: &'text str) -> Self {
        let plain = Self {
            name: text,
            file: None,
            line: None,
        };
        let Some((qualifier, name)) = text.rsplit_once("::") else {
            return plain;
        };
        if name.is_empty() {
            return plain;
        }
        let (file, line) = match qualifier.rsplit_once(':') {
            Some((file, digits))
                if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                (file, digits.parse::<u32>().ok())
            }
            _ => (qualifier, None),
        };
        // Only a spelling that reads as a source file qualifies the name;
        // `Class::method` and `module::function` stay relative names.
        let reads_as_file = crate::language::Language::from_path(&PackagePath::new(file))
            != crate::language::Language::Unknown;
        if file.is_empty() || !reads_as_file {
            return plain;
        }
        Self {
            name,
            file: Some(file),
            line,
        }
    }

    /// The final name segment, which a match must carry exactly.
    fn leaf(&self) -> &'text str {
        let after_colons = self.name.rsplit("::").next().unwrap_or(self.name);
        after_colons.rsplit('.').next().unwrap_or(after_colons)
    }

    fn admits(&self, record: &Record) -> bool {
        let identity = record.identity();
        if let Some(file) = self.file {
            let Some(path) = identity.path().map(PackagePath::as_str) else {
                return false;
            };
            let at_boundary = path
                .strip_suffix(file)
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'));
            if !at_boundary {
                return false;
            }
        }
        self.line
            .is_none_or(|line| identity.line().map(LineNumber::get) == Some(line))
    }
}

/// Whether a record is sited in the package's library or application code,
/// rather than in an example, a test or a benchmark.
fn defined_in_library_code(record: &Record) -> bool {
    record
        .identity()
        .path()
        .is_some_and(|path| !backend_library::is_support_source_path(path.as_str()))
}

/// Whether a record is sited in an example, a test or a benchmark.
fn defined_in_support_code(record: &Record) -> bool {
    record
        .identity()
        .path()
        .is_some_and(|path| backend_library::is_support_source_path(path.as_str()))
}

/// Whether a record is a compiler type row: a type with no site of its own,
/// which stands for the type a declaration defines rather than a second
/// definition.
fn is_type_value(record: &Record) -> bool {
    record.identity().path().is_none()
        && record.kind() == Some(backend_library::DeclarationKind::Type)
}

/// A record a name can resolve to: a declaration, never an import that only
/// brings a name in, a module, a package or an external placeholder.
fn names_a_declaration(record: &Record) -> bool {
    matches!(
        record.identity().shape(),
        IdentityShape::Declaration | IdentityShape::Semantic
    ) && record.kind() != Some(backend_library::DeclarationKind::Import)
}

/// Longest signature or summary, in characters, a candidate spells out.
const CANDIDATE_DETAIL_CHARS: usize = 72;

/// The first line of `text`, trimmed and cut to `limit` characters.
fn clip_line(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= limit {
        return line.to_owned();
    }
    let mut clipped = line.chars().take(limit).collect::<String>();
    clipped.push('…');
    clipped
}

/// Whether the records span more than one project, so a candidate's location
/// must name its project to be told apart.
fn spans_projects<'a>(records: impl IntoIterator<Item = &'a Record>) -> bool {
    let mut roots = records.into_iter().filter_map(|record| {
        record
            .identity()
            .project()
            .map(crate::identity::ProjectRef::root)
    });
    roots
        .next()
        .is_some_and(|first| roots.any(|root| root != first))
}

fn candidate_of(record: &Record, command: &'static str, project: bool) -> Candidate {
    let identity = record.identity();
    // A declaration whose producer captured no site is told apart by what it
    // does say about itself: its signature, then its first documentation line.
    let location = match (identity.path(), identity.line()) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path.to_string(),
        _ => record
            .signature()
            .map(crate::signature::Signature::text)
            .or_else(|| record.summary().map(str::to_owned))
            .map_or_else(
                || identity.name().to_owned(),
                |text| clip_line(&text, CANDIDATE_DETAIL_CHARS),
            ),
    };
    let location = match identity.project() {
        Some(owner) if project => format!("{} › {location}", owner.name()),
        _ => location,
    };
    Candidate::new(
        identity.coordinate().clone(),
        record.kind(),
        location,
        command,
    )
}

/// Resolves name text with the resolver behind `resolve`.
///
/// Exactly one declaration (after the two lanes' duplicates are merged and
/// imports and placeholders are set aside) selects its coordinate. Several are
/// refused with a bounded candidate list and the exact retry for each; none is
/// refused with the nearest names.
fn resolve_name(
    engine: &mut dyn Engine,
    text: &str,
    command: &'static str,
) -> Result<String, Fault> {
    let operand = NameOperand::parse(text);
    let snapshot = snapshot(
        engine,
        Probe::Names {
            text: operand.name,
            limit: NAME_ROWS,
        },
        "resolve",
    )?;
    let list = record_list(text, &snapshot);
    let leaf = operand.leaf();
    let declared = list
        .records()
        .iter()
        .filter(|record| names_a_declaration(record))
        .collect::<Vec<_>>();
    let mut matches = declared
        .iter()
        .copied()
        .filter(|record| record.identity().name() == leaf && operand.admits(record))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        matches = declared
            .iter()
            .copied()
            .filter(|record| {
                record.identity().name().eq_ignore_ascii_case(leaf) && operand.admits(record)
            })
            .collect();
    }
    // A compiler type row names the type its declaration defines; it never
    // competes with a declaration that has a site.
    if matches.len() > 1 {
        let declarations = matches
            .iter()
            .copied()
            .filter(|record| !is_type_value(record))
            .collect::<Vec<_>>();
        if !declarations.is_empty() {
            matches = declarations;
        }
    }
    // When one match is defined in library code and the rest only in
    // examples, tests or benchmarks, the library definition is the one the
    // name means; the rest stay reachable by their exact coordinates.
    if matches.len() > 1 {
        let library = matches
            .iter()
            .copied()
            .filter(|record| defined_in_library_code(record))
            .collect::<Vec<_>>();
        if let [primary] = library.as_slice()
            && matches
                .iter()
                .all(|record| std::ptr::eq(*record, *primary) || defined_in_support_code(record))
        {
            matches = vec![*primary];
        }
    }
    match matches.as_slice() {
        [only] => Ok(only.identity().coordinate().as_str().to_owned()),
        [] => {
            let near = list
                .records()
                .iter()
                .filter(|record| {
                    matches!(
                        record.identity().shape(),
                        IdentityShape::Declaration | IdentityShape::Semantic
                    )
                })
                .take(MAX_FAULT_CANDIDATES)
                .collect::<Vec<_>>();
            let project = spans_projects(near.iter().copied());
            let nearest = near
                .iter()
                .map(|record| candidate_of(record, command, project))
                .collect();
            Err(Fault::unresolved(text, nearest))
        }
        several => {
            let project = spans_projects(several.iter().copied());
            Err(Fault::ambiguous(
                text,
                several.len(),
                !list.has_more(),
                several
                    .iter()
                    .take(MAX_FAULT_CANDIDATES)
                    .map(|record| candidate_of(record, command, project))
                    .collect(),
            ))
        }
    }
}

/// Reads one declaration probe; when the complete coordinate is absent,
/// offers the declarations that share its name instead of a bare absence.
fn read_declaration(
    engine: &mut dyn Engine,
    probe: Probe<'_>,
    coordinate: &str,
    command: &'static str,
) -> Result<ReplyDto, Fault> {
    let fault = match read(engine, probe) {
        Ok(reply) => return Ok(reply),
        Err(fault) => fault,
    };
    if fault.slug() != crate::fault::FaultSlug::NotFound {
        return Err(fault);
    }
    let name = Identity::parse(coordinate).name().to_owned();
    if name.is_empty() {
        return Err(fault);
    }
    let nearest = engine
        .probe(Probe::Names {
            text: &name,
            limit: NAME_ROWS,
        })
        .ok()
        .and_then(|reply| match reply.reply {
            CommandReply::Names(snapshot) | CommandReply::Search(snapshot) => Some(snapshot),
            _ => None,
        })
        .map(|snapshot| record_list(&name, &snapshot))
        .map(|list| {
            let sharing = list
                .records()
                .iter()
                .filter(|record| names_a_declaration(record) && record.identity().name() == name)
                .take(MAX_FAULT_CANDIDATES)
                .collect::<Vec<_>>();
            let project = spans_projects(sharing.iter().copied());
            sharing
                .iter()
                .map(|record| candidate_of(record, command, project))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if nearest.is_empty() {
        return Err(fault);
    }
    Err(Fault::unresolved(coordinate, nearest)
        .about(Operand::Coordinate(Coordinate::new(coordinate))))
}

/// The structural spelling of a declaration the caller named by its
/// compiler-backed (`<project>::semantic::<id>::<name>`) coordinate.
///
/// The document says where the declaration sits, and the structural spelling
/// is accepted only when its key is the document's own, so a declaration that
/// has no structural row keeps the coordinate it was named by.
fn canonical_coordinate(requested: &str, document: &backend_library::Document) -> String {
    let Some(location) = document.location.captured() else {
        return requested.to_owned();
    };
    let Some((project, rest)) = requested.split_once("::semantic::") else {
        return requested.to_owned();
    };
    let Some((_, name)) = rest.split_once("::") else {
        return requested.to_owned();
    };
    if project.is_empty() || name.is_empty() {
        return requested.to_owned();
    }
    let structural = format!(
        "{project}::{}:{}::{name}",
        location.path(),
        location.start_line()
    );
    if backend_library::symbol_key(&structural) == document.symbol {
        structural
    } else {
        requested.to_owned()
    }
}

/// The canonical coordinate of a declaration named by its compiler-backed
/// spelling; any other coordinate is already canonical.
fn canonical_operand(engine: &mut dyn Engine, coordinate: String) -> String {
    if Identity::parse(&coordinate).shape() != IdentityShape::Semantic {
        return coordinate;
    }
    match engine.probe(Probe::Document(&coordinate)) {
        Ok(reply) => match reply.reply {
            CommandReply::Document(document) | CommandReply::Page(document) => {
                canonical_coordinate(&coordinate, &document)
            }
            _ => coordinate,
        },
        Err(_) => coordinate,
    }
}

fn snapshot(engine: &mut dyn Engine, probe: Probe<'_>, what: &str) -> Result<ViewSnapshot, Fault> {
    let reply = read(engine, probe)?;
    match reply.reply {
        CommandReply::Search(snapshot) | CommandReply::Names(snapshot) => Ok(snapshot),
        _ => Err(shape(what)),
    }
}

fn search_snapshot(
    reply: ReplyDto,
    what: &str,
) -> Result<(ViewSnapshot, Option<backend_library::SemanticSearchStatus>), Fault> {
    let semantic_search = reply.semantic_search_status();
    match reply.reply {
        CommandReply::Search(snapshot) => Ok((snapshot, semantic_search)),
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
    engine
        .probe_page(probe, continuation)
        .map_err(|error| probe_fault(&error, probe))
}

fn read(engine: &mut dyn Engine, probe: Probe<'_>) -> Result<ReplyDto, Fault> {
    engine
        .probe(probe)
        .map_err(|error| probe_fault(&error, probe))
}

/// Preserves a typed client failure and its requested operand, adding only
/// the actionable next step justified by that exact probe and failure class.
/// Every surface, including direct graph-page adapters, shares this lowering.
#[must_use]
pub fn probe_fault(error: &ClientError, probe: Probe<'_>) -> Fault {
    next_step(Fault::from_client_error(error, probe.operand()), probe)
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
    if let Probe::Search { text, limit } = probe
        && fault.slug() == crate::FaultSlug::QueryPreparation
    {
        return fault.with_affordance(Affordance::RetrySearch {
            text: text.to_owned(),
            limit,
        });
    }
    if let Probe::Outline(path) | Probe::OutlinePage { path, .. } = probe
        && fault.slug() == crate::FaultSlug::NotFound
    {
        return Fault::unpublished_outline(path);
    }
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

struct RelationRows {
    rows: Vec<Row>,
    graph_relations: Option<Box<[GraphRelation]>>,
}

/// Fetches graph rows and keeps the compiler-selected typed edge sidecar
/// attached to the same response revision.
fn probe_rows(engine: &mut dyn Engine, coordinate: &str, notes: &mut Vec<Fault>) -> RelationRows {
    match engine
        .probe(Probe::Related(coordinate))
        .map(|reply| reply.reply)
    {
        Ok(CommandReply::Graph(snapshot)) => RelationRows {
            rows: snapshot.root.rows().to_vec(),
            graph_relations: snapshot.graph_relations,
        },
        Ok(_) => {
            notes.push(shape("related"));
            RelationRows {
                rows: Vec::new(),
                graph_relations: None,
            }
        }
        Err(error) => {
            notes.push(
                Fault::from_client_error(&error, Operand::Coordinate(Coordinate::new(coordinate)))
                    .with_affordance(Affordance::UseCommand {
                        name: "related",
                        args: vec![coordinate.to_owned()].into_boxed_slice(),
                    }),
            );
            RelationRows {
                rows: Vec::new(),
                graph_relations: None,
            }
        }
    }
}

/// Fetches the owning package's flat outline so members can be named.
fn probe_members(
    engine: &mut dyn Engine,
    coordinate: &str,
    document: &backend_library::Document,
    notes: &mut Vec<Fault>,
) -> Vec<Row> {
    let Some(project) = Identity::parse(coordinate).project().cloned() else {
        return Vec::new();
    };
    match document_member_rows(engine, project.root(), document) {
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

/// Streams the existing owner-issued package projection without retaining its
/// unrelated rows. A first package page cannot prove a declaration has no more
/// members; only the terminal page closes this retained-row observation.
/// This does not promote structural coverage to native/source completeness.
fn document_member_rows(
    engine: &mut dyn Engine,
    path: &str,
    document: &backend_library::Document,
) -> Result<Vec<Row>, ClientError> {
    let package = backend_library::package_key(path);
    let mut selected = std::collections::BTreeMap::new();
    let mut continuation = None;
    let mut frontier = None;
    let mut source_basis = document.source_basis();
    loop {
        let reply = engine.probe_page(
            Probe::OutlinePage {
                path,
                limit: PROBE_ROWS,
            },
            continuation,
        )?;
        let (snapshot, terminal) = match reply.reply {
            CommandReply::ProjectionPage(page) => (page.snapshot, Some(page.terminal)),
            // Root-only in-process constructors retain their compatibility
            // shape. A real continuation still goes through probe_page, which
            // refuses unsupported transports instead of restarting page one.
            CommandReply::Packages(snapshot)
            | CommandReply::Names(snapshot)
            | CommandReply::Search(snapshot)
            | CommandReply::Graph(snapshot) => (snapshot, None),
            _ => {
                return Err(ClientError::Protocol(
                    "document member projection changed shape".to_owned(),
                ));
            }
        };
        let source = snapshot.root.basis();
        if source.root != document.basis {
            return Err(ClientError::CommandFailed(
                backend_library::CommandFailure::WrongBasis {
                    expected: document.basis.into(),
                    observed: source.root.into(),
                },
            ));
        }
        if source_basis.is_some_and(|basis| basis != source) {
            return Err(ClientError::Protocol(
                "document member source object or stream differs from its pinned source".to_owned(),
            ));
        }
        // Legacy root-only documents cannot attest their object. Once the
        // first outline page does, all later pages must retain that exact
        // object as well; the stream frontier does not include object identity.
        source_basis = Some(source);
        let observed = snapshot.root.frontier();
        if continuation.is_some_and(|previous: PageContinuation| {
            snapshot.root.recipe() != previous.cursor().recipe()
        }) {
            return Err(ClientError::Protocol(
                "document member query changed between pages".to_owned(),
            ));
        }
        if frontier.is_some_and(|expected| expected != observed) {
            return Err(ClientError::Protocol(
                "document member frontier changed between pages".to_owned(),
            ));
        }
        frontier = Some(observed);
        let expected_terminal = snapshot
            .next
            .map_or(backend_library::PageTerminal::Complete, |cursor| {
                backend_library::PageTerminal::More(PageContinuation::from_cursor(cursor))
            });
        if terminal.is_some_and(|terminal| terminal != expected_terminal) {
            return Err(ClientError::Protocol(
                "document member page terminal does not match its cursor".to_owned(),
            ));
        }
        if snapshot.root.rows().len() > usize::from(PROBE_ROWS) && terminal.is_some() {
            return Err(ClientError::Protocol(
                "document member page exceeds its admitted limit".to_owned(),
            ));
        }
        for row in snapshot.root.rows() {
            if row.id != backend_library::RowId::Symbol(document.symbol)
                && row.parent != Some(document.symbol)
            {
                continue;
            }
            if row.package != Some(package) || !matches!(row.id, backend_library::RowId::Symbol(_))
            {
                return Err(ClientError::Protocol(
                    "document member row belongs to another package".to_owned(),
                ));
            }
            if selected.insert(row.id, row.clone()).is_some() {
                return Err(ClientError::Protocol(
                    "document member row repeated across pages".to_owned(),
                ));
            }
        }
        let Some(next) = snapshot.next else {
            break;
        };
        // Each cursor certifies its own page image, not the previous page's
        // content version or the source relation root. Only the query recipe
        // and source frontier remain constant across distinct page images.
        let page_cursor = backend_library::Cursor::for_view_root(&snapshot.root);
        let expected_offset = continuation
            .map_or(0, |previous| previous.cursor().query_offset())
            .checked_add(u64::from(PROBE_ROWS));
        if snapshot.root.rows().len() != usize::from(PROBE_ROWS)
            || next.recipe() != page_cursor.recipe()
            || next.version() != page_cursor.version()
            || next.frontier() != page_cursor.frontier()
            || Some(next.query_offset()) != expected_offset
        {
            return Err(ClientError::Protocol(
                "document member continuation did not advance in its source context".to_owned(),
            ));
        }
        continuation = Some(PageContinuation::from_cursor(next));
    }
    if !selected.contains_key(&backend_library::RowId::Symbol(document.symbol)) {
        return Err(ClientError::Protocol(
            "document owner is absent from its member projection".to_owned(),
        ));
    }
    Ok(selected.into_values().collect())
}

fn check_outline_projection(
    package: backend_library::PackageKey,
    tree: &backend_library::Outline,
    snapshot: &ViewSnapshot,
) -> Result<(), ClientError> {
    if tree.package != package {
        return Err(ClientError::Protocol(
            "outline package does not match the requested name projection".to_owned(),
        ));
    }
    let Some(producer) = tree.source_basis() else {
        return Err(ClientError::Protocol(
            "outline omits its producer source basis; name rows cannot be joined safely".to_owned(),
        ));
    };
    let source = snapshot.root.basis();
    if tree.basis() != source.root || producer != source {
        return Err(ClientError::CommandFailed(
            backend_library::CommandFailure::WrongBasis {
                expected: source.root.into(),
                observed: tree.basis().into(),
            },
        ));
    }
    Ok(())
}

fn outline_snapshot(engine: &mut dyn Engine, path: &str) -> Result<ViewSnapshot, ClientError> {
    let reply = engine.probe(Probe::OutlinePage {
        path,
        limit: PROBE_ROWS,
    })?;
    match reply.reply {
        CommandReply::ProjectionPage(page) => Ok(page.snapshot),
        CommandReply::Packages(snapshot)
        | CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => Ok(snapshot),
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

#[cfg(test)]
mod readiness_controls {
    #![allow(clippy::expect_used)]
    use super::*;
    use backend_library::{
        Command, CommandFailure, Library, OutlineQuery, PageRequest, QueryLimit, package_key,
    };

    #[test]
    fn outline_projection_refuses_a_changed_selected_root_or_source_frontier() {
        let library = Library::new();
        let snapshot = library
            .packages_page(PageRequest::new(
                library.revision_root(),
                QueryLimit::default(),
            ))
            .expect("actual projection")
            .snapshot;
        let basis = snapshot.root.basis();
        let mut tree = backend_library::Outline::new(
            package_key("/gymbro"),
            library.revision_root(),
            backend_library::OutlineNode {
                symbol: backend_library::symbol_key("WeekService"),
                children: Box::new([]),
            },
        )
        .with_source_basis(basis);
        assert!(check_outline_projection(package_key("/gymbro"), &tree, &snapshot).is_ok());
        tree.package = package_key("/foreign");
        assert!(matches!(
            check_outline_projection(package_key("/gymbro"), &tree, &snapshot),
            Err(ClientError::Protocol(_))
        ));
        tree.package = package_key("/gymbro");
        tree.source = None;
        assert!(matches!(
            check_outline_projection(package_key("/gymbro"), &tree, &snapshot),
            Err(ClientError::Protocol(_))
        ));
        tree.source = Some(basis);
        tree.basis = backend_library::view_state_root(&[("foreign".to_owned(), "root".to_owned())]);
        assert!(matches!(
            check_outline_projection(package_key("/gymbro"), &tree, &snapshot),
            Err(ClientError::CommandFailed(
                CommandFailure::WrongBasis { .. }
            ))
        ));
        tree.basis = library.revision_root();
        tree.source = Some(backend_library::Basis {
            root: tree.basis,
            ..basis
        });
        // Even with the same visible root, a different branch/log/schema/source
        // object must not be joined. Change the producer object, not its rows.
        tree.source.as_mut().expect("source").object =
            backend_library::object_version(b"foreign producer");
        assert!(matches!(
            check_outline_projection(package_key("/gymbro"), &tree, &snapshot),
            Err(ClientError::CommandFailed(
                CommandFailure::WrongBasis { .. }
            ))
        ));
    }

    #[test]
    fn an_actual_absent_outline_retains_not_found_and_an_exact_reindex_action() {
        let library = Library::new();
        let path = "/workspace/not-yet-published";
        let query = OutlineQuery::new(package_key(path), library.revision_root());
        let error = library
            .execute(Command::Outline(query))
            .expect_err("package is absent in the actual catalog");
        let failure = CommandFailure::from(error);
        assert_eq!(failure, CommandFailure::NotFound);
        let fault = next_step(
            Fault::from_client_error(
                &ClientError::CommandFailed(failure),
                Operand::Path(path.to_owned()),
            ),
            Probe::Outline(path),
        );
        assert_eq!(fault.slug(), crate::FaultSlug::NotFound);
        assert_eq!(fault.cause().slug(), crate::CauseSlug::Absent);
        assert_eq!(fault.operand().render(), path);
        assert_eq!(
            fault.affordance().tool_call().expect("index action"),
            serde_json::json!({"name":"backend.index", "arguments":{"path":path}})
        );
        assert!(fault.cause().sentence().contains("no outline is published"));
        let page_error = library
            .outline_page(
                package_key(path),
                PageRequest::new(library.revision_root(), QueryLimit::default()),
            )
            .expect_err("flat page has the same real absence");
        assert_eq!(CommandFailure::from(page_error), CommandFailure::NotFound);
    }

    #[test]
    fn a_corrupt_or_stale_outline_is_never_relabelled_as_missing_publication() {
        let path = "/workspace/project";
        for failure in [
            CommandFailure::IncoherentView("retained relation proof is corrupt".to_owned()),
            CommandFailure::CursorMismatch,
        ] {
            let fault = Fault::from_client_error(
                &ClientError::CommandFailed(failure),
                Operand::Path(path.to_owned()),
            );
            for probe in [
                Probe::Outline(path),
                Probe::OutlinePage { path, limit: 200 },
            ] {
                let observed = next_step(fault.clone(), probe);
                assert_eq!(observed, fault);
                assert!(!matches!(observed.affordance(), Affordance::Reindex { .. }));
            }
        }
    }
}

#[cfg(test)]
mod document_member_paging_controls {
    use super::*;
    use backend_library::{
        Basis, Coverage, Cursor, DeclarationKind, Document, DocumentQuery, Frontier, HealthReport,
        Library, PageRequest, PageTerminal, ProjectionPage, QueryLimit, RowId, SourceLocation,
        ViewRoot, object_version, package_key, symbol_key, view_key, view_state_root,
    };

    const PATH: &str = "/workspace/constant-members";
    const OWNER: &str = "/workspace/constant-members::scheduler.ts:88::generateSchedule";
    const NAMES: [&str; 5] = [
        "allSections",
        "missingSections",
        "wanted",
        "busyBlocks",
        "sectionsByClass",
    ];

    fn catalog() -> Library {
        let basis = Basis::new(
            view_state_root(&[(PATH.to_owned(), "retained member observation".to_owned())]),
            object_version(b"structural source"),
        );
        let package = package_key(PATH);
        let owner = symbol_key(OWNER);
        let mut rows = vec![
            Row::in_package(RowId::Package(package), basis, package, PATH),
            Row::in_package(RowId::Symbol(owner), basis, package, OWNER)
                .with_kind(DeclarationKind::Function)
                .with_source(SourceLocation::new("scheduler.ts", 88).expect("source location")),
        ];
        for (index, name) in NAMES.iter().enumerate() {
            let coordinate = format!("{OWNER}::{name}");
            rows.push(
                Row::in_package(
                    RowId::Symbol(symbol_key(&coordinate)),
                    basis,
                    package,
                    coordinate,
                )
                .with_parent(owner)
                .with_kind(DeclarationKind::Constant)
                .with_source(
                    SourceLocation::new(
                        "scheduler.ts",
                        91 + u32::try_from(index).expect("small fixture"),
                    )
                    .expect("source location"),
                ),
            );
        }
        rows.extend((0..532).map(|index| {
            let coordinate = format!("{PATH}::a-unrelated.ts:{}::function_{index}", index + 1);
            Row::in_package(
                RowId::Symbol(symbol_key(&coordinate)),
                basis,
                package,
                coordinate,
            )
            .with_kind(DeclarationKind::Function)
            .with_source(SourceLocation::new("a-unrelated.ts", 1).expect("source location"))
        }));
        let root = ViewRoot::new_incomplete(
            view_key(b"member pagination control"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 17),
            rows,
            vec![Coverage::Unavailable {
                lane: backend_library::Lane::Exact,
                reason: backend_library::Reason::NoIndex,
            }],
        )
        .expect("retained rows never claim native/source completeness");
        Library::from_view(root.clone(), Cursor::for_view_root(&root))
            .expect("actual immutable catalog")
    }

    #[derive(Clone, Copy, Default)]
    enum Disturbance {
        #[default]
        None,
        Cancel,
        ChangedBasis,
        ChangedFrontier,
        RepeatedCursor,
        WrongTerminal,
        ForeignMember,
    }

    struct Reader {
        catalog: Library,
        disturbance: Disturbance,
        calls: usize,
        cursors: Vec<Option<PageContinuation>>,
    }
    impl Reader {
        fn new(disturbance: Disturbance) -> Self {
            Self {
                catalog: catalog(),
                disturbance,
                calls: 0,
                cursors: Vec::new(),
            }
        }
        fn document(&self) -> Document {
            self.catalog
                .document(DocumentQuery::selected(
                    symbol_key(OWNER),
                    self.catalog.revision_root(),
                ))
                .expect("real selected document")
        }
    }
    impl Engine for Reader {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            Ok(self.catalog.revision_root())
        }
        fn health(&mut self) -> Result<HealthReport, ClientError> {
            Ok(HealthReport::from_root(
                self.catalog.view(),
                self.catalog.cursor(),
            ))
        }
        fn surface(&mut self, _: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            Err(ClientError::Protocol("not a mutation fixture".to_owned()))
        }
        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            match probe {
                Probe::Document(OWNER) => {
                    Ok(ReplyDto::new(1, CommandReply::Document(self.document())))
                }
                Probe::Related(_) => Err(ClientError::Protocol(
                    "this fixture proves members, not graph authority".to_owned(),
                )),
                _ => self.probe_page(probe, None),
            }
        }
        fn probe_page(
            &mut self,
            probe: Probe<'_>,
            continuation: Option<PageContinuation>,
        ) -> Result<ReplyDto, ClientError> {
            let Probe::OutlinePage { path, limit } = probe else {
                return Err(ClientError::Protocol("wrong probe".to_owned()));
            };
            assert_eq!(path, PATH);
            assert_eq!(limit, PROBE_ROWS);
            self.calls += 1;
            self.cursors.push(continuation);
            if self.calls == 2 && matches!(self.disturbance, Disturbance::Cancel) {
                return Err(ClientError::RequestCancelled(
                    backend_client::RequestCancellationReason::Explicit,
                ));
            }
            let request = PageRequest::new(
                self.catalog.revision_root(),
                QueryLimit::new(limit).expect("exact credit"),
            );
            let request = continuation.map_or(request, |cursor| request.with_continuation(cursor));
            let mut page: ProjectionPage = self
                .catalog
                .outline_page(package_key(path), request)
                .expect("actual native-style page selector");
            if self.calls == 2 {
                match self.disturbance {
                    Disturbance::ChangedBasis
                    | Disturbance::ChangedFrontier
                    | Disturbance::ForeignMember => {
                        let mut basis = page.snapshot.root.basis();
                        let mut rows = page.snapshot.root.rows().to_vec();
                        let mut frontier = page.snapshot.root.frontier();
                        if matches!(self.disturbance, Disturbance::ChangedBasis) {
                            basis.object =
                                object_version(b"another source object with the same root");
                            for row in &mut rows {
                                row.basis = basis;
                            }
                        }
                        if matches!(self.disturbance, Disturbance::ChangedFrontier) {
                            frontier = Frontier::new(
                                basis.branch,
                                basis.log,
                                basis.schema,
                                basis.root,
                                frontier.sequence + 1,
                            );
                        }
                        if matches!(self.disturbance, Disturbance::ForeignMember) {
                            let foreign = package_key("/foreign");
                            rows[0] = Row::in_package(
                                RowId::Symbol(symbol_key("/foreign::child")),
                                basis,
                                foreign,
                                "/foreign::child",
                            )
                            .with_parent(symbol_key(OWNER));
                        }
                        page.snapshot.root = ViewRoot::new_incomplete(
                            page.snapshot.root.recipe(),
                            basis,
                            frontier,
                            rows,
                            page.snapshot.root.coverage().to_vec(),
                        )
                        .expect("distinct fixture observation");
                    }
                    Disturbance::RepeatedCursor => {
                        page.snapshot.next = continuation.map(PageContinuation::cursor);
                        page.terminal =
                            PageTerminal::More(continuation.expect("real preceding owner cursor"));
                    }
                    Disturbance::WrongTerminal => page.terminal = PageTerminal::Complete,
                    Disturbance::None | Disturbance::Cancel => {}
                }
            }
            Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)))
        }
    }

    #[test]
    fn document_members_after_package_prefix_are_complete_without_unrelated_rows() {
        let mut reader = Reader::new(Disturbance::None);
        let document = reader.document();
        let first = reader
            .catalog
            .outline_page(
                package_key(PATH),
                PageRequest::new(
                    reader.catalog.revision_root(),
                    QueryLimit::new(200).expect("credit"),
                ),
            )
            .expect("original first-page witness");
        assert!(first.snapshot.next.is_some());
        assert!(
            first
                .snapshot
                .root
                .rows()
                .iter()
                .filter(|row| row.parent == Some(document.symbol))
                .count()
                < NAMES.len()
        );
        let rows =
            document_member_rows(&mut reader, PATH, &document).expect("all retained member pages");
        assert_eq!(reader.calls, 3);
        assert_eq!(
            rows.len(),
            NAMES.len() + 1,
            "only owner and direct members are retained"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.parent == Some(document.symbol))
                .map(|row| Identity::parse(&row.label).name().to_owned())
                .collect::<std::collections::BTreeSet<_>>(),
            NAMES.map(str::to_owned).into_iter().collect()
        );
        assert_eq!(reader.cursors[0], None);
        assert!(reader.cursors[1..].iter().all(Option::is_some));
        assert_eq!(
            reader.catalog.view().coverage().len(),
            1,
            "incomplete source coverage was not promoted"
        );
        let page = page_from_document(OWNER, &document, &rows, &[], Vec::new());
        assert_eq!(
            page.members()
                .iter()
                .flat_map(|group| group.members())
                .count(),
            5
        );
    }

    #[test]
    fn legacy_root_only_document_pins_first_page_object_before_reading_members() {
        for disturbance in [Disturbance::None, Disturbance::ChangedBasis] {
            let mut reader = Reader::new(disturbance);
            let selected = reader.document();
            let document = Document::new(selected.symbol, selected.basis, []);
            assert_eq!(document.source_basis(), None);
            let mut notes = Vec::new();
            let rows = probe_members(&mut reader, OWNER, &document, &mut notes);
            if matches!(disturbance, Disturbance::None) {
                assert_eq!(rows.len(), NAMES.len() + 1);
                assert_eq!(reader.calls, 3);
                assert!(notes.is_empty());
            } else {
                assert!(rows.is_empty(), "a changed object cannot install a prefix");
                assert_eq!(reader.calls, 2);
                assert_eq!(notes.len(), 1);
                assert_eq!(notes[0].slug(), crate::FaultSlug::Protocol);
            }
        }
    }

    #[test]
    fn incomplete_member_walk_never_installs_a_partial_count_or_rebinds_source() {
        for disturbance in [
            Disturbance::Cancel,
            Disturbance::ChangedBasis,
            Disturbance::ChangedFrontier,
            Disturbance::RepeatedCursor,
            Disturbance::WrongTerminal,
            Disturbance::ForeignMember,
        ] {
            let mut reader = Reader::new(disturbance);
            let document = reader.document();
            let mut notes = Vec::new();
            let rows = probe_members(&mut reader, OWNER, &document, &mut notes);
            assert!(rows.is_empty(), "failed walk cannot publish a prefix");
            assert_eq!(
                notes.len(),
                1,
                "the omitted section has an actual typed reason"
            );
            assert_eq!(
                reader.calls, 2,
                "no status retry, restart or next source selection"
            );
            assert_eq!(reader.cursors[0], None);
            assert!(
                reader.cursors[1].is_some(),
                "exact original owner cursor was forwarded"
            );
        }
    }
}

#[cfg(test)]
mod address_resolution_controls {
    use super::*;
    use backend_library::{
        Basis, Coverage, Cursor, DeclarationKind, Document, DocumentQuery, Frontier, HealthReport,
        Library, NameQuery, QueryLimit, RowId, SourceLocation, ViewRoot, object_version,
        package_key, symbol_key, view_key, view_state_root,
    };

    const PROJECT: &str = "/workspace/next-asp";
    const SCHEDULER: &str = "/workspace/next-asp::src/lib/scheduler.ts:21::generateSchedule";
    const SEMANTIC: &str = "/workspace/next-asp::semantic::417a1b7eff7c444eb114d2b0ce7c4a2eecb261696ee43ad62d64ff06cb30f536::generateSchedule";
    const IMPORT: &str = "/workspace/next-asp::semantic::1d2aa0501a247d84ae8e273f5889ad486c58efea5ec2e049ea35445fb29cad66::generateSchedule";
    const OTHER: &str = "/workspace/next-asp::src/legacy/scheduler.ts:7::generateSchedule";
    const EXAMPLE: &str = "/workspace/next-asp::examples/demo.ts:3::generateSchedule";
    const UNIQUE: &str = "/workspace/next-asp::src/lib/scheduler.ts:40::scoreOf";
    const SCORE_SEMANTIC: &str = "/workspace/next-asp::semantic::aa2d7a6b38c24be3fdd184f348b8b6c2f004655b1ed34167622f4940acb9ba63::scoreOf";

    fn external(display: &str) -> String {
        format!("{PROJECT}::external::symbol:{}::{display}", "3a".repeat(32))
    }

    fn catalog(with_duplicate_name: bool, with_example: bool) -> Library {
        let basis = Basis::new(
            view_state_root(&[(PROJECT.to_owned(), "twin observation".to_owned())]),
            object_version(b"twin source"),
        );
        let package = package_key(PROJECT);
        let site = |path: &str, line| SourceLocation::new(path, line).expect("source location");
        let declared = |label: &str, kind, path: &str, line| {
            Row::in_package(RowId::Symbol(symbol_key(label)), basis, package, label)
                .with_kind(kind)
                .with_source(site(path, line))
        };
        let mut rows = vec![
            Row::in_package(RowId::Package(package), basis, package, PROJECT),
            declared(
                SCHEDULER,
                DeclarationKind::Function,
                "src/lib/scheduler.ts",
                21,
            )
            .with_signature("export function generateSchedule(input: SchedulerInput): Schedule[]"),
            declared(
                SEMANTIC,
                DeclarationKind::Function,
                "src/lib/scheduler.ts",
                21,
            )
            .with_signature("export function generateSchedule(input: SchedulerInput): Schedule[]"),
            declared(IMPORT, DeclarationKind::Import, "src/app/page.tsx", 2)
                .with_signature("value\n@/lib/scheduler\ngenerateSchedule"),
            declared(
                UNIQUE,
                DeclarationKind::Function,
                "src/lib/scheduler.ts",
                40,
            ),
            declared(
                SCORE_SEMANTIC,
                DeclarationKind::Function,
                "src/lib/scheduler.ts",
                40,
            ),
        ];
        for display in ["generateSchedule", "get", "length"] {
            let label = external(display);
            rows.push(
                Row::new(RowId::Symbol(symbol_key(&label)), basis, label)
                    .with_document(Box::<[backend_library::Fragment]>::default()),
            );
        }
        if with_duplicate_name {
            rows.push(declared(
                OTHER,
                DeclarationKind::Function,
                "src/legacy/scheduler.ts",
                7,
            ));
        }
        if with_example {
            rows.push(declared(
                EXAMPLE,
                DeclarationKind::Function,
                "examples/demo.ts",
                3,
            ));
        }
        let root = ViewRoot::new_incomplete(
            view_key(b"twin address resolution"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 3),
            rows,
            vec![Coverage::Unavailable {
                lane: backend_library::Lane::Exact,
                reason: backend_library::Reason::NoIndex,
            }],
        )
        .expect("retained rows never claim native/source completeness");
        Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("actual catalog")
    }

    struct Resolver {
        catalog: Library,
        probes: Vec<String>,
    }

    impl Resolver {
        fn new(with_duplicate_name: bool) -> Self {
            Self {
                catalog: catalog(with_duplicate_name, false),
                probes: Vec::new(),
            }
        }

        fn with_example() -> Self {
            Self {
                catalog: catalog(false, true),
                probes: Vec::new(),
            }
        }
    }

    impl Engine for Resolver {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            Ok(self.catalog.revision_root())
        }
        fn health(&mut self) -> Result<HealthReport, ClientError> {
            Ok(HealthReport::from_root(
                self.catalog.view(),
                self.catalog.cursor(),
            ))
        }
        fn surface(&mut self, _: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            Err(ClientError::Protocol("not a surface fixture".to_owned()))
        }
        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            self.probes.push(format!("{probe:?}"));
            let root = self.catalog.revision_root();
            match probe {
                Probe::Names { text, limit } => {
                    let query = NameQuery::new(text, root, QueryLimit::new(limit).expect("credit"));
                    Ok(ReplyDto::new(
                        1,
                        CommandReply::Names(
                            self.catalog
                                .names(&query)
                                .map_err(|error| ClientError::CommandFailed(error.into()))?,
                        ),
                    ))
                }
                Probe::Document(at) | Probe::Source(at) => {
                    let document = self
                        .catalog
                        .document(DocumentQuery::new(symbol_key(at), root))
                        .map_err(|error| ClientError::CommandFailed(error.into()))?;
                    Ok(ReplyDto::new(1, CommandReply::Document(document)))
                }
                Probe::Graph(at) | Probe::Related(at) => {
                    let snapshot = self
                        .catalog
                        .graph(backend_library::GraphNeighborhoodQuery::new(
                            symbol_key(at),
                            root,
                        ))
                        .map_err(|error| ClientError::CommandFailed(error.into()))?;
                    Ok(ReplyDto::new(1, CommandReply::Graph(snapshot)))
                }
                _ => Err(ClientError::Protocol("unexpected probe".to_owned())),
            }
        }
    }

    fn resolve(engine: &mut Resolver, text: &str) -> Result<String, Fault> {
        declaration_operand(engine, text, "show")
    }

    #[test]
    fn complete_coordinates_pass_through_without_a_probe() {
        let mut engine = Resolver::new(false);
        for coordinate in [SCHEDULER, SEMANTIC, "pkg:cargo/serde@1.0::src/lib.rs:1::x"] {
            assert_eq!(resolve(&mut engine, coordinate).as_deref(), Ok(coordinate));
        }
        assert!(engine.probes.is_empty(), "{:?}", engine.probes);
    }

    #[test]
    fn a_bare_name_resolves_to_the_one_declaration_after_merging_lanes_and_setting_aside_imports() {
        let mut engine = Resolver::new(false);
        // The structural row, its compiler-backed twin, an import of the name
        // and two external placeholders all carry `generateSchedule`; only
        // one is a declaration.
        assert_eq!(
            resolve(&mut engine, "generateSchedule").as_deref(),
            Ok(SCHEDULER)
        );
        assert_eq!(resolve(&mut engine, "scoreOf").as_deref(), Ok(UNIQUE));
    }

    #[test]
    fn a_file_qualified_name_narrows_to_the_matching_file_and_line() {
        let mut engine = Resolver::new(true);
        for text in [
            "scheduler.ts:21::generateSchedule",
            "lib/scheduler.ts::generateSchedule",
            "src/lib/scheduler.ts:21::generateSchedule",
        ] {
            assert_eq!(
                resolve(&mut engine, text).as_deref(),
                Ok(SCHEDULER),
                "{text}"
            );
        }
        assert_eq!(
            resolve(&mut engine, "legacy/scheduler.ts::generateSchedule").as_deref(),
            Ok(OTHER)
        );
    }

    #[test]
    fn the_one_library_definition_is_the_name_even_when_an_example_repeats_it() {
        let mut engine = Resolver::with_example();
        assert_eq!(
            resolve(&mut engine, "generateSchedule").as_deref(),
            Ok(SCHEDULER),
            "an example's function of the same name does not make the library one ambiguous"
        );
        // The example stays reachable by its own coordinate and as a file-qualified name.
        assert_eq!(
            resolve(&mut engine, "examples/demo.ts::generateSchedule").as_deref(),
            Ok(EXAMPLE)
        );
        // Two library definitions are still ambiguous, with the library ones listed first.
        let mut both = Resolver::new(true);
        let fault = resolve(&mut both, "generateSchedule").expect_err("two library definitions");
        assert_eq!(fault.slug(), crate::FaultSlug::Ambiguous);
    }

    #[test]
    fn several_declarations_are_refused_with_one_exact_retry_each() {
        let mut engine = Resolver::new(true);
        let fault = resolve(&mut engine, "generateSchedule").expect_err("two declarations");
        assert_eq!(fault.slug(), crate::FaultSlug::Ambiguous);
        let mut coordinates = fault
            .candidates()
            .iter()
            .map(|candidate| candidate.coordinate().as_str().to_owned())
            .collect::<Vec<_>>();
        coordinates.sort();
        assert_eq!(coordinates, vec![OTHER.to_owned(), SCHEDULER.to_owned()]);
        for candidate in fault.candidates() {
            assert_eq!(candidate.kind(), Some(DeclarationKind::Function));
            let shell = candidate.shell().expect("shell retry");
            assert_eq!(
                shell,
                format!("nudox show {}", candidate.coordinate().as_str())
            );
            let call = candidate.tool_call().expect("tool retry");
            assert_eq!(
                call["arguments"]["coordinate"],
                candidate.coordinate().as_str()
            );
        }
        assert!(
            fault
                .candidates()
                .iter()
                .any(|candidate| { candidate.location() == "src/lib/scheduler.ts:21" })
        );
    }

    #[test]
    fn an_unknown_name_is_refused_with_the_nearest_declarations() {
        let mut engine = Resolver::new(false);
        let fault = resolve(&mut engine, "generateSchedul").expect_err("no exact declaration");
        assert_eq!(fault.slug(), crate::FaultSlug::NotFound);
        assert!(
            fault
                .candidates()
                .iter()
                .any(|candidate| candidate.coordinate().as_str() == SCHEDULER),
            "{:?}",
            fault.candidates()
        );
        let nothing = resolve(&mut engine, "zzzNothing").expect_err("nothing near");
        assert_eq!(nothing.slug(), crate::FaultSlug::NotFound);
        assert!(nothing.candidates().is_empty());
        assert!(matches!(
            nothing.affordance(),
            Affordance::UseCommand { name: "search", .. }
        ));
    }

    #[test]
    fn both_spellings_show_the_same_declaration_under_its_structural_coordinate() {
        // The fixture's engine cannot resolve a compiler-backed label (that
        // is the owner's job), so the structural document stands in for the
        // reply the owner returns for either spelling.
        let mut engine = Resolver::new(false);
        let reply = engine
            .probe(Probe::Document(SCHEDULER))
            .expect("structural document");
        let CommandReply::Document(document) = reply.reply else {
            panic!("document");
        };
        assert_eq!(canonical_coordinate(SEMANTIC, &document), SCHEDULER);
        assert_eq!(canonical_coordinate(SCHEDULER, &document), SCHEDULER);
        // A compiler-backed declaration with no structural row keeps its own
        // coordinate: the key of the derived spelling is not the document's.
        let lone = Document::new(symbol_key("a compiler-only row"), document.basis, [])
            .with_location(document.location.clone());
        assert_eq!(canonical_coordinate(SEMANTIC, &lone), SEMANTIC);
    }

    #[test]
    fn name_resolution_misses_name_the_declarations_that_share_the_name() {
        let mut engine = Resolver::new(false);
        let stale = "/workspace/next-asp::src/lib/scheduler.ts:20::generateSchedule";
        let fault = read_declaration(&mut engine, Probe::Document(stale), stale, "show")
            .expect_err("stale line");
        assert_eq!(fault.slug(), crate::FaultSlug::NotFound);
        assert_eq!(
            fault
                .candidates()
                .iter()
                .map(|candidate| candidate.coordinate().as_str())
                .collect::<Vec<_>>(),
            vec![SCHEDULER],
            "the stale coordinate points at where the declaration is now"
        );
    }
}
