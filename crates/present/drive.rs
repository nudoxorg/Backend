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
//! A failed section probe remains a typed note on the useful page. Required
//! read failures also classify that composite answer as failed; optional
//! source/lane availability remains evidence about the available scope.

use crate::assemble::{
    outline_tree, page_from_document, page_from_document_with_graph_relations, record_list,
    shelf_from_snapshot, shelf_from_snapshot_with_project_coverage,
};
use crate::call::Request;
use crate::fault::{Affordance, Fault, Operand};
use crate::identity::{Coordinate, Identity, KeyTag};
use crate::outline::OutlineTree;
use crate::page::Page;
use crate::product::{ProductRecord, ProductView};
use crate::project_coverage::read_current_project_coverage;
use crate::record::RecordList;
use crate::shelf::Shelf;
use crate::status::Status;
use backend_client::ClientError;
use backend_library::{
    CommandReply, CompileExecutionIntent, DeclarationRead, DeclarationSelector, GraphRelation,
    HealthReport, IndexSearchCursor, IntentId, PageContinuation, ReplyDto, Row, SurfaceCommand,
    SurfaceReply, ViewSnapshot, ViewStateRoot,
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
    /// Root-bound native compiler-generation discovery cursor.
    SemanticVersions(backend_library::SemanticVersionsCursor),
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
    /// One exact row addressed by its untrusted public selector.
    Declaration {
        /// Closed declaration operand.
        selector: &'a DeclarationSelector,
        /// Requested declaration capability.
        read: DeclarationRead,
    },
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
            Self::Declaration { selector, .. } => Operand::Argument(declaration_operand(selector)),
        }
    }
}

/// Whatever a surface talks to in order to read an admitted reply.
///
/// The one implementation in production forwards to a connected local daemon
/// session; the implementations in tests return canned library values. Nothing
/// in this crate knows which it is holding.
pub trait Engine {
    /// Pages one admitted document's immediate members without walking its
    /// package. Older engines explicitly refuse; incomplete/unsupported member
    /// reads remain composite faults, never empty successful membership.
    fn members_page(
        &mut self,
        _document: &backend_library::Document,
        _limit: u16,
        _continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        Err(ClientError::Protocol(
            "engine does not expose direct member pages".to_owned(),
        ))
    }

    /// Reads an owner-certified graph delivery window for this exact declaration.
    /// Unsupported engines refuse rather than returning an unbounded or empty result.
    fn declaration_graph_window(
        &mut self,
        _selector: &DeclarationSelector,
        _window: backend_library::QueryWindow,
        _continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        Err(ClientError::Protocol(
            "engine does not expose native graph delivery windows".to_owned(),
        ))
    }

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

    /// Reads a native catalog window without changing its issued query credit.
    /// Unsupported engines reject the new contract rather than resizing a cursor.
    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: backend_library::QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        if window.is_windowed() {
            return Err(ClientError::Protocol(
                "engine does not expose native catalog delivery windows".to_owned(),
            ));
        }
        self.probe_page(probe, continuation)
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
    /// Changes only executable reader advice, retaining producer rows/receipts.
    #[must_use]
    pub fn with_mcp_exposure(mut self, exposure: crate::McpExposure) -> Self {
        if exposure == crate::McpExposure::Direct {
            return self;
        }
        match &mut self {
            Self::Page(page) => page.expose_mcp_advice(exposure),
            Self::Shelf(shelf) => shelf.expose_mcp_advice(exposure),
            Self::Product(product) => product.expose_mcp_advice(exposure),
            Self::Status(status) => status.expose_mcp_advice(exposure),
            Self::Records(_) | Self::Outline(_) => {}
        }
        self
    }
    /// Returns the shared failure carried by an otherwise admitted answer.
    /// The full answer remains available, including exact owner receipts.
    #[must_use]
    pub fn fault(&self) -> Option<&Fault> {
        match self {
            Self::Product(product) => product.fault(),
            Self::Page(page) => page.fault(),
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
                Request::Search { .. }
                    | Request::Resolve { .. }
                    | Request::OutlinePage { .. }
                    | Request::GraphPage { .. }
            ) {
                return Err(Fault::usage(
                    "cursor",
                    "only search, names, bounded outlines and bounded graphs have portable query continuations",
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
        Request::Page(at) => page(engine, at, None).map(Box::new).map(Answer::Page),
        Request::ResumePage { selector, cursor } => {
            if selector.selected_id().is_none() {
                return Err(Fault::usage(
                    "cursor",
                    "document continuation requires the original full selected row ID",
                ));
            }
            selected_declaration(engine, selector, DeclarationRead::Document, Some(cursor))
        }
        Request::Source(at) => source(engine, at),
        Request::Declaration { selector, read } => {
            selected_declaration(engine, selector, *read, None)
        }
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
            let (snapshot, semantic_search, published_names) = search_snapshot(reply, "search")?;
            let list = record_list(text, &snapshot);
            let list = if let Some(status) = semantic_search {
                list.with_semantic_search_status(status)
            } else {
                list
            };
            let list = if let Some(readiness) = published_names {
                list.with_published_names(readiness)
            } else {
                list
            };
            let list = if list.is_empty() {
                list.with_empty_reason(
                    if published_names.is_some() {
                        "no current declaration names matched; full lexical search has not been queried in this result stage"
                    } else {
                        "no indexed declarations matched this query text; try a declaration name, signature fragment, or documentation keyword"
                    },
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
        Request::OutlinePage { .. } | Request::GraphPage { .. } => {
            answer_paged(engine, request, None)
        }
        Request::Surface(command) => surface(engine, command),
    }
}

/// Projects an already admitted native search/name/outline reply without another read.
/// Surfaces can retain typed response-ceiling errors before this projection.
///
/// # Errors
/// Returns the existing catalog projection fault for a changed reply shape or failed command.
pub fn catalog_page_answer(request: &Request, reply: ReplyDto) -> Result<Answer, Fault> {
    match request {
        Request::Search { text, .. } => {
            let (snapshot, semantic_search, published_names) = search_snapshot(reply, "search")?;
            let list = record_list(text, &snapshot);
            let list = if let Some(status) = semantic_search {
                list.with_semantic_search_status(status)
            } else {
                list
            };
            let list = if let Some(readiness) = published_names {
                list.with_published_names(readiness)
            } else {
                list
            };
            let list = if list.is_empty() {
                list.with_empty_reason(if published_names.is_some() {
                    "no current declaration names matched; full lexical search has not been queried in this result stage"
                } else { "no indexed declarations matched this query text; try a declaration name, signature fragment, or documentation keyword" })
            } else {
                list
            };
            Ok(Answer::Records(Box::new(list)))
        }
        Request::Resolve { text, .. } => {
            let CommandReply::Names(snapshot) = reply.reply else {
                return Err(shape("resolve"));
            };
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::GraphPage { selector, .. } => {
            let CommandReply::ProjectionPage(page) = reply.reply else {
                return Err(shape("graph-page"));
            };
            if page.terminal
                != page
                    .snapshot
                    .next
                    .map_or(backend_library::PageTerminal::Complete, |cursor| {
                        backend_library::PageTerminal::More(PageContinuation::from_cursor(cursor))
                    })
            {
                return Err(shape("graph-page terminal"));
            }
            let heading = match selector {
                DeclarationSelector::Coordinate(value)
                | DeclarationSelector::SelectedAt {
                    coordinate: value, ..
                } => value.clone(),
                DeclarationSelector::Selected(id) => backend_library::encode_id(id.bytes()),
            };
            let records = record_list(&heading, &page.snapshot);
            if records.page_basis().is_none() {
                return Err(shape("graph-page source basis"));
            }
            Ok(Answer::Records(Box::new(records)))
        }
        Request::OutlinePage { path, limit } => {
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
            let order = page
                .outline_order
                .as_ref()
                .ok_or_else(|| shape("outline-page order"))?;
            crate::outline::page_tree(path, &page.snapshot, *limit, order)
                .map(Box::new)
                .map(Answer::Outline)
                .map_err(|error| Fault::from_client_error(&error, Operand::Path(path.clone())))
        }
        _ => Err(shape("catalog window request")),
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
            let (snapshot, semantic_search, published_names) = search_snapshot(reply, "search")?;
            let list = record_list(text, &snapshot);
            let list = if let Some(status) = semantic_search {
                list.with_semantic_search_status(status)
            } else {
                list
            };
            let list = if let Some(readiness) = published_names {
                list.with_published_names(readiness)
            } else {
                list
            };
            let list = if list.is_empty() {
                list.with_empty_reason(
                    if published_names.is_some() {
                        "no current declaration names matched; full lexical search has not been queried in this result stage"
                    } else {
                        "no indexed declarations matched this query text; try a declaration name, signature fragment, or documentation keyword"
                    },
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
        Request::GraphPage { selector, limit } => {
            let selector = match selector {
                DeclarationSelector::Coordinate(value) => {
                    DeclarationSelector::Coordinate(resolve_declaration(engine, value, "graph")?)
                }
                selected => selected.clone(),
            };
            let credit = backend_library::QueryLimit::new(*limit)
                .ok_or_else(|| Fault::usage("limit", "invalid graph query credit"))?;
            let reply = engine
                .declaration_graph_window(
                    &selector,
                    backend_library::QueryWindow::fixed(credit),
                    continuation,
                )
                .map_err(|error| {
                    probe_fault(
                        &error,
                        Probe::Declaration {
                            selector: &selector,
                            read: DeclarationRead::Graph,
                        },
                    )
                })?;
            catalog_page_answer(
                &Request::GraphPage {
                    selector,
                    limit: *limit,
                },
                reply,
            )
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
            catalog_page_answer(request, reply)
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
    let health = engine.health().ok();
    let shelf = match health.as_ref() {
        Some(health) => {
            let project_coverage = read_current_project_coverage(engine, &snapshot, health);
            shelf_from_snapshot_with_project_coverage(&snapshot, tag, health, &project_coverage)
        }
        None => shelf_from_snapshot(&snapshot, tag),
    };
    let causes = health.as_ref().map_or_else(Default::default, |health| {
        crate::compiler_causes::failed_package_causes(engine, &snapshot, health)
    });
    Ok(Answer::Shelf(Box::new(shelf.with_compiler_causes(&causes))))
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
    let mut status = Status::from_health_reply(&report, None);
    // Preserve the one-health-read fast path for ready, partial, moving and
    // cancelled lanes. Only an admitted terminal native refusal asks for a
    // bounded failed-package sample; this never requests compiler work.
    if report.coverage().iter().any(|coverage| {
        matches!(
            coverage,
            backend_library::Coverage::Unavailable {
                lane: backend_library::Lane::Semantic,
                reason: backend_library::Reason::Incomplete | backend_library::Reason::Unconfigured,
            }
        )
    }) && let Ok(reply) = read(engine, Probe::Packages)
        && let CommandReply::Packages(snapshot) = reply.reply
        && crate::compiler_causes::snapshot_matches_health(&snapshot, &report)
    {
        let cause = crate::compiler_causes::failed_package_causes(engine, &snapshot, &report)
            .into_values()
            .next();
        status = status.with_compiler_refusal(cause);
    }
    Ok(Answer::Status(Box::new(status)))
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

/// Resolves a coordinate command's operand through the same admitted names
/// projection used by `resolve`. Exact coordinates keep their native identity.
/// A bounded page never certifies uniqueness while it has a continuation.
///
/// # Errors
/// Returns an absent or ambiguous fault, or the existing native read failure.
pub fn resolve_declaration(
    engine: &mut dyn Engine,
    text: &str,
    command: &'static str,
) -> Result<String, Fault> {
    if text.starts_with('/')
        || text.starts_with("pkg:")
        || matches!(text.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
    {
        return Ok(text.to_owned());
    }
    let (qualifier, name) = text.rsplit_once("::").unwrap_or(("", text));
    let (file, line) = qualifier
        .rsplit_once(':')
        .map_or((qualifier, None), |(file, line)| {
            line.parse::<u32>()
                .ok()
                .map_or((qualifier, None), |line| (file, Some(line)))
        });
    let file_qualified = !file.is_empty()
        && crate::language::Language::from_path(&crate::identity::PackagePath::new(file))
            != crate::language::Language::Unknown;
    let query = if file_qualified { name } else { text };
    let snapshot = snapshot(
        engine,
        Probe::Names {
            text: query,
            limit: 64,
        },
        "resolve",
    )?;
    let mut exact = Vec::new();
    let mut folded = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in snapshot.root.rows() {
        let record = crate::record::Record::from_row(row);
        let identity = record.identity();
        if !matches!(
            identity.shape(),
            crate::identity::IdentityShape::Declaration | crate::identity::IdentityShape::Semantic
        ) || record.kind() == Some(backend_library::DeclarationKind::Import)
            || !seen.insert(row.id)
        {
            continue;
        }
        if file_qualified
            && (!identity.path().is_some_and(|path| {
                path.as_str()
                    .strip_suffix(file)
                    .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('/'))
            }) || line.is_some_and(|line| {
                identity.line().map(crate::identity::LineNumber::get) != Some(line)
            }))
        {
            continue;
        }
        let actual = if query.contains("::") {
            row.label
                .rsplit_once("::")
                .map_or(row.label.as_str(), |(_, name)| name)
        } else {
            identity.name()
        };
        let exact_name =
            actual == query || (query.contains("::") && row.label.ends_with(&format!("::{query}")));
        if exact_name {
            exact.push(row.label.clone());
        } else if actual.eq_ignore_ascii_case(query) {
            folded.push(row.label.clone());
        }
    }
    let matches = if exact.is_empty() { folded } else { exact };
    match matches.as_slice() {
        [only] if snapshot.next.is_none() => Ok(only.clone()),
        [] if snapshot.next.is_none() => Err(Fault::new(
            crate::fault::FaultSlug::NotFound,
            Operand::Argument(text.to_owned()),
            crate::fault::Cause::new(
                crate::fault::CauseSlug::Absent,
                "no declaration matches this name in the selected revision",
            ),
            Affordance::UseCommand {
                name: "resolve",
                args: Box::new([text.to_owned()]),
            },
        )),
        _ => {
            let retries = matches
                .iter()
                .take(8)
                .map(|coordinate| {
                    Affordance::UseCommand {
                        name: command,
                        args: Box::new([coordinate.clone()]),
                    }
                    .shell()
                    .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("; ");
            let extent = if snapshot.next.is_some() {
                "the bounded name page does not prove a unique declaration"
            } else {
                "several distinct declarations match this name"
            };
            Err(Fault::new(
                crate::fault::FaultSlug::Ambiguous,
                Operand::Argument(text.to_owned()),
                crate::fault::Cause::new(
                    crate::fault::CauseSlug::Unproven,
                    format!("{extent}; use an exact coordinate: {retries}"),
                ),
                Affordance::UseCommand {
                    name: "resolve",
                    args: Box::new([text.to_owned()]),
                },
            ))
        }
    }
}

fn declaration_operand(selector: &DeclarationSelector) -> String {
    match selector {
        DeclarationSelector::Coordinate(value)
        | DeclarationSelector::SelectedAt {
            coordinate: value, ..
        } => value.clone(),
        DeclarationSelector::Selected(id) => backend_library::encode_id(id.bytes()),
    }
}

fn selected_declaration(
    engine: &mut dyn Engine,
    selector: &DeclarationSelector,
    kind: DeclarationRead,
    cursor: Option<&str>,
) -> Result<Answer, Fault> {
    let reply = read(
        engine,
        Probe::Declaration {
            selector,
            read: kind,
        },
    )?;
    match kind {
        DeclarationRead::Document | DeclarationRead::Source => {
            let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply
            else {
                return Err(shape("selected document"));
            };
            let coordinate = document
                .selection
                .as_ref()
                .map(|selection| selection.coordinate())
                .ok_or_else(|| shape("selected document coordinate"))?;
            let page = if kind == DeclarationRead::Document {
                let mut notes = Vec::new();
                let relations = probe_relation_rows(
                    engine,
                    Probe::Declaration {
                        selector,
                        read: DeclarationRead::Related,
                    },
                    coordinate,
                    &mut notes,
                );
                let members = probe_members(engine, coordinate, &document, &mut notes);
                page_from_document_with_graph_relations(
                    coordinate,
                    &document,
                    &members,
                    &relations.rows,
                    relations.graph_relations.as_deref().unwrap_or(&[]),
                    notes,
                )
            } else {
                page_from_document(coordinate, &document, &[], &[], Vec::new())
            };
            let page = if kind == DeclarationRead::Document {
                crate::document_sections::window(page, &document, cursor)?
            } else {
                page
            };
            if kind == DeclarationRead::Source {
                if let Some(fault) = page.source().fault() {
                    return Err(fault.clone());
                }
            }
            Ok(Answer::Page(Box::new(page)))
        }
        DeclarationRead::Graph | DeclarationRead::Related => {
            let CommandReply::Graph(snapshot) = reply.reply else {
                return Err(shape("selected graph"));
            };
            Ok(Answer::Records(Box::new(record_list(
                &declaration_operand(selector),
                &snapshot,
            ))))
        }
    }
}

fn page(engine: &mut dyn Engine, text: &str, cursor: Option<&str>) -> Result<Page, Fault> {
    let coordinate = resolve_declaration(engine, text, "show")?;
    let coordinate = coordinate.as_str();
    let reply = read(engine, Probe::Document(coordinate))?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("document"));
    };
    let mut notes = Vec::with_capacity(2);
    let relation_rows = probe_rows(engine, coordinate, &mut notes);
    let members = probe_members(engine, coordinate, &document, &mut notes);
    let page = page_from_document_with_graph_relations(
        coordinate,
        &document,
        &members,
        &relation_rows.rows,
        relation_rows.graph_relations.as_deref().unwrap_or(&[]),
        notes,
    );
    crate::document_sections::window(page, &document, cursor)
}

fn source(engine: &mut dyn Engine, text: &str) -> Result<Answer, Fault> {
    let coordinate = resolve_declaration(engine, text, "source")?;
    let coordinate = coordinate.as_str();
    let reply = read(engine, Probe::Source(coordinate))?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("source"));
    };
    let page = page_from_document(coordinate, &document, &[], &[], Vec::new());
    // Unlike a document page, this request asks specifically for source.
    // Retain the exact typed reason when no captured text can be supplied.
    if let Some(fault) = page.source().fault() {
        return Err(fault.clone());
    }
    Ok(Answer::Page(Box::new(page)))
}

fn neighbourhood(engine: &mut dyn Engine, text: &str, incoming: bool) -> Result<Answer, Fault> {
    let at = resolve_declaration(engine, text, if incoming { "related" } else { "graph" })?;
    let at = at.as_str();
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
    let list = record_list(at, &snapshot);
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
    let mut rendered = outline_tree(path, &tree, snapshot.root.rows());
    if snapshot.next.is_some() {
        rendered = rendered.with_page(&snapshot, PROBE_ROWS);
    }
    Ok(Answer::Outline(Box::new(rendered)))
}

fn surface(engine: &mut dyn Engine, command: &SurfaceCommand) -> Result<Answer, Fault> {
    let admit = |text: String| {
        backend_library::ProductText::new(text.clone())
            .map_err(|error| Fault::admission(error, Operand::Argument(text)))
    };
    let command = match command {
        SurfaceCommand::References { target } => SurfaceCommand::References {
            target: admit(resolve_declaration(engine, target.as_str(), "references")?)?,
        },
        SurfaceCommand::Read { locators } => {
            let mut resolved = Vec::with_capacity(locators.len());
            for locator in locators {
                resolved.push(admit(resolve_declaration(
                    engine,
                    locator.as_str(),
                    "read",
                )?)?);
            }
            SurfaceCommand::Read {
                locators: resolved.into_boxed_slice(),
            }
        }
        other => other.clone(),
    };
    let reply = engine
        .surface(command.clone())
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    Ok(Answer::Product(Box::new(
        crate::product_view_for_owned_command(&command, reply),
    )))
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
) -> Result<
    (
        ViewSnapshot,
        Option<backend_library::SemanticSearchStatus>,
        Option<backend_library::PublishedSearchReadiness>,
    ),
    Fault,
> {
    let semantic_search = reply.semantic_search_status();
    match reply.reply {
        CommandReply::Search(snapshot) => Ok((snapshot, semantic_search, None)),
        CommandReply::PublishedNameSearch(page) => {
            Ok((page.snapshot, semantic_search, Some(page.readiness)))
        }
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
    probe_relation_rows(engine, Probe::Related(coordinate), coordinate, notes)
}

fn probe_relation_rows(
    engine: &mut dyn Engine,
    probe: Probe<'_>,
    coordinate: &str,
    notes: &mut Vec<Fault>,
) -> RelationRows {
    match engine.probe(probe).map(|reply| reply.reply) {
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
    let identity = Identity::parse(coordinate);
    let Some(project) = identity.project().cloned() else {
        return Vec::new();
    };
    // An admitted external target is retained for navigation, without claiming
    // ownership by the local package named in its scoped display address.
    // Its document selection proves the target; a local outline cannot prove
    // that foreign declaration's members or source availability.
    if identity.shape() == crate::identity::IdentityShape::External
        && coordinate.starts_with(&format!(
            "{}::external::{}::",
            project.root(),
            backend_library::RowId::Symbol(document.symbol).stable_key(),
        ))
        && document.selection.as_ref().is_some_and(|selection| {
            selection.coordinate() == coordinate
                && selection.symbol() == document.symbol
                && selection.source().root == document.basis
                && document.source_basis() == Some(selection.source())
        })
    {
        return Vec::new();
    }
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

/// Streams the selected owner's immediate-parent arrangement only. A first
/// member page cannot prove completion; the authenticated terminal closes the
/// retained-row observation. Native/source completeness is not promoted.
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
        let reply = engine.members_page(document, PROBE_ROWS, continuation)?;
        let (snapshot, terminal) = match reply.reply {
            CommandReply::ProjectionPage(page) => (page.snapshot, Some(page.terminal)),
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
        // first member page does, all later pages must retain that exact
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
                return Err(ClientError::Protocol(
                    "document member projection contains another owner".to_owned(),
                ));
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
    fn an_actual_absent_outline_keeps_catalog_absence_without_guessing_path_admission() {
        let library = Library::new();
        let existing_directory = std::env::temp_dir();
        assert!(existing_directory.is_dir(), "positive directory control");
        let existing_path = existing_directory.to_string_lossy();
        // The presenter has only catalog absence for each spelling. Even the
        // existing directory is not proof that indexing this operand is intended.
        for path in [
            "/workspace/not-yet-published",
            "relative/missing",
            existing_path.as_ref(),
        ] {
            let query = OutlineQuery::new(package_key(path), library.revision_root());
            let error = library
                .execute(Command::Outline(query))
                .expect_err("package is absent in the actual catalog");
            let failure = CommandFailure::from(error);
            assert_eq!(failure, CommandFailure::NotFound);
            let page_error = library
                .outline_page(
                    package_key(path),
                    PageRequest::new(library.revision_root(), QueryLimit::default()),
                )
                .expect_err("flat page has the same real absence");
            assert_eq!(CommandFailure::from(page_error), CommandFailure::NotFound);
            for probe in [
                Probe::Outline(path),
                Probe::OutlinePage { path, limit: 200 },
            ] {
                let fault = next_step(
                    Fault::from_client_error(
                        &ClientError::CommandFailed(failure.clone()),
                        Operand::Path(path.to_owned()),
                    ),
                    probe,
                );
                assert_eq!(fault.slug(), crate::FaultSlug::NotFound);
                assert_eq!(fault.cause().slug(), crate::CauseSlug::Absent);
                assert_eq!(fault.operand().render(), path);
                assert_eq!(
                    fault.affordance().tool_call(),
                    Some(serde_json::json!({"name":"backend.packages", "arguments":{}}))
                );
                assert_eq!(
                    fault.affordance().shell().as_deref(),
                    Some("nudox packages")
                );
                assert!(fault.cause().sentence().contains("no outline is published"));
            }
        }
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

    fn catalog(extra_members: usize) -> Library {
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
        rows.extend((0..extra_members).map(|index| {
            let coordinate = format!("{OWNER}::additional_{index}");
            Row::in_package(
                RowId::Symbol(symbol_key(&coordinate)),
                basis,
                package,
                coordinate,
            )
            .with_parent(owner)
            .with_kind(DeclarationKind::Constant)
        }));
        rows.extend((0..35_300).map(|index| {
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
                catalog: catalog(0),
                disturbance,
                calls: 0,
                cursors: Vec::new(),
            }
        }
        fn large(disturbance: Disturbance) -> Self {
            Self {
                catalog: catalog(400),
                ..Self::new(disturbance)
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
        fn members_page(
            &mut self,
            document: &Document,
            limit: u16,
            continuation: Option<PageContinuation>,
        ) -> Result<ReplyDto, ClientError> {
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
                .members_page(
                    DocumentQuery::selected(symbol_key(OWNER), document.basis),
                    request,
                )
                .expect("actual direct-parent page selector");
            if document.symbol != symbol_key(OWNER) {
                page.snapshot.root = ViewRoot::new_incomplete(
                    page.snapshot.root.recipe(),
                    page.snapshot.root.basis(),
                    page.snapshot.root.frontier(),
                    Vec::new(),
                    page.snapshot.root.coverage().to_vec(),
                )
                .expect("negative complete projection omits the selected owner");
                page.snapshot.next = None;
                page.terminal = PageTerminal::Complete;
            }
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
        assert_eq!(reader.calls, 1);
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
            let mut reader = Reader::large(disturbance);
            let selected = reader.document();
            let document = Document::new(selected.symbol, selected.basis, []);
            assert_eq!(document.source_basis(), None);
            let mut notes = Vec::new();
            let rows = probe_members(&mut reader, OWNER, &document, &mut notes);
            if matches!(disturbance, Disturbance::None) {
                assert_eq!(rows.len(), NAMES.len() + 401);
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
    fn external_member_scope_requires_exact_selection_and_keeps_local_owner_guard() {
        let mut reader = Reader::new(Disturbance::None);
        let symbol = symbol_key("retained foreign target");
        let coordinate = format!("{PATH}::external::{}::foreign", RowId::Symbol(symbol));
        let source = reader.catalog.view().basis();
        let source = Basis {
            root: reader.catalog.revision_root(),
            ..source
        };
        let row = Row::new(RowId::Symbol(symbol), source, &coordinate);
        assert_eq!(row.package, None);
        let document = Document::new(symbol, source.root, [])
            .with_source_basis(source)
            .with_selection(
                backend_library::DocumentSelection::from_row(&row, source)
                    .expect("exact external selection"),
            )
            .expect("same selected row and full source basis");
        let mut notes = Vec::new();
        assert!(probe_members(&mut reader, &coordinate, &document, &mut notes).is_empty());
        assert_eq!(
            reader.calls, 0,
            "external navigation is not package ownership"
        );
        assert!(notes.is_empty());
        assert_eq!(
            document.location,
            backend_library::SourceAvailability::NotCaptured
        );

        // An external-looking operand alone does not bypass member admission.
        // Neither does missing optional source on a genuinely local selection.
        let mismatched_key = format!(
            "{PATH}::external::{}::foreign",
            RowId::Symbol(symbol_key("another retained target"))
        );
        let mismatched_selection = backend_library::DocumentSelection::from_row(
            &Row::new(RowId::Symbol(symbol), source, &mismatched_key),
            source,
        );
        for (coordinate, selection) in [
            (coordinate, None),
            (mismatched_key, mismatched_selection),
            (
                OWNER.to_owned(),
                backend_library::DocumentSelection::from_row(
                    &Row::in_package(RowId::Symbol(symbol), source, package_key(PATH), OWNER),
                    source,
                ),
            ),
        ] {
            let mut reader = Reader::new(Disturbance::None);
            let mut document = Document::new(symbol, source.root, []).with_source_basis(source);
            if let Some(selection) = selection {
                document = document.with_selection(selection).expect("same row basis");
            }
            let mut notes = Vec::new();
            assert!(probe_members(&mut reader, &coordinate, &document, &mut notes).is_empty());
            assert_eq!(
                reader.calls, 1,
                "one direct projection must still prove the selected owner"
            );
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].slug(), crate::FaultSlug::Protocol);
            assert!(
                notes[0]
                    .cause()
                    .sentence()
                    .contains("document owner is absent")
            );
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
            let mut reader = Reader::large(disturbance);
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

    #[test]
    fn authored_document_direct_sites_include_meta_and_both_accessor_bindings_in_one_query() {
        let mut reader = Reader::new(Disturbance::None);
        let basis = reader.catalog.view().basis();
        let package = package_key(PATH);
        let owner = symbol_key(OWNER);
        let source = include_str!("fixtures/docs-document/models.py");
        let members: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("fixtures/docs-document/members.json"))
                .expect("source-derived member sites");
        assert_eq!(members.len(), 47);
        let mut rows = reader
            .catalog
            .view()
            .rows()
            .iter()
            .filter(|row| row.id != RowId::Symbol(owner) && row.parent != Some(owner))
            .cloned()
            .collect::<Vec<_>>();
        rows.push(
            Row::in_package(
                RowId::Symbol(owner),
                basis,
                package,
                format!("{PATH}::models.py:994::Document"),
            )
            .with_kind(DeclarationKind::Class)
            .with_source(SourceLocation::new("models.py", 994).expect("owner site")),
        );
        let mut expected = std::collections::BTreeSet::new();
        for site in &members {
            let line = u32::try_from(site["line"].as_u64().expect("line")).expect("line width");
            let name = site["name"].as_str().expect("name");
            assert!(
                source
                    .lines()
                    .nth(line as usize - 1)
                    .expect("authored source line")
                    .contains(name)
            );
            let id = RowId::Symbol(symbol_key(&format!("native Document member at {line}")));
            assert!(expected.insert(id));
            rows.push(
                Row::in_package(
                    id,
                    basis,
                    package,
                    format!("{PATH}::models.py:{line}::{name}"),
                )
                .with_parent(owner)
                .with_kind(if site["kind"] == "function" {
                    DeclarationKind::Function
                } else {
                    DeclarationKind::Variable
                })
                .with_source(SourceLocation::new("models.py", line).expect("member site")),
            );
        }
        assert!(
            source
                .lines()
                .nth(1041)
                .expect("nested type site")
                .contains("class Meta:")
        );
        let meta = RowId::Symbol(symbol_key("native Document Meta at 1042"));
        expected.insert(meta);
        rows.push(
            Row::in_package(
                meta,
                basis,
                package,
                format!("{PATH}::models.py:1042::Meta"),
            )
            .with_parent(owner)
            .with_kind(DeclarationKind::Class)
            .with_source(SourceLocation::new("models.py", 1042).expect("nested type")),
        );
        let root = ViewRoot::new_incomplete(
            reader.catalog.view().recipe(),
            basis,
            reader.catalog.view().frontier(),
            rows,
            reader.catalog.view().coverage().to_vec(),
        )
        .expect("retained source projection");
        reader.catalog = Library::from_view(root.clone(), Cursor::for_view_root(&root))
            .expect("direct-parent arrangement");
        let document = reader.document();
        let rows =
            document_member_rows(&mut reader, PATH, &document).expect("48 exact direct sites");
        assert_eq!(
            reader.calls, 1,
            "35,300 unrelated declarations cause no additional query"
        );
        assert_eq!(rows.len(), 49);
        assert_eq!(
            rows.iter()
                .filter(|row| row.parent == Some(owner))
                .map(|row| row.id)
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
        assert_eq!(
            rows.iter()
                .filter(|row| Identity::parse(&row.label).name() == "ancestors_link_definition")
                .count(),
            2
        );
        assert_eq!(reader.catalog.work_counters().scan_rows, 0);
    }
}

#[cfg(test)]
mod declaration_resolution_controls {
    use super::*;
    use backend_library::{
        Basis, Cursor, DeclarationKind, Freshness, Frontier, RowId, SourceLocation, ViewRoot,
        object_version, package_key, symbol_key, view_key, view_state_root,
    };

    struct Reader {
        snapshot: ViewSnapshot,
        calls: usize,
    }
    impl Engine for Reader {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            Ok(self.snapshot.root.basis().root)
        }
        fn health(&mut self) -> Result<HealthReport, ClientError> {
            Err(ClientError::Protocol("not a readiness control".to_owned()))
        }
        fn surface(&mut self, _: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            Err(ClientError::Protocol("not a surface control".to_owned()))
        }
        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            assert!(matches!(probe, Probe::Names { .. }));
            self.calls += 1;
            Ok(ReplyDto::new(1, CommandReply::Names(self.snapshot.clone())))
        }
    }
    fn reader(paths: &[(&str, u32, &str)], more: bool) -> Reader {
        let basis = Basis::new(
            view_state_root(&[]),
            object_version(b"name resolution source control"),
        );
        let package = package_key("/workspace/go");
        let rows = paths
            .iter()
            .map(|(path, line, name)| {
                let label = format!("/workspace/go::{path}:{line}::{name}");
                Row::in_package(RowId::Symbol(symbol_key(&label)), basis, package, label)
                    .with_kind(DeclarationKind::Function)
                    .with_source(SourceLocation::new(*path, *line).expect("captured site"))
            })
            .collect();
        let root = ViewRoot::new_incomplete(
            view_key(b"name control"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 1),
            rows,
            vec![],
        )
        .expect("control source rows");
        let next = more.then(|| Cursor::for_view_root(&root));
        Reader {
            snapshot: ViewSnapshot {
                root,
                freshness: Freshness::Current,
                next,
                graph_relations: None,
                rich_graph: None,
            },
            calls: 0,
        }
    }
    #[test]
    fn every_coordinate_command_uses_the_same_unique_name_resolution() {
        for command in ["show", "source", "graph", "related", "references", "read"] {
            let mut reader = reader(&[("api.go", 3, "Parse")], false);
            assert_eq!(
                resolve_declaration(&mut reader, "Parse", command).expect("unique native row"),
                "/workspace/go::api.go:3::Parse"
            );
            assert_eq!(reader.calls, 1);
        }
    }
    #[test]
    fn a_distinct_test_or_example_declaration_is_never_silently_discarded() {
        let mut reader = reader(
            &[("api.go", 3, "Parse"), ("examples/api.go", 6, "Parse")],
            false,
        );
        assert_eq!(
            resolve_declaration(&mut reader, "Parse", "show")
                .expect_err("two real declarations")
                .slug(),
            crate::FaultSlug::Ambiguous
        );
        assert_eq!(
            resolve_declaration(&mut reader, "api.go:3::Parse", "show")
                .expect("exact source qualifier"),
            "/workspace/go::api.go:3::Parse"
        );
    }
    #[test]
    fn a_continuation_prevents_a_false_unique_result() {
        let mut reader = reader(&[("api.go", 3, "Parse")], true);
        assert_eq!(
            resolve_declaration(&mut reader, "Parse", "show")
                .expect_err("unseen rows may be distinct")
                .slug(),
            crate::FaultSlug::Ambiguous
        );
    }
    #[test]
    fn exact_coordinates_never_trigger_name_probes_or_rebind() {
        let mut reader = reader(&[("api.go", 3, "Parse")], false);
        let coordinate = "/workspace/go::semantic::exact-producer-identity::Parse";
        assert_eq!(
            resolve_declaration(&mut reader, coordinate, "show").expect("exact native operand"),
            coordinate
        );
        assert_eq!(reader.calls, 0);
    }
}
