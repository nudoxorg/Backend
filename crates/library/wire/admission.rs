//! Shared command/reply admission for process clients.
//!
//! CLI and MCP have different frame and I/O error types, but they must agree
//! on the meaning of a reply.  Keeping command shape, request correlation,
//! cursor projection, and health proof admission here prevents either client
//! from growing a second protocol state machine.

use super::ReplyDto;
use crate::{
    Command, CommandDto, CommandReply, Cursor, Document, Fragment, Outline, OutlineNode, Row,
    ViewProjection, ViewProjectionError, ViewRevision, WireClaim,
};
use core::{fmt, mem::size_of};

/// Maximum UTF-8 bytes admitted for one free-form command argument.
///
/// The limit belongs to the versioned command contract rather than to an
/// individual process adapter.  CLI, MCP, and embedded callers therefore
/// reject the same hostile input before parsing or allocating a reply.
pub const MAX_COMMAND_TEXT: usize = 8 * 1024;

/// Failure while admitting bounded command text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestAdmissionError {
    /// A text-bearing command supplied an empty value.
    EmptyText,
    /// A text-bearing command exceeded [`MAX_COMMAND_TEXT`].
    TextTooLarge,
    /// A daemon-owned surface command violated one of its structural bounds.
    InvalidSurface,
}

impl fmt::Display for RequestAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => formatter.write_str("query text cannot be empty"),
            Self::TextTooLarge => write!(formatter, "query text exceeds {MAX_COMMAND_TEXT} bytes"),
            Self::InvalidSurface => formatter.write_str("surface command failed admission"),
        }
    }
}

impl std::error::Error for RequestAdmissionError {}

/// Failure while checking a typed command/reply pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyAdmissionError {
    /// The response correlation identity differs from the request.
    RequestMismatch {
        /// Request identity sent by the caller.
        expected: u64,
        /// Identity returned by the producer.
        observed: u64,
    },
    /// The response shape or identity does not match the command.
    Protocol(String),
    /// A view projection failed its shared root/cursor/freshness admission.
    Projection(ViewProjectionError),
}

impl fmt::Display for ReplyAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestMismatch { expected, observed } => write!(
                formatter,
                "reply request id {observed} does not match request id {expected}"
            ),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Projection(error) => write!(formatter, "view projection rejected: {error:?}"),
        }
    }
}

impl std::error::Error for ReplyAdmissionError {}

/// Admits the bounded text fields of one command DTO.
///
/// This is intentionally small and transport neutral.  Syntax parsers may
/// provide friendlier field-specific diagnostics, but every process boundary
/// calls this function before handing the command to an owner or serializer.
/// Keeping it here prevents CLI and MCP from drifting on empty or oversized
/// text handling.
///
/// # Errors
///
/// Returns [`RequestAdmissionError`] when a text-bearing command is empty or
/// exceeds [`MAX_COMMAND_TEXT`].
pub fn admit_request(request: &CommandDto) -> Result<(), RequestAdmissionError> {
    if matches!(&request.command, Command::PackagePage(page) | Command::MembersPage { page, .. } if page.window().is_windowed() || page.continuation().is_some_and(|cursor| cursor.cursor().query_position().predecessor().is_some()))
    {
        return Err(RequestAdmissionError::InvalidSurface);
    }
    if matches!(&request.command, Command::GraphQuery(query) if query.page().window().is_windowed() || query.page().continuation().is_some_and(|cursor| cursor.cursor().query_position().predecessor().is_some()))
    {
        return Err(RequestAdmissionError::InvalidSurface);
    }
    if let Command::OutlinePage { page, .. } = &request.command
        && let Some(witness) = page.outline_witness()
        && (page.continuation().is_some()
            || witness.query_position().offset() == 0
            || page.window().predecessor(witness.query_position()).is_none())
    {
        return Err(RequestAdmissionError::InvalidSurface);
    }
    if let Command::MembersPage { owner, page } = &request.command
        && owner.basis() != page.basis()
    {
        return Err(RequestAdmissionError::InvalidSurface);
    }
    let catalog = match &request.command {
        Command::Name(query) => Some((query.window(), query.cursor())),
        Command::Search(query) => Some((query.window(), query.cursor())),
        Command::SearchPublishedNames(query) => {
            Some((query.query().window(), query.query().cursor()))
        }
        Command::GraphPage { page, .. } | Command::OutlinePage { page, .. } => Some((
            page.window(),
            page.continuation().map(crate::PageContinuation::cursor),
        )),
        _ => None,
    };
    if catalog.is_some_and(|(window, cursor)| {
        cursor.is_some_and(|cursor| window.predecessor(cursor.query_position()).is_none())
    }) {
        return Err(RequestAdmissionError::InvalidSurface);
    }
    let text = match &request.command {
        Command::Name(query) => Some(query.text()),
        Command::Search(query) => Some(query.text()),
        Command::SearchPublishedNames(query) => Some(query.query().text()),
        Command::Resolve { text } => Some(text.as_str()),
        Command::Surface(command) => {
            command
                .admit()
                .map_err(|_| RequestAdmissionError::InvalidSurface)?;
            None
        }
        _ => None,
    };
    if let Some(text) = text {
        if text.is_empty() {
            return Err(RequestAdmissionError::EmptyText);
        }
        if text.len() > MAX_COMMAND_TEXT {
            return Err(RequestAdmissionError::TextTooLarge);
        }
    }
    Ok(())
}

/// Admits one typed command/reply pair through the shared library rules.
///
/// Successful health replies cross the producer certificate boundary and are
/// accepted only when they yield a complete [`crate::CompleteViewProjection`].
/// Snapshot replies use the same root, cursor, and freshness projection used
/// by desktop subscriptions.
///
/// # Errors
///
/// Returns [`ReplyAdmissionError`] when correlation, command shape, identity,
/// freshness, cursor, or health proof admission fails.
pub fn admit_reply(request: &CommandDto, reply: &ReplyDto) -> Result<(), ReplyAdmissionError> {
    admit_reply_with_capability(request, reply, None)
}

/// Admits a reply while carrying an externally admitted complete-coverage
/// capability from the authenticated owner/channel.
///
/// # Errors
///
/// Returns [`ReplyAdmissionError`] when correlation, shape, identity,
/// freshness, or complete-view coverage admission fails.
pub fn admit_reply_with_capability(
    request: &CommandDto,
    reply: &ReplyDto,
    capability: Option<crate::CoverageCapability>,
) -> Result<(), ReplyAdmissionError> {
    admit_reply_internal(request, reply, capability, false)
}

/// Admit a bounded observed policy claim on an independently authenticated
/// local owner channel. The claim remains untrusted source metadata.
///
/// An arbitrary caller-created value cannot stand in for the authenticated peer:
/// ```compile_fail
/// # fn attempt(request: &backend_library::CommandDto, reply: &backend_library::ReplyDto) {
/// let _ = backend_library::admit_reply_with_local_source_policy(request, reply, None, &());
/// # }
/// ```
#[cfg(any(unix, windows))]
pub fn admit_reply_with_local_source_policy(
    request: &CommandDto,
    reply: &ReplyDto,
    capability: Option<crate::CoverageCapability>,
    channel: &backend_replication::AuthenticatedLocalPeer,
) -> Result<(), ReplyAdmissionError> {
    let _authenticated_peer = channel;
    admit_reply_internal(request, reply, capability, true)
}

fn admit_reply_internal(
    request: &CommandDto,
    reply: &ReplyDto,
    capability: Option<crate::CoverageCapability>,
    local_policy: bool,
) -> Result<(), ReplyAdmissionError> {
    if request.request_id != reply.request_id {
        return Err(ReplyAdmissionError::RequestMismatch {
            expected: request.request_id,
            observed: reply.request_id,
        });
    }
    if !super::reply::search_status_matches_reply(&reply.reply, reply.semantic_search_status()) {
        return Err(ReplyAdmissionError::Protocol(
            "semantic status exceeded the reply's search stage".to_owned(),
        ));
    }
    if let CommandReply::Document(document) | CommandReply::Page(document) = &reply.reply
        && (!document.source_policy.is_unknown()
            || document.source_range.as_ref().is_some_and(|page| !page.source_policy.is_unknown()))
        && (!local_policy || !document.source_policy.matches_document(document))
    {
        return Err(ReplyAdmissionError::Protocol(
            "source policy claim requires selected owner admission".to_owned(),
        ));
    }
    admit_reply_shape(&request.command, &reply.reply, local_policy)?;
    let expected_basis = command_basis(&request.command);
    match &reply.reply {
        CommandReply::Packages(snapshot)
        | CommandReply::ProjectionPage(crate::ProjectionPage { snapshot, .. })
        | CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => {
            let materialized_basis = expected_basis
                .map(|expected| {
                    let observed = snapshot.root.basis().root;
                    if expected.matches(observed) {
                        Ok(observed)
                    } else {
                        Err(ReplyAdmissionError::Protocol(
                            "reply source revision does not match the request".to_owned(),
                        ))
                    }
                })
                .transpose()?;
            if let CommandReply::ProjectionPage(page) = &reply.reply {
                admit_projection_page(&request.command, page, local_policy)?;
            }
            if matches!(
                &reply.reply,
                CommandReply::Names(_) | CommandReply::Search(_)
            ) {
                admit_query_page(&request.command, snapshot)?;
            }
            // Query continuations certify the predecessor, not the successor
            // page. The owner checks its exact predecessor rows while selecting
            // this page; shared admission checks the query contract separately.
            let response_cursor = match &reply.reply {
                CommandReply::ProjectionPage(_)
                | CommandReply::Names(_)
                | CommandReply::Search(_) => None,
                _ => command_cursor(&request.command),
            };
            ViewProjection::from_snapshot(snapshot, materialized_basis, response_cursor)
                .map(|_| ())
                .map_err(ReplyAdmissionError::Projection)?;
        }
        CommandReply::PublishedNameSearch(page) => {
            if !page.readiness.is_consistent() {
                return Err(ReplyAdmissionError::Protocol(
                    "name-stage readiness contradicts its full-search state".to_owned(),
                ));
            }
            let query = match &request.command {
                Command::SearchPublishedNames(query) => query.clone(),
                Command::Search(query)
                    if query.cursor().is_none()
                        && page.readiness.full_search != crate::FullSearchReadiness::Ready =>
                {
                    crate::PublishedNameSearchQuery::admit(query.clone()).map_err(|_| {
                        ReplyAdmissionError::Protocol(
                            "full search cannot degrade arbitrary query syntax".to_owned(),
                        )
                    })?
                }
                _ => {
                    return Err(ReplyAdmissionError::Protocol(
                        "published-name stage changed command domain".to_owned(),
                    ));
                }
            };
            if query.query().basis() != page.snapshot.root.basis().root {
                return Err(ReplyAdmissionError::Protocol(
                    "published-name stage changed source revision".to_owned(),
                ));
            }
            let source = page.snapshot.root.basis().root;
            let recipe = crate::QueryPageRecipe::published_names(source, &query);
            let predecessor = crate::QueryPageRecipe::published_names_predecessor(source, &query)
                .ok_or_else(|| {
                ReplyAdmissionError::Protocol(
                    "published-name predecessor changed credit".to_owned(),
                )
            })?;
            if recipe.identity() != page.snapshot.root.recipe() {
                return Err(ReplyAdmissionError::Protocol(
                    "published-name stage recipe mismatch".to_owned(),
                ));
            }
            admit_catalog_window(
                &page.snapshot,
                query.query().window(),
                query.query().cursor(),
                predecessor.identity(),
            )?;
            ViewProjection::from_snapshot(&page.snapshot, Some(source), None)
                .map_err(ReplyAdmissionError::Projection)?;
        }
        CommandReply::Health(_) => {
            let materialized_basis = match (&reply.reply, expected_basis) {
                (CommandReply::Health(root), Some(expected)) => {
                    let observed = root.root();
                    if !expected.matches(observed) {
                        return Err(ReplyAdmissionError::Protocol(
                            "reply source revision does not match the request".to_owned(),
                        ));
                    }
                    Some(observed)
                }
                _ => None,
            };
            reply
                .admit_complete_view_projection_with_capability(materialized_basis, capability)
                .map(|_| ())
                .map_err(ReplyAdmissionError::Protocol)?;
        }
        CommandReply::Readiness(report) => {
            if report.revision().cursor().root() != report.revision().root()
                || report.revision().cursor().query_offset() != 0
                || report.revision().source() != report.basis().object
            {
                return Err(ReplyAdmissionError::Protocol(
                    "readiness report does not describe one owner state".to_owned(),
                ));
            }
        }
        CommandReply::GraphQueryPage(page) => {
            admit_graph_query_page(request, page)?;
        }
        CommandReply::SemanticShapes(batch) => {
            let Command::SemanticShapes(shape_request) = &request.command else {
                return Err(ReplyAdmissionError::Protocol(
                    "semantic-shape reply does not match its command".to_owned(),
                ));
            };
            batch
                .admit_against(shape_request)
                .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
        }
        CommandReply::Revision(_)
        | CommandReply::Error(_)
        | CommandReply::Failed(_)
        | CommandReply::Added(_)
        | CommandReply::Removed(_)
        | CommandReply::Document(_)
        | CommandReply::Page(_)
        | CommandReply::Outline(_)
        | CommandReply::Resolved(_)
        | CommandReply::Surface(_) => {}
    }
    Ok(())
}

fn admit_query_page(
    command: &Command,
    snapshot: &crate::ViewSnapshot,
) -> Result<(), ReplyAdmissionError> {
    use crate::QueryPageRecipe;
    let root = snapshot.root.basis().root;
    let invalid = || {
        ReplyAdmissionError::Protocol(
            "query page does not match its continuation contract".to_owned(),
        )
    };
    let (output, input, window, predecessor) = match command {
        Command::Name(query) => (
            QueryPageRecipe::names(root, query),
            QueryPageRecipe::names_predecessor(root, query).ok_or_else(invalid)?,
            query.window(),
            query.cursor(),
        ),
        Command::Search(query) => (
            QueryPageRecipe::search(root, query),
            QueryPageRecipe::search_predecessor(root, query).ok_or_else(invalid)?,
            query.window(),
            query.cursor(),
        ),
        _ => return Err(invalid()),
    };
    if snapshot.root.recipe() != output.identity() {
        return Err(invalid());
    }
    admit_catalog_window(snapshot, window, predecessor, input.identity())
}

fn admit_catalog_window(
    snapshot: &crate::ViewSnapshot,
    window: crate::QueryWindow,
    predecessor: Option<Cursor>,
    predecessor_recipe: crate::ViewRecipeId,
) -> Result<(), ReplyAdmissionError> {
    let invalid = || {
        ReplyAdmissionError::Protocol(
            "catalog page does not match its issued credit/delivery/predecessor contract"
                .to_owned(),
        )
    };
    let count = snapshot.root.row_count();
    if count > u64::from(window.delivery().get()) {
        return Err(invalid());
    }
    let offset = predecessor.map_or(0, Cursor::query_offset);
    if let Some(cursor) = predecessor {
        let frontier = snapshot.root.frontier();
        let previous = window
            .predecessor(cursor.query_position())
            .ok_or_else(invalid)?;
        if cursor.recipe() != predecessor_recipe
            || cursor.branch() != frontier.branch
            || cursor.log() != frontier.log
            || cursor.schema() != crate::CURSOR_SCHEMA
            || (offset > 0 && count == 0)
            || (offset == 0 && cursor.root() != snapshot.root.basis().root)
            || (offset > 0 && offset < u64::from(previous.delivery().get()))
        {
            return Err(invalid());
        }
    }
    if let Some(next) = snapshot.next {
        if count != u64::from(window.delivery().get())
            || next.query_offset() != offset.checked_add(count).ok_or_else(invalid)?
            || predecessor.is_some_and(|cursor| next.sequence() < cursor.sequence())
        {
            return Err(invalid());
        }
        match (window.is_windowed(), next.query_position().predecessor()) {
            (false, None) => {}
            (true, Some(span))
                if span.issued() == window.issued()
                    && u64::from(span.delivery().get()) == count => {}
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

fn admit_graph_query_page(
    request: &CommandDto,
    page: &crate::GraphQueryPage,
) -> Result<(), ReplyAdmissionError> {
    let Command::GraphQuery(query) = &request.command else {
        return Err(ReplyAdmissionError::Protocol(
            "graph query page does not match its command".to_owned(),
        ));
    };
    let invalid = |error: String| {
        ReplyAdmissionError::Protocol(format!(
            "graph query page does not match its owner request: {error}"
        ))
    };
    query
        .admit_page(page)
        .map_err(|error| invalid(error.to_string()))?;
    if let crate::PageTerminal::More(next) = page.terminal {
        let certificate = request
            .certificate()
            .ok_or_else(|| invalid("owner certificate omitted".to_owned()))?;
        let recipe = certificate
            .claims
            .iter()
            .find_map(|claim| {
                if let WireClaim::Cursor { recipe, .. } = claim {
                    Some(recipe)
                } else {
                    None
                }
            })
            .ok_or_else(|| invalid("owner cursor claim omitted".to_owned()))?;
        let owner_recipe = certificate
            .key_bytes::<crate::canonical::ViewRecipeSchema>(crate::WireSchema::ViewRecipe, recipe)
            .map_err(invalid)?;
        let cursor = next.cursor();
        // The reply cursor already has typed canonical identities. Compare its
        // entire owner frontier with the request's preserved authority claim;
        // this also binds a first page, which has no predecessor cursor.
        let owner = Cursor::for_view(
            owner_recipe,
            cursor.version(),
            crate::Frontier::new(
                cursor.branch(),
                cursor.log(),
                cursor.schema(),
                cursor.root(),
                cursor.sequence(),
            ),
        );
        certificate.cursor_claim(owner).map_err(invalid)?;
        query
            .admit_page_against(page, owner)
            .map_err(|error| invalid(error.to_string()))?;
    }
    Ok(())
}

fn admit_projection_page(
    command: &Command,
    page: &crate::ProjectionPage,
    local_policy: bool,
) -> Result<(), ReplyAdmissionError> {
    let expected_recipe = match command {
        Command::PackagePage(_) => {
            crate::QueryPageRecipe::packages(page.snapshot.root.basis().root).identity()
        }
        Command::OutlinePage {
            package,
            page: request,
        } => {
            let order = page.outline_order.as_ref().ok_or_else(|| {
                ReplyAdmissionError::Protocol("outline page omitted its native order".to_owned())
            })?;
            let predecessor = request.continuation().map(crate::PageContinuation::cursor);
            let witness = request.outline_witness();
            if predecessor.is_some() && witness.is_some() {
                return Err(ReplyAdmissionError::Protocol(
                    "outline page supplied both a cursor and witness".to_owned(),
                ));
            }
            let start = predecessor.map_or_else(
                || witness.map_or(0, |witness| witness.query_position().offset()),
                Cursor::query_offset,
            );
            if order.start() != start {
                return Err(ReplyAdmissionError::Protocol("outline order changed its absolute start".to_owned()));
            }
            order.validate(&page.snapshot, *package, request.limit())
                .map_err(ReplyAdmissionError::Protocol)?;
            if let Some(next) = page.snapshot.next {
                if order.symbols().len() != usize::from(request.window().delivery().get())
                    || next.query_offset() != start.checked_add(order.symbols().len() as u64)
                        .ok_or_else(|| ReplyAdmissionError::Protocol("outline offset overflow".to_owned()))?
                {
                    return Err(ReplyAdmissionError::Protocol("outline continuation changed its ordered predecessor count".to_owned()));
                }
            }
            // Only the native producer reconstructs/authenticates the prior
            // ordered rows and digest. This helper use checks span and scope;
            // echoing the supplied recipe here grants no predecessor authority.
            if let Some(predecessor) = predecessor {
                admit_catalog_window(
                    &page.snapshot,
                    request.window(),
                    Some(predecessor),
                    predecessor.recipe(),
                )?;
            } else if let Some(witness) = witness {
                let count = page.snapshot.root.row_count();
                if count > u64::from(request.window().delivery().get())
                    || (start > 0 && count == 0)
                    || witness.query_position().offset() == 0
                    || request.window().predecessor(witness.query_position()).is_none()
                    || witness.branch() != page.snapshot.root.frontier().branch
                    || witness.log() != page.snapshot.root.frontier().log
                    || witness.schema() != page.snapshot.root.frontier().schema
                {
                    return Err(ReplyAdmissionError::Protocol(
                        "outline page does not match its untrusted predecessor span".to_owned(),
                    ));
                }
                if let Some(next) = page.snapshot.next {
                    if count != u64::from(request.window().delivery().get())
                        || next.query_offset() != start.checked_add(count).ok_or_else(|| {
                            ReplyAdmissionError::Protocol("outline offset overflow".to_owned())
                        })?
                        || next.sequence() < witness.sequence()
                        || next.branch() != witness.branch()
                        || next.log() != witness.log()
                        || next.schema() != witness.schema()
                    {
                        return Err(ReplyAdmissionError::Protocol(
                            "outline successor changed the witnessed frontier".to_owned(),
                        ));
                    }
                    match (request.window().is_windowed(), next.query_position().predecessor()) {
                        (false, None) => {}
                        (true, Some(span))
                            if span.issued() == request.window().issued()
                                && span.delivery() == request.window().delivery() => {}
                        _ => {
                            return Err(ReplyAdmissionError::Protocol(
                                "outline successor changed its issued credit/delivery span"
                                    .to_owned(),
                            ));
                        }
                    }
                }
            } else {
                admit_catalog_window(&page.snapshot, request.window(), None, page.snapshot.root.recipe)?;
            }
            crate::QueryPageRecipe::outline_window(page.snapshot.root.basis().root, *package, request.window(), order).identity()
        },
        Command::MembersPage {
            owner,
            page: request,
        } => crate::QueryPageRecipe::members(
            page.snapshot.root.basis().root,
            owner.symbol(),
            request.limit(),
        )
        .identity(),
        Command::GraphPage {
            symbol,
            page: request,
        } => crate::QueryPageRecipe::graph_window(
            page.snapshot.root.basis().root,
            *symbol,
            request.window(),
        )
        .identity(),
        _ => {
            return Err(ReplyAdmissionError::Protocol(
                "projection page reply does not match a paged command".to_owned(),
            ));
        }
    };
    if !matches!(command, Command::OutlinePage { .. }) && page.outline_order.is_some() {
        return Err(ReplyAdmissionError::Protocol("outline order crossed its page family".to_owned()));
    }
    if page.snapshot.root.recipe != expected_recipe {
        return Err(ReplyAdmissionError::Protocol(
            "projection page recipe does not match the requested query".to_owned(),
        ));
    }

    // A bounded qualified graph page may carry its center as a result-only
    // witness when the actual row belongs to another page. It is an observed
    // claim on the authenticated local channel, not a raw-key authority mint.
    if let Command::GraphPage { symbol, .. } = command
        && let Some(graph) = &page.snapshot.rich_graph
    {
        let page_nodes = page.snapshot.root.rows().iter().map(|row| crate::GraphNodeId::for_row(row.id))
            .collect::<std::collections::BTreeSet<_>>();
        let captured = graph.nodes.iter().any(|node| !node.source_policy.is_unknown())
            || graph.edges.iter().any(|edge| !edge.source_policy.is_unknown());
        let center_is_witness = !page_nodes.contains(&graph.center);
        if center_is_witness && !captured {
            return Err(ReplyAdmissionError::Protocol("graph page center witness is not admitted".to_owned()));
        }
        if captured {
            let invalid = || ReplyAdmissionError::Protocol("graph page center witness is not admitted".to_owned());
            if !local_policy || (center_is_witness && page.snapshot.graph_relations.is_some())
                || graph.revision.view_root != page.snapshot.root.root().to_bytes()
                || graph.admit().is_err()
            { return Err(invalid()); }
            let Some(center) = graph.nodes.iter().find(|node| node.id == graph.center) else { return Err(invalid()) };
            if center.source_policy.is_unknown()
                || !center.source_policy.matches_graph_page_address(*symbol)
                || graph.nodes.iter().any(|node| {
                    (!page_nodes.contains(&node.id) && node.id != graph.center)
                        || node.source_policy.is_unknown()
                        || !node.source_policy.matches_graph_basis(page.snapshot.root.basis())
                })
                || page.snapshot.root.rows().iter().any(|row| {
                    let id = crate::GraphNodeId::for_row(row.id);
                    graph.nodes.iter().find(|node| node.id == id).is_none_or(|node| {
                        node.label.as_str() != row.label.as_str()
                            || node.coordinate.as_ref().map(|coordinate| coordinate.as_str()) != Some(row.label.as_str())
                    })
                })
                || graph.edges.iter().any(|edge| {
                    (center_is_witness && !page_nodes.contains(&edge.from) && !page_nodes.contains(&edge.to))
                        || edge.source_policy.is_unknown()
                        || !edge.source_policy.matches_graph_basis(page.snapshot.root.basis())
                })
            { return Err(invalid()); }
        }
    }

    if let Command::MembersPage { owner, .. } = command
        && owner
            .source_basis()
            .is_some_and(|source| source != page.snapshot.root.basis())
    {
        return Err(ReplyAdmissionError::Protocol(
            "member page changed its selected source object or stream".to_owned(),
        ));
    }
    if let Command::GraphPage {
        symbol,
        page: request,
    } = command
        && request.window().is_windowed()
    {
        let predecessor = request.continuation().map(crate::PageContinuation::cursor);
        let input = crate::QueryPageRecipe::graph_predecessor(
            page.snapshot.root.basis().root,
            *symbol,
            *request,
        )
        .ok_or_else(|| {
            ReplyAdmissionError::Protocol("graph query predecessor changed credit".to_owned())
        })?;
        admit_catalog_window(
            &page.snapshot,
            request.window(),
            predecessor,
            input.identity(),
        )?;
    }
    let expected_next = page.snapshot.next.map(crate::PageContinuation::from_cursor);
    let terminal_matches_snapshot = match page.terminal {
        crate::PageTerminal::More(continuation) => expected_next == Some(continuation),
        crate::PageTerminal::Complete | crate::PageTerminal::Cancelled => expected_next.is_none(),
    };
    if !terminal_matches_snapshot {
        return Err(ReplyAdmissionError::Protocol(
            "projection page terminal does not match its continuation".to_owned(),
        ));
    }
    Ok(())
}

/// Returns a conservative allocation bound for one reply DTO.
///
/// This bound is deliberately owned by the DTO crate.  Process clients have
/// different transport error types, but they must reject the same hostile
/// nested rows, outline depth, and certificate claims before invoking their
/// serializers.  The calculation walks persistent relation entries through
/// [`crate::ViewRoot::encoded_size_bound`] and never materializes a compatibility
/// row slice.
#[must_use]
pub fn reply_memory_bound(reply: &ReplyDto) -> usize {
    let mut bound = 0usize;
    match &reply.reply {
        CommandReply::Packages(snapshot)
        | CommandReply::ProjectionPage(crate::ProjectionPage { snapshot, .. })
        | CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => {
            add_bound(&mut bound, snapshot.root.encoded_size_bound());
            if let Some(graph) = &snapshot.rich_graph {
                add_bound(&mut bound, match &graph.availability {
                    crate::GraphAvailability::Unavailable(v) | crate::GraphAvailability::Failed(v) => v.as_str().len().saturating_mul(6),
                    _ => 0,
                });
                for node in graph.nodes.iter() {
                    add_bound(&mut bound, 512usize.saturating_add(node.source_policy.encoded_size_bound())
                        .saturating_add(node.label.as_str().len().saturating_mul(6))
                        .saturating_add(node.coordinate.as_ref().map_or(0, |v| v.as_str().len().saturating_mul(6)))
                        .saturating_add(node.kind.as_ref().map_or(0, |v| v.as_str().len().saturating_mul(6)))
                        .saturating_add(match &node.availability {
                            crate::GraphAvailability::Unavailable(v) | crate::GraphAvailability::Failed(v) => v.as_str().len().saturating_mul(6),
                            _ => 0,
                        }));
                }
                for edge in graph.edges.iter() {
                    add_bound(&mut bound, 512usize.saturating_add(edge.source_policy.encoded_size_bound())
                        .saturating_add(edge.dependency_requirement.as_ref().map_or(0, |v| v.as_str().len().saturating_mul(6)))
                        .saturating_add(match &edge.availability {
                            crate::GraphAvailability::Unavailable(v) | crate::GraphAvailability::Failed(v) => v.as_str().len().saturating_mul(6),
                            _ => 0,
                        }));
                }
            }
            if let CommandReply::ProjectionPage(page) = &reply.reply {
                if let Some(order) = &page.outline_order {
                    add_bound(&mut bound, 256usize.saturating_add(order.symbols().len().saturating_mul(128)));
                }
            }
        }
        CommandReply::PublishedNameSearch(page) => {
            add_bound(
                &mut bound,
                page.snapshot.root.encoded_size_bound().saturating_add(128),
            );
        }
        CommandReply::Health(root) => add_bound(&mut bound, root.encoded_size_bound()),
        CommandReply::Readiness(report) => add_bound(
            &mut bound,
            512usize.saturating_add(report.coverage().len().saturating_mul(64)),
        ),
        CommandReply::Revision(_) => add_bound(&mut bound, crate::CURSOR_CONTROL_BYTES + 256),
        CommandReply::Document(document) | CommandReply::Page(document) => {
            add_document_bound(&mut bound, document);
        }
        CommandReply::Outline(outline) => add_outline_bound(&mut bound, outline),
        CommandReply::Resolved(rows) => {
            for row in rows {
                add_row_bound(&mut bound, row);
            }
        }
        CommandReply::GraphQueryPage(page) => {
            for row in &page.rows {
                for (name, value) in row.fields() {
                    add_bound(&mut bound, name.len());
                    add_graph_value_bound(&mut bound, value);
                }
            }
        }
        CommandReply::SemanticShapes(_) => {
            add_bound(&mut bound, crate::MAX_SEMANTIC_SHAPE_BYTES);
        }
        CommandReply::Surface(reply) => {
            add_bound(&mut bound, reply.encoded_size_bound());
        }
        CommandReply::Added(_) | CommandReply::Removed(_) => {}
        CommandReply::Error(message) => {
            add_bound(&mut bound, 512usize.saturating_add(message.len()));
        }
        CommandReply::Failed(crate::CommandFailure::PartiallyPublished(partial)) => {
            add_bound(
                &mut bound,
                512usize.saturating_add(partial.encoded_size_bound()),
            );
        }
        CommandReply::Failed(failure) => {
            add_bound(
                &mut bound,
                512usize.saturating_add(failure.to_string().len()),
            );
        }
    }
    if let Some(certificate) = reply.certificate() {
        for claim in &certificate.claims {
            add_claim_bound(&mut bound, claim);
        }
    }
    if reply.health_cursor().is_some() {
        add_bound(&mut bound, crate::CURSOR_CONTROL_BYTES);
    }
    bound
}

fn add_bound(bound: &mut usize, bytes: usize) {
    *bound = bound.saturating_add(bytes);
}

fn add_row_bound(bound: &mut usize, row: &Row) {
    let mut bytes = 512usize
        .saturating_add(row.label.len())
        .saturating_add(row.signature.as_deref().map_or(0, str::len));
    bytes = bytes.saturating_add(
        row.identity_preimage()
            .map_or(0, |preimage| preimage.as_str().len()),
    );
    if let Some(kind) = row.kind {
        bytes = bytes.saturating_add(kind.name().len().saturating_add(16));
    }
    if let Some(source) = row.source.captured() {
        bytes = bytes
            .saturating_add(source.path().len())
            .saturating_add(size_of::<u32>() + 16);
    }
    bytes = bytes.saturating_add(row.excerpt.text().map_or(0, str::len));
    bytes = bytes.saturating_add(facts_bound(&row.facts));
    for fragment in &row.document {
        bytes = bytes.saturating_add(match fragment {
            Fragment::Text(value) | Fragment::Code(value) => 64usize.saturating_add(value.len()),
            Fragment::Link { label, .. } => 96usize.saturating_add(label.len()),
            Fragment::Break => 8,
        });
    }
    add_bound(bound, bytes);
}

/// Encoded-size bound of one row's or document's declaration facts: the
/// retained text plus a fixed allowance for the wire framing.
fn facts_bound(facts: &crate::DeclarationFacts) -> usize {
    if facts.is_unobserved() {
        0
    } else {
        128usize.saturating_add(facts.text_bytes())
    }
}

fn add_document_bound(bound: &mut usize, document: &Document) {
    let mut bytes = 512usize
        .saturating_add(document.selection.as_ref().map_or(0, |selection| {
            512usize.saturating_add(selection.coordinate().len())
        }))
        .saturating_add(document.source_policy.encoded_size_bound())
        .saturating_add(document.source_range.as_ref().map_or(0, |page| {
            page.source_policy.encoded_size_bound().saturating_add(page.text.len().saturating_mul(6))
        }))
        .saturating_add(document.signature.as_deref().map_or(0, str::len))
        .saturating_add(document.excerpt.text().map_or(0, str::len))
        .saturating_add(facts_bound(&document.facts));
    for fragment in &document.fragments {
        bytes = bytes.saturating_add(match fragment {
            Fragment::Text(value) | Fragment::Code(value) => 64usize.saturating_add(value.len()),
            Fragment::Link { label, .. } => 96usize.saturating_add(label.len()),
            Fragment::Break => 8,
        });
    }
    add_bound(bound, bytes);
}

fn add_graph_value_bound(bound: &mut usize, value: &crate::GraphValue) {
    match value {
        crate::GraphValue::String(value) => add_bound(bound, value.len()),
        crate::GraphValue::List(values) => {
            for value in values {
                add_graph_value_bound(bound, value);
            }
        }
        crate::GraphValue::Null
        | crate::GraphValue::Boolean(_)
        | crate::GraphValue::Signed(_)
        | crate::GraphValue::Unsigned(_)
        | crate::GraphValue::Float(_) => add_bound(bound, 16),
    }
}

fn add_outline_bound(bound: &mut usize, outline: &Outline) {
    fn visit(bound: &mut usize, node: &OutlineNode, depth: usize) {
        if depth > 64 {
            *bound = usize::MAX;
            return;
        }
        add_bound(bound, 128);
        for child in &node.children {
            visit(bound, child, depth + 1);
        }
    }
    add_bound(bound, 512);
    visit(bound, &outline.root, 0);
    for root in &outline.additional_roots {
        visit(bound, root, 0);
    }
}

fn add_claim_bound(bound: &mut usize, claim: &WireClaim) {
    let bytes = match claim {
        WireClaim::Key { value, .. }
        | WireClaim::RowIdentity {
            preimage: value, ..
        } => 256usize.saturating_add(value.len()),
        WireClaim::KeyBytes { value, .. } | WireClaim::Version { value, .. } => {
            256usize.saturating_add(value.len())
        }
        WireClaim::KeyCommitment { .. } | WireClaim::RootCommitment { .. } => 256,
        WireClaim::Root { canonical, .. } => 256usize.saturating_add(canonical.len()),
        WireClaim::Intent { token, payload, .. } => 256usize
            .saturating_add(token.len())
            .saturating_add(payload.len()),
        WireClaim::Delta {
            base,
            target,
            changes,
            ..
        } => 256usize
            .saturating_add(base.len())
            .saturating_add(target.len())
            .saturating_add(changes.len()),
        WireClaim::Cursor {
            recipe,
            version,
            branch,
            log,
            root,
            ..
        } => 256usize
            .saturating_add(recipe.len())
            .saturating_add(version.len())
            .saturating_add(branch.len())
            .saturating_add(log.len())
            .saturating_add(root.len()),
        WireClaim::Coverage {
            scope,
            observed,
            producer,
            context,
            evidence,
        } => 256usize
            .saturating_add(scope.len())
            .saturating_add(observed.len())
            .saturating_add(producer.len())
            .saturating_add(context.len())
            .saturating_add(evidence.len()),
    };
    add_bound(bound, bytes);
}

fn admit_reply_shape(command: &Command, reply: &CommandReply, local_policy: bool) -> Result<(), ReplyAdmissionError> {
    let valid = match (command, reply) {
        (_, CommandReply::Failed(crate::CommandFailure::ReplyTooLarge { basis })) => {
            let expected = match command {
                Command::Search(query) => query.basis(),
                Command::Name(query) => query.basis(),
                Command::GraphPage { page, .. } => page.basis(),
                _ => {
                    return Err(ReplyAdmissionError::Protocol(
                        "catalog reply ceiling does not match this command".to_owned(),
                    ));
                }
            };
            if *basis != expected {
                return Err(ReplyAdmissionError::Protocol(
                    "catalog reply ceiling changed its requested revision".to_owned(),
                ));
            }
            true
        }
        (
            Command::Surface(crate::SurfaceCommand::IndexSearch { .. }),
            CommandReply::Failed(crate::CommandFailure::QueryPreparation { .. }),
        ) => {
            // IndexSearch carries no request basis. This is an observation of
            // the current local-plane readiness, never result/cursor authority.
            // A retry retains its original query and opaque cursor; once ready,
            // the existing composite snapshot check can still refuse that cursor.
            true
        }
        (_, CommandReply::Failed(crate::CommandFailure::QueryPreparation { basis, .. })) => {
            let expected = match command {
                Command::Search(query) => query.basis(),
                Command::GraphQuery(query)
                    if !matches!(query.control(), crate::GraphQueryControl::Cancel) =>
                {
                    query.page().basis()
                }
                _ => {
                    return Err(ReplyAdmissionError::Protocol(
                        "query preparation does not match this command".to_owned(),
                    ));
                }
            };
            if *basis != expected {
                return Err(ReplyAdmissionError::Protocol(
                    "query preparation source revision does not match the request".to_owned(),
                ));
            }
            true
        }

        (
            Command::Add {
                cargo_target,
                package,
                ..
            },
            CommandReply::Failed(crate::CommandFailure::PartiallyPublished(partial)),
        ) => {
            partial
                .admit()
                .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
            if crate::package_key(partial.package.as_str()) != *package {
                return Err(ReplyAdmissionError::Protocol(
                    "partial add reply does not match the requested package".to_owned(),
                ));
            }
            if partial
                .source_capture
                .cargo_target
                .as_ref()
                .map(|scope| &scope.name)
                != cargo_target.as_ref()
            {
                return Err(ReplyAdmissionError::Protocol(
                    "partial add reply does not match the requested Cargo target".to_owned(),
                ));
            }
            true
        }
        (_, CommandReply::Failed(crate::CommandFailure::PartiallyPublished(_))) => {
            return Err(ReplyAdmissionError::Protocol(
                "partial add publication does not match this command".to_owned(),
            ));
        }
        (_, CommandReply::Error(_) | CommandReply::Failed(_))
        | (Command::Packages, CommandReply::Packages(_))
        | (Command::Name(_), CommandReply::Names(_))
        | (Command::Resolve { .. }, CommandReply::Resolved(_))
        | (Command::Search(_), CommandReply::Search(_) | CommandReply::PublishedNameSearch(_))
        | (Command::SearchPublishedNames(_), CommandReply::PublishedNameSearch(_))
        | (Command::Health, CommandReply::Health(_) | CommandReply::Readiness(_))
        | (
            Command::Revision | Command::OutlineOwnerWitnessCapability,
            CommandReply::Revision(_),
        )
        | (
            Command::PackagePage(_)
            | Command::OutlinePage { .. }
            | Command::GraphPage { .. }
            | Command::MembersPage { .. },
            CommandReply::ProjectionPage(_),
        )
        | (Command::GraphQuery(_), CommandReply::GraphQueryPage(_)) => true,
        (Command::SemanticShapes(_), CommandReply::SemanticShapes(_)) => true,
        (Command::Surface(command), CommandReply::Surface(reply)) => {
            admit_surface_reply_internal(command, reply, local_policy)?;
            true
        }
        (Command::Add { package, .. }, CommandReply::Added(intent)) => {
            if *intent != crate::Intent::request_package(*package).id() {
                return Err(ReplyAdmissionError::Protocol(
                    "add reply intent does not match the requested package".to_owned(),
                ));
            }
            true
        }
        (Command::Remove { package }, CommandReply::Removed(intent)) => {
            if *intent != crate::Intent::remove_package(*package).id() {
                return Err(ReplyAdmissionError::Protocol(
                    "remove reply intent does not match the requested package".to_owned(),
                ));
            }
            true
        }
        (
            Command::Document(query) | Command::Source(query),
            CommandReply::Document(document) | CommandReply::Page(document),
        ) => {
            if !document.admits_query(query) {
                return Err(ReplyAdmissionError::Protocol(
                    "document reply identity does not match the query".to_owned(),
                ));
            }
            match (query.source_range(), document.source_range.as_ref()) {
                (Some(request), Some(page)) if page.admits(request) || (local_policy && document.source_policy.matches_document(document) && page.matches_request_shape(request)) => {}
                (None, None) => {}
                _ => return Err(ReplyAdmissionError::Protocol(
                    "source range reply does not match the request".to_owned(),
                )),
            }
            true
        }
        (
            Command::Show { symbol },
            CommandReply::Document(document) | CommandReply::Page(document),
        ) => {
            if document.symbol != *symbol {
                return Err(ReplyAdmissionError::Protocol(
                    "document reply identity does not match the query".to_owned(),
                ));
            }
            true
        }
        (Command::Outline(query), CommandReply::Outline(outline)) => {
            if outline.package != query.package()
                || outline.basis() != query.basis()
                || query
                    .source_basis()
                    .is_some_and(|source| outline.source_basis() != Some(source))
            {
                return Err(ReplyAdmissionError::Protocol(
                    "outline reply identity does not match the query".to_owned(),
                ));
            }
            true
        }
        (Command::Graph(query) | Command::Related(query), CommandReply::Graph(snapshot)) => {
            if !query.basis().matches(snapshot.root.basis().root) {
                return Err(ReplyAdmissionError::Protocol(
                    "graph reply source revision does not match the request".to_owned(),
                ));
            }
            // Match the execution owner's order: native ID, qualified exact
            // label, then captured source alias. Selected addresses use only
            // native membership. A neighboring label cannot shadow that ID.
            let absent = || {
                ReplyAdmissionError::Protocol(
                    "graph reply omitted the selected source declaration".to_owned(),
                )
            };
            let ambiguous = || {
                ReplyAdmissionError::Protocol(
                    "graph reply source declaration is ambiguous or differs from its center"
                        .to_owned(),
                )
            };
            let native = snapshot.root.row_refs().find(|row| {
                matches!(row.id, crate::RowId::Symbol(symbol)
                    if query.symbol().matches(symbol))
            });
            let center = if let Some(center) = native {
                center
            } else {
                if query.symbol().is_selected() {
                    return Err(absent());
                }
                let mut exact = snapshot.root.row_refs().filter(|row| {
                    matches!(row.id, crate::RowId::Symbol(_))
                        && row.label.len() <= crate::MAX_ROW_IDENTITY_PREIMAGE_BYTES
                        && query.symbol().matches(crate::symbol_key(&row.label))
                });
                if let Some(center) = exact.next() {
                    if exact.next().is_some() {
                        return Err(ambiguous());
                    }
                    if crate::PackageQualifiedDeclaration::from_row(center).is_none() {
                        return Err(absent());
                    }
                    center
                } else {
                    let mut aliases = snapshot.root.row_refs().filter(|row| {
                        crate::CapturedSourceCoordinate::from_row(row)
                            .is_some_and(|proof| proof.matches_address(query.symbol()))
                    });
                    let Some(center) = aliases.next() else {
                        return Err(absent());
                    };
                    if aliases.next().is_some() {
                        return Err(ambiguous());
                    }
                    center
                }
            };
            if snapshot
                .rich_graph
                .as_ref()
                .is_some_and(|rich| rich.center != crate::GraphNodeId::for_row(center.id))
            {
                return Err(ambiguous());
            }
            true
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ReplyAdmissionError::Protocol(
            "reply kind does not match the command".to_owned(),
        ))
    }
}

/// Admits a surface reply against the actual command, including caller identity.
///
/// Command IDs alone cannot distinguish ticket and durable-operation replies.
/// This borrowed boundary is shared by wire clients and caller journals before
/// a receipt can acknowledge saved work. It does not require envelope cloning.
///
/// # Errors
/// Returns [`ReplyAdmissionError`] for invalid structure, a different reply
/// family, or an observation that does not match the exact requested identity.
pub fn admit_surface_reply(
    command: &crate::SurfaceCommand,
    reply: &crate::SurfaceReply,
) -> Result<(), ReplyAdmissionError> {
    admit_surface_reply_internal(command, reply, false)
}

/// Checks only Read request/reply shape and claim correlation. This does not
/// authenticate a peer or owner seal; call it only after a trusted transport
/// has admitted the full response. Generic admission remains fail-closed.
pub fn admit_surface_read_shape(
    command: &crate::SurfaceCommand,
    reply: &crate::SurfaceReply,
) -> Result<(), ReplyAdmissionError> {
    if !matches!(command, crate::SurfaceCommand::Read { .. }) {
        return Err(ReplyAdmissionError::Protocol("expected Read request".into()));
    }
    admit_surface_reply_internal(command, reply, true)
}

/// Checks only References request/reply correlation and bounded raw claims.
/// This grants no owner, peer, source-byte, or edge authority and is intended
/// only after a trusted transport has admitted the complete response.
pub fn admit_surface_references_shape(
    command: &crate::SurfaceCommand,
    reply: &crate::SurfaceReply,
) -> Result<(), ReplyAdmissionError> {
    if !matches!(command, crate::SurfaceCommand::References { .. }) {
        return Err(ReplyAdmissionError::Protocol("expected References request".into()));
    }
    admit_surface_reply_internal(command, reply, true)
}

fn admit_surface_reply_internal(
    command: &crate::SurfaceCommand,
    reply: &crate::SurfaceReply,
    local_policy: bool,
) -> Result<(), ReplyAdmissionError> {
    if let (
        crate::SurfaceCommand::SemanticSource { request },
        crate::SurfaceReply::SemanticSource(chunk),
    ) = (command, reply)
    {
        // Exact variant matching supplies the command-kind check. Correlation
        // includes complete chunk admission and decodes this range only once.
        chunk
            .admit_request(request)
            .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
    } else {
        reply
            .admit_with_local_source_policy(command.id(), local_policy)
            .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
    }
    if let crate::SurfaceCommand::References { target } = command {
        let crate::SurfaceReply::References { target: actual, .. } = reply else {
            return Err(ReplyAdmissionError::Protocol("references reply changed shape".into()));
        };
        if target != actual {
            return Err(ReplyAdmissionError::Protocol("references target differs from request".into()));
        }
    }
    if let crate::SurfaceCommand::Read { locators } = command {
        let crate::SurfaceReply::Read(records) = reply else {
            return Err(ReplyAdmissionError::Protocol("read reply changed shape".into()));
        };
        if records.len() != locators.len()
            || records.iter().zip(locators.iter()).any(|(record, locator)| record.label != *locator)
        {
            return Err(ReplyAdmissionError::Protocol(
                "read records differ from requested locators".into(),
            ));
        }
    }
    if let crate::SurfaceCommand::Impact { request } = command {
        let crate::SurfaceReply::Impact(report) = reply else {
            return Err(ReplyAdmissionError::Protocol("impact reply kind changed".into()));
        };
        if report.direction != request.direction
            || report.depth != request.depth
            || report.strength_filter != request.strength
            || report.rows.len() > usize::from(request.page_rows)
        {
            return Err(ReplyAdmissionError::Protocol(
                "impact reply differs from the request".into(),
            ));
        }
        if let (crate::impact::ImpactSeed::Named(spec), crate::impact::ImpactSeedState::Resolved) =
            (&request.seed, &report.seed_state)
        {
            if report.seeds.iter().any(|seed| seed.domain != spec.domain) {
                return Err(ReplyAdmissionError::Protocol(
                    "impact seeds belong to another domain than requested".into(),
                ));
            }
        }
    }
    if let crate::SurfaceCommand::StyloImpact { request } = command {
        let crate::SurfaceReply::StyloImpact(page) = reply else {
            return Err(ReplyAdmissionError::Protocol(
                "CSS impact reply kind changed".into(),
            ));
        };
        let cursor_matches = match &page.terminal {
            crate::stylo_impact::StyloImpactTerminal::More { continuation, .. } => continuation.matches_seed(&request.seed),
            crate::stylo_impact::StyloImpactTerminal::OutputLimit { continuation: Some(cursor), .. } => cursor.matches_seed(&request.seed),
            _ => true,
        };
        if page.seed != request.seed
            || !cursor_matches
            || page.rows.len() > usize::from(request.query.page_rows)
            || crate::surface::serialized_json_size(reply) > request.query.output_bytes as usize
            || page
                .rows
                .iter()
                .any(|row| !stylo_hit_matches_request(&row.witness, &request.query))
        {
            return Err(ReplyAdmissionError::Protocol(
                "CSS impact reply differs from request or exceeds its budget".into(),
            ));
        }
    }
    if let crate::SurfaceCommand::ImpactCapture { request } = command {
        let crate::SurfaceReply::ImpactCapture(reply) = reply else { return Err(ReplyAdmissionError::Protocol("impact capture reply kind changed".into())); };
        if reply.seed != request.seed || reply.lease.claims().project != crate::package_key(request.package.as_str()).to_bytes() {
            return Err(ReplyAdmissionError::Protocol("impact capture reply differs from request".into()));
        }
    }
    if let crate::SurfaceCommand::ImpactCompare { request } = command {
        let crate::SurfaceReply::ImpactCompare(page) = reply else { return Err(ReplyAdmissionError::Protocol("impact comparison reply kind changed".into())); };
        let seed = match request.side { crate::impact_comparison::ImpactSide::Base => request.base.claims().seed, crate::impact_comparison::ImpactSide::Target => request.target.claims().seed };
        let offset = request.continuation.as_ref().map_or(0, |cursor| cursor.claims().offset);
        let witness_bytes = page.rows.iter().try_fold(0usize, |sum, row| row.witness.len().checked_mul(32).and_then(|n| sum.checked_add(n))).unwrap_or(usize::MAX);
        let counters_complete = page.counters.input_rows.is_some() && page.counters.input_bytes.is_some() && page.counters.temporary_bytes.is_some() && page.counters.edges.is_some() && page.counters.nodes.is_some() && page.counters.witness_bytes.is_some();
        let cursor_matches = match &page.terminal {
            crate::impact_comparison::ImpactTerminal::More { continuation } => {
                let c = continuation.claims();
                !page.rows.is_empty() && counters_complete && request.recipe().is_ok_and(|recipe| c.recipe == recipe)
                    && u32::try_from(page.rows.len()).ok().and_then(|n| offset.checked_add(n)) == Some(c.offset)
            },
            crate::impact_comparison::ImpactTerminal::WorkLimit { reason } => reason.len() <= 128,
            crate::impact_comparison::ImpactTerminal::Expired | crate::impact_comparison::ImpactTerminal::SeedUnavailable => page.rows.is_empty(),
            _ => true,
        };
        if page.base != request.base || page.target != request.target || page.side != request.side || !cursor_matches
            || page.rows.len() > usize::from(request.limits.page_rows) || witness_bytes > request.limits.witness_bytes as usize
            || crate::surface::serialized_json_size(reply) > request.limits.output_bytes as usize
            || page.counters.input_rows.is_some_and(|n| n > request.limits.input_rows) || page.counters.input_bytes.is_some_and(|n| n > request.limits.input_bytes) || page.counters.temporary_bytes.is_some_and(|n| n > request.limits.temporary_bytes) || page.counters.edges.is_some_and(|n| n > request.limits.edges) || page.counters.nodes.is_some_and(|n| n > request.limits.nodes) || page.counters.witness_bytes.is_some_and(|n| n > request.limits.witness_bytes)
            || page.rows.iter().any(|row| seed.as_ref() != row.witness.first() || !row.witness.contains(&row.dependency)) {
            return Err(ReplyAdmissionError::Protocol("impact comparison reply differs from request or exceeds its bounds".into()));
        }
    }
    if let crate::SurfaceCommand::ImpactDiff { request } = command {
        use crate::impact_comparison::{ImpactDiffCertainty as Certainty, ImpactDiffDomain as Domain, ImpactDiffSide as Side, ImpactDiffTerminal as Terminal, ImpactSide};
        let crate::SurfaceReply::ImpactDiff(page) = reply else { return Err(ReplyAdmissionError::Protocol("impact diff reply kind changed".into())); };
        let offset = request.continuation.as_ref().map_or(0, |cursor| cursor.claims().offset);
        let cursor_ok = match &page.terminal {
            Terminal::More { continuation } => !page.rows.is_empty() && request.recipe().is_ok_and(|recipe| continuation.claims().recipe == recipe) && u32::try_from(page.rows.len()).ok().and_then(|n| offset.checked_add(n)) == Some(continuation.claims().offset) && !page.walk_limited && page.counters.is_some() && page.comparisons.is_some() && page.compared_bytes.is_some(),
            Terminal::WorkLimit { reason } => reason.len() <= 128,
            Terminal::Expired | Terminal::Cancelled => page.rows.is_empty(),
            Terminal::Complete => !page.walk_limited && page.counters.is_some() && page.comparisons.is_some() && page.compared_bytes.is_some(),
        };
        let valid_rows = page.rows.iter().all(|row| {
            let certainty_ok = match (row.side, row.certainty) {
                (Side::BaseOnly, Certainty::ExactCapturedRow) => page.target_capture_complete,
                (Side::TargetOnly, Certainty::ExactCapturedRow) => page.base_capture_complete,
                (Side::BothChanged, Certainty::ExactCapturedRow) => true,
                (Side::BaseOnly, Certainty::CandidateMissing) => !page.target_capture_complete,
                (Side::TargetOnly, Certainty::CandidateMissing) => !page.base_capture_complete,
                (Side::BothChanged, Certainty::CandidateMissing) => false,
            };
            let base_source_ok = row.base_source.as_ref().is_none_or(|source| matches!(row.side, Side::BaseOnly | Side::BothChanged) && source.claims().lease == request.base && source.claims().site == row.id);
            let target_source_ok = row.target_source.as_ref().is_none_or(|source| matches!(row.side, Side::TargetOnly | Side::BothChanged) && source.claims().lease == request.target && source.claims().site == row.id);
            certainty_ok && base_source_ok && target_source_ok && (request.domain != Domain::VerifiedSourceImages || (row.impacts.is_empty() && row.base_source.is_none() && row.target_source.is_none())) && row.impacts.iter().all(|path| {
                (path.path.is_some() || page.context_gapped) && path.witness.first() == Some(&row.id) && path.witness.contains(&path.dependency) && matches!((row.side, path.side), (Side::BaseOnly, ImpactSide::Base) | (Side::TargetOnly, ImpactSide::Target) | (Side::BothChanged, _))
            })
        });
        let impact_count = page.rows.iter().try_fold(0usize, |sum, row| sum.checked_add(row.impacts.len())).unwrap_or(usize::MAX);
        let witness_bytes = page.rows.iter().flat_map(|row| &row.impacts).try_fold(0usize, |sum, path| path.witness.len().checked_mul(32).and_then(|n| sum.checked_add(n))).unwrap_or(usize::MAX);
        let path_bytes = page.rows.iter().flat_map(|row| &row.impacts).try_fold(0usize, |sum, path| sum.checked_add(path.path.as_ref().map_or(0, String::len))).unwrap_or(usize::MAX);
        let counters_ok = page.counters.as_ref().is_none_or(|c| c.scan_rows <= request.limits.scan_rows && c.scan_bytes <= request.limits.scan_bytes && c.temporary_bytes <= request.limits.temporary_bytes && c.seed_walks <= u32::from(request.limits.seed_walks) && c.walk_input_rows <= request.limits.walk_input_rows && c.walk_input_bytes <= request.limits.walk_input_bytes && c.walk_edges <= request.limits.walk_edges && c.walk_nodes <= request.limits.walk_nodes && c.walk_rows <= request.limits.walk_rows && c.witness_bytes <= request.limits.witness_bytes && c.walk_path_bytes <= request.limits.walk_path_bytes && c.walk_temporary_peak <= request.limits.temporary_bytes);
        if page.base != request.base || page.target != request.target || page.domain != request.domain || !cursor_ok || !valid_rows || !counters_ok
            || page.rows.len() > usize::from(request.limits.page_rows) || impact_count > request.limits.walk_rows as usize || witness_bytes > request.limits.witness_bytes as usize || path_bytes > request.limits.walk_path_bytes as usize
            || crate::surface::serialized_json_size(reply) > request.limits.output_bytes as usize
            || page.comparisons.is_some_and(|n| n > request.limits.comparisons) || page.compared_bytes.is_some_and(|n| n > request.limits.compared_bytes) {
            return Err(ReplyAdmissionError::Protocol("impact diff reply differs from request or exceeds its bounds".into()));
        }
    }
    if let crate::SurfaceCommand::ImpactReadSource { .. } = command {
        if !matches!(reply, crate::SurfaceReply::ImpactReadSource(_)) { return Err(ReplyAdmissionError::Protocol("impact source reply kind changed".into())); }
    }
    if let crate::SurfaceCommand::StyloQuery { request } = command {
        let crate::SurfaceReply::StyloQuery(page) = reply else {
            return Err(ReplyAdmissionError::Protocol(
                "CSS query reply kind changed".into(),
            ));
        };
        if page.hits.len() > usize::from(request.page_rows)
            || crate::surface::serialized_json_size(reply) > request.output_bytes as usize
            || page
                .hits
                .iter()
                .any(|hit| !stylo_hit_matches_request(hit, request))
        {
            return Err(ReplyAdmissionError::Protocol(
                "CSS query reply exceeds requested budget".into(),
            ));
        }
    }
    if let crate::SurfaceCommand::SqlQuery { request } = command {
        use crate::sql_query::SqlQueryRole;
        use backend_semantic::sql_analysis::SqlFact;
        let crate::SurfaceReply::SqlQuery(page) = reply else {
            return Err(ReplyAdmissionError::Protocol("SQL query reply kind changed".into()));
        };
        if page.basis.project != crate::package_key(request.package.as_str()).to_bytes()
            || (!page.host_gaps.is_empty() && !matches!(request.role, SqlQueryRole::Gap | SqlQueryRole::Diagnostic))
            || page.hits.len() + page.host_gaps.len() > usize::from(request.page_rows)
            || crate::surface::serialized_json_size(reply) > request.output_bytes as usize
            || page.hits.iter().any(|hit| !match (request.role, &hit.fact) {
                (SqlQueryRole::Any, _) => true,
                (SqlQueryRole::Relation, SqlFact::SqliteRelation { name, .. }) => request.name.as_ref().is_none_or(|value| value == name),
                (SqlQueryRole::OutputLineage, SqlFact::SqliteOutputLineage { output_name, .. }) => request.name.as_ref().is_none_or(|value| value == output_name),
                (SqlQueryRole::Diagnostic, SqlFact::Diagnostic { .. }) => true,
                (SqlQueryRole::Diagnostic, SqlFact::Reference(reference)) => {
                    matches!(reference.kind, backend_semantic::sql_analysis::SqlRefKind::Column | backend_semantic::sql_analysis::SqlRefKind::Relation)
                        && matches!(reference.resolution, backend_semantic::sql_analysis::SqlResolution::Unresolved { .. })
                }
                (SqlQueryRole::ColumnRead, SqlFact::PostgresColumnRead { .. }) => true,
                (SqlQueryRole::Statement, SqlFact::Statement { .. }) => true,
                (SqlQueryRole::Gap, SqlFact::Statement { support, .. }) => {
                    !matches!(support, backend_semantic::sql_analysis::SqlStatementSupport::Full)
                }
                (SqlQueryRole::Definition, SqlFact::Definition(definition)) => {
                    request.name.as_ref().is_none_or(|value| {
                        value == &definition.name || definition.head_name.as_ref() == Some(value)
                    }) && request.kind.is_none_or(|kind| kind == definition.kind)
                        && request.parent.as_ref().is_none_or(|value| {
                            definition.parent.as_ref() == Some(value) || definition.head_parent.as_ref() == Some(value)
                        })
                }
                (SqlQueryRole::Reference, SqlFact::Reference(reference)) => {
                    use backend_semantic::sql_analysis::{SqlObjectKind as K, SqlRefKind as R};
                    request.name.as_ref().is_none_or(|value| value == &reference.name)
                        && request.kind.is_none_or(|kind| match (kind, reference.kind) {
                            (K::Table | K::View, R::Relation) | (K::Column, R::Column) | (K::Function, R::Function)
                            | (K::Type, R::Type) | (K::Schema, R::Schema) | (K::Index, R::Index)
                            | (K::Sequence, R::Sequence) | (K::Trigger, R::Trigger) => true,
                            _ => false,
                        })
                }
                _ => false,
            }) {
            return Err(ReplyAdmissionError::Protocol("SQL query reply exceeds request or filter".into()));
        }
    }
    if let (crate::SurfaceCommand::SqlReadSource { reference },
        crate::SurfaceReply::SqlReadSource(crate::sql_query::SqlSourceRead::Available { reference: echoed, .. })) = (command, reply)
    {
        if reference != echoed {
            return Err(ReplyAdmissionError::Protocol("SQL source reply reference changed".into()));
        }
    }
    if let crate::SurfaceCommand::SemanticVersions {
        package,
        limit,
        cursor,
    } = command
    {
        let crate::SurfaceReply::SemanticVersionsPage(page) = reply else {
            return Err(ReplyAdmissionError::Protocol(
                "paged semantic-versions command requires a native descriptor page".to_owned(),
            ));
        };
        page.admit_request(package, *limit, cursor.as_ref())
            .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
    }
    admit_index_reply(command, reply)?;
    if let crate::SurfaceCommand::PackageProfile {
        package,
        compiler_outcome,
    } = command
    {
        let compiler_outcomes = match (compiler_outcome, reply) {
            (Some(_), crate::SurfaceReply::PackageCompilerOutcomes(outcomes)) => Some(outcomes),
            (
                None,
                crate::SurfaceReply::PackageProfile {
                    compiler_outcomes, ..
                },
            ) => compiler_outcomes.as_ref(),
            _ => {
                return Err(ReplyAdmissionError::Protocol(
                    "package profile reply changed read mode".to_owned(),
                ));
            }
        };
        match compiler_outcomes {
            Some(outcomes) => outcomes
                .admit_request(package, compiler_outcome.as_ref())
                .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?,
            None if compiler_outcome.is_some()
                || matches!(package, crate::PackageReference::Local(_)) =>
            {
                return Err(ReplyAdmissionError::Protocol(
                    "current compiler outcomes are missing from local package profile reply"
                        .to_owned(),
                ));
            }
            None => {}
        }
    }
    if let crate::SurfaceCommand::PackageCoverageCurrent { package, view_root } = command {
        let crate::SurfaceReply::PackageCoverageCurrent(result) = reply else {
            return Err(ReplyAdmissionError::Protocol(
                "published package coverage reply changed shape".to_owned(),
            ));
        };
        result
            .admit_request(package, *view_root)
            .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
    }
    if let crate::SurfaceCommand::PackageSourceMembership { request } = command {
        if !matches!(reply, crate::SurfaceReply::PackageSourceMembershipPage(page) if page.admits_request(request))
        {
            return Err(ReplyAdmissionError::Protocol(
                "source membership reply does not match the exact requested frontier".to_owned(),
            ));
        }
    }
    if let crate::SurfaceReply::ProjectTree(tree) = reply {
        let (root, captured) = match command {
            crate::SurfaceCommand::ProjectTree { root } => (root, false),
            crate::SurfaceCommand::CapturedProjectTree { root } => (root, true),
            _ => {
                return Err(ReplyAdmissionError::Protocol(
                    "project tree has another request family".to_owned(),
                ));
            }
        };
        if captured != matches!(&tree.source, crate::browse::TreeSource::CapturedSource { .. })
            || !tree.request_binding().is_some_and(|binding| {
                binding.matches_requested_root(std::path::Path::new(root.as_str()))
            })
        {
            return Err(ReplyAdmissionError::Protocol(
                "project tree reader or observation does not match the exact requested directory and mode".to_owned(),
            ));
        }
    }
    Ok(())
}

fn stylo_hit_matches_request(
    hit: &crate::stylo_query::StyloQueryHit,
    request: &crate::stylo_query::StyloQueryRequest,
) -> bool {
    use crate::stylo_query::{StyloNameRole as Role, StyloQueryHit as Hit};
    use backend_semantic::stylo_analysis::{StyloSelectorComponentKind, StyloSourceRole};
    match hit {
        Hit::SourceSite { site, .. } => {
            site.decoded == request.name
                && matches!(
                    (request.role, site.role),
                    (Role::Class, StyloSourceRole::ClassName)
                        | (
                            Role::CustomProperty,
                            StyloSourceRole::CustomPropertyDeclarationName
                        )
                )
        }
        Hit::NativeSelector { selector, .. } => {
            request.role == Role::Class
                && selector.components.iter().any(|component| {
                    component.kind == StyloSelectorComponentKind::Class
                        && component.name == request.name
                })
        }
        Hit::NativeDeclaration { declaration, .. } => {
            request.role == Role::CustomProperty
                && !declaration.property.contains('\\')
                && declaration.property == request.name
        }
        Hit::NativeDependency { dependency, .. } => {
            request.role == Role::CustomProperty
                && dependency.kind == "var"
                && request.name.strip_prefix("--") == Some(dependency.name.as_str())
        }
    }
}

/// Internal consistency cannot authorize another caller's receipt.
fn admit_index_reply(
    command: &crate::SurfaceCommand,
    reply: &crate::SurfaceReply,
) -> Result<(), ReplyAdmissionError> {
    use crate::{
        IndexJobObservation, IndexJobOutcome, IndexOperationObservation, IndexStartResult,
        SurfaceCommand, SurfaceReply,
    };

    let operation = match (command, reply) {
        (
            SurfaceCommand::IndexOperationStart {
                cargo_target,
                operation_key,
                package,
                execution_intent,
            },
            SurfaceReply::IndexOperationStarted(observation),
        ) => Some((
            *operation_key,
            Some((package, *execution_intent, cargo_target.as_ref())),
            observation,
        )),
        (
            SurfaceCommand::IndexOperationStatus { operation_key },
            SurfaceReply::IndexOperationStatus(observation),
        ) => Some((*operation_key, None, observation)),
        (
            SurfaceCommand::IndexOperationStart { .. }
            | SurfaceCommand::IndexOperationStatus { .. },
            _,
        ) => {
            return Err(ReplyAdmissionError::Protocol(
                "index operation reply family does not match the command".to_owned(),
            ));
        }
        (
            SurfaceCommand::IndexStart {
                cargo_target,
                package,
                ..
            },
            SurfaceReply::IndexStarted(result),
        ) => {
            let observed = match result {
                IndexStartResult::Started { ticket, .. } => ticket.package(),
                IndexStartResult::Terminal(terminal) => terminal.ticket.package(),
            };
            if package != observed {
                return Err(ReplyAdmissionError::Protocol(
                    "index start reply does not match the requested package".to_owned(),
                ));
            }
            if let IndexStartResult::Terminal(terminal) = result
                && let IndexJobOutcome::PartiallyPublished(partial) = &terminal.outcome
                && partial
                    .source_capture
                    .cargo_target
                    .as_ref()
                    .map(|scope| &scope.name)
                    != cargo_target.as_ref()
            {
                return Err(ReplyAdmissionError::Protocol(
                    "index start reply does not match the requested Cargo target".to_owned(),
                ));
            }
            return Ok(());
        }
        (SurfaceCommand::IndexStart { .. }, _) => {
            return Err(ReplyAdmissionError::Protocol(
                "index start reply family does not match the command".to_owned(),
            ));
        }
        _ => None,
    };
    if let Some((requested_key, request, observation)) = operation {
        let observed_key = match observation {
            IndexOperationObservation::Known(status) => status.operation_key,
            IndexOperationObservation::Unknown { operation_key }
            | IndexOperationObservation::OutsideReceiptWindow { operation_key, .. } => {
                *operation_key
            }
        };
        if requested_key != observed_key {
            return Err(ReplyAdmissionError::Protocol(
                "index operation reply does not match the exact requested key".to_owned(),
            ));
        }
        if let Some((package, execution_intent, cargo_target)) = request {
            let digest = crate::index_operation_scoped_request_digest(
                package,
                execution_intent,
                cargo_target,
            );
            let matches = match observation {
                IndexOperationObservation::Known(status) => {
                    status.package == *package
                        && status.execution_intent == execution_intent
                        && status.cargo_target.as_ref() == cargo_target
                        && status.request_digest == digest
                }
                IndexOperationObservation::OutsideReceiptWindow { request_digest, .. } => {
                    *request_digest == digest
                }
                IndexOperationObservation::Unknown { .. } => true,
            };
            if !matches {
                return Err(ReplyAdmissionError::Protocol(
                    "index operation reply does not match the requested package and execution intent".to_owned(),
                ));
            }
        }
        return Ok(());
    }

    let (requested, observed) = match (command, reply) {
        (SurfaceCommand::IndexAwait { ticket }, SurfaceReply::IndexTerminal(terminal)) => {
            (ticket, &terminal.ticket)
        }
        (
            SurfaceCommand::IndexProgress { ticket, .. },
            SurfaceReply::IndexProgress(observation),
        ) => {
            let observed = match observation {
                IndexJobObservation::Pending(page) => &page.ticket,
                IndexJobObservation::Terminal(terminal) => &terminal.ticket,
                IndexJobObservation::Unknown { ticket, .. } => ticket,
            };
            (ticket, observed)
        }
        (SurfaceCommand::IndexCancel { ticket }, SurfaceReply::IndexCancellation(receipt)) => {
            (ticket, &receipt.ticket)
        }
        (
            SurfaceCommand::IndexAwait { .. }
            | SurfaceCommand::IndexProgress { .. }
            | SurfaceCommand::IndexCancel { .. },
            _,
        ) => {
            return Err(ReplyAdmissionError::Protocol(
                "index ticket reply family does not match the command".to_owned(),
            ));
        }
        _ => return Ok(()),
    };
    if requested != observed {
        return Err(ReplyAdmissionError::Protocol(
            "index reply does not match the exact requested ticket".to_owned(),
        ));
    }
    Ok(())
}

fn command_basis(command: &Command) -> Option<ViewRevision> {
    match command {
        Command::Document(query) | Command::Source(query) => Some(query.basis()),
        Command::Outline(query) => Some(query.basis()),
        Command::Name(query) => Some(query.basis()),
        Command::Search(query) => Some(query.basis()),
        Command::SearchPublishedNames(query) => Some(query.query().basis()),
        Command::Graph(query) | Command::Related(query) => Some(query.basis()),
        Command::PackagePage(page)
        | Command::OutlinePage { page, .. }
        | Command::GraphPage { page, .. }
        | Command::MembersPage { page, .. } => Some(page.basis()),
        Command::GraphQuery(query) => Some(query.page().basis()),
        Command::SemanticShapes(request) => Some(request.basis()),
        _ => None,
    }
}

fn command_cursor(command: &Command) -> Option<Cursor> {
    match command {
        Command::Name(query) => query.cursor(),
        Command::Search(query) => query.cursor(),
        Command::SearchPublishedNames(query) => query.query().cursor(),
        Command::PackagePage(page)
        | Command::OutlinePage { page, .. }
        | Command::GraphPage { page, .. }
        | Command::MembersPage { page, .. } => {
            page.continuation().map(crate::PageContinuation::cursor)
        }
        Command::GraphQuery(query) => query
            .page()
            .continuation()
            .map(crate::PageContinuation::cursor),
        _ => None,
    }
}

#[cfg(test)]
mod query_preparation_tests {
    use super::*;
    use crate::{GraphQueryRequest, NameQuery, Query, QueryLimit, QueryPreparationState};
    use std::collections::BTreeMap;

    #[test]
    fn centerless_graph_page_requires_request_bound_local_observed_witness() {
        let basis = crate::Basis::new(crate::view_state_root(&[]), crate::object_version(b"graph-page-witness"));
        let mut rows = (0..2).map(|index| {
            let label = format!("fixture::node{index}");
            crate::Row::new(crate::RowId::Symbol(crate::symbol_key(&label)), basis, label)
        }).collect::<Vec<_>>();
        rows.sort_by_key(|row| row.id);
        let center = match rows[0].id { crate::RowId::Symbol(symbol) => symbol, _ => unreachable!() };
        let neighbor = match rows[1].id { crate::RowId::Symbol(symbol) => symbol, _ => unreachable!() };
        let root = crate::ViewRoot::new_checked(
            crate::view_key(b"graph-page-witness"), basis,
            crate::Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            rows, vec![crate::Coverage::Complete], crate::wire::tests::capability(basis.object),
        ).expect("root");
        let library = crate::Library::from_view(root.clone(), crate::Cursor::for_view_root_at(&root, 0)).expect("library");
        let ids = root.rows().iter().map(|row| row.id).collect::<Vec<_>>();
        let issued = QueryLimit::new(2).expect("credit");
        let window = crate::QueryWindow::new(issued, QueryLimit::new(1).expect("delivery")).expect("window");
        let request = crate::PageRequest::new(library.revision_root(), issued).with_window(window).expect("request");
        let address = crate::SymbolAddress::selected(center);
        let first_request = request;
        let mut first = library.graph_page_from_ids(center, address, request, &ids).expect("first page");
        let crate::PageTerminal::More(next) = first.terminal else { panic!("continuation") };
        let request = request.with_continuation(next);
        let mut second = library.graph_page_from_ids(center, address, request, &ids).expect("second page");
        assert_eq!(second.snapshot.root.rows().len(), 1);
        assert_eq!(second.snapshot.root.rows()[0].id, crate::RowId::Symbol(neighbor));

        let selected_basis = second.snapshot.root.basis();
        let source_policy_at = |row: &crate::Row, basis| {
            let selection = crate::DocumentSelection::from_row(row, basis).expect("selection");
            let profile = crate::SemanticLanguageProfile::from_name("tsx").expect("profile");
            crate::SourcePolicyQualification::issue_observed(&[7; 32], &selection, crate::SourcePolicyObservation {
                workspace_root: [1; 32], evidence_root: [2; 32], project: [3; 32], profile, publication_profile: profile,
                publication_coordinate: "pkg:npm/fixture@1.0.0".into(), source_path: "page.tsx".into(),
                source_fact: [4; 32], input_digest: [5; 32], byte_len: 30, publication_binding: [6; 32],
                image_identity: [7; 32], image_byte_len: 128, program_membership: [8; 32], entity_index: 1,
                declaration_family: [9; 16], declaration_variant: [10; 16],
            }).expect("claim")
        };
        let source_policy_for_row = |row: &crate::Row| source_policy_at(row, selected_basis);
        let source_policy = |symbol| source_policy_for_row(root.row_ref(crate::RowId::Symbol(symbol)).expect("selected row"));
        let generation = crate::SemanticGenerationId::new([11; 32]);
        let revision = crate::RichGraphRevision::new(second.snapshot.root.root().to_bytes(), generation, [12; 32]);
        let mut builder = crate::RichGraphBuilder::new(revision, crate::GraphNodeId::for_symbol(center));
        for symbol in [center, neighbor] {
            let row = root.row_ref(crate::RowId::Symbol(symbol)).expect("row");
            let mut node = crate::RichGraphNode::new(crate::GraphNodeId::for_symbol(symbol), row.label.clone(), Some(row.label.clone()), None, crate::GraphAvailability::Ready, None).expect("node");
            node.source_policy = source_policy(symbol);
            builder.add_node(node).expect("add node");
        }
        let relation = crate::GraphRelation::new(crate::RowId::Symbol(center), crate::RowId::Symbol(neighbor), crate::SemanticLinkKind::Calls);
        let mut edge = crate::RichGraphEdge::from_relation(relation, crate::GraphProvenance::semantic(generation, [12; 32], [13; 32], crate::SemanticConfidence::Compiler));
        edge.source_policy = source_policy(center);
        builder.add_edge(edge).expect("edge");
        let gap = crate::GraphAvailability::Unavailable(crate::ProductText::from_static("observed policy unknown"));
        second.snapshot.rich_graph = Some(builder.finish_with_availability(gap).expect("graph"));
        second.snapshot.graph_relations = None;
        let command = Command::GraphPage { symbol: address, page: request };
        assert!(admit_projection_page(&command, &second, true).is_ok());
        assert!(matches!(second.terminal, crate::PageTerminal::Complete));
        assert!(admit_projection_page(&command, &second, false).is_err());
        let wrong = Command::GraphPage { symbol: crate::SymbolAddress::selected(neighbor), page: request };
        assert!(admit_projection_page(&wrong, &second, true).is_err());
        // Removing every policy must not bypass center/request/peer admission.
        let mut stripped = second.clone();
        let graph = stripped.snapshot.rich_graph.as_mut().expect("graph");
        for node in std::sync::Arc::make_mut(&mut graph.nodes) {
            node.source_policy = crate::SourcePolicyQualification::Unknown;
        }
        for edge in std::sync::Arc::make_mut(&mut graph.edges) {
            edge.source_policy = crate::SourcePolicyQualification::Unknown;
        }
        assert!(graph.admit().is_ok(), "the stripped graph is otherwise shape-valid");
        assert!(admit_projection_page(&command, &stripped, true).is_err());
        assert!(admit_projection_page(&command, &stripped, false).is_err());
        assert!(admit_projection_page(&wrong, &stripped, true).is_err());
        let mut missing = second.clone();
        let graph = missing.snapshot.rich_graph.as_mut().expect("graph");
        let center_id = graph.center;
        std::sync::Arc::make_mut(&mut graph.nodes).iter_mut().find(|node| node.id == center_id).expect("center").source_policy = crate::SourcePolicyQualification::Unknown;
        assert!(admit_projection_page(&command, &missing, true).is_err());
        let mut swapped = second.clone();
        let graph = swapped.snapshot.rich_graph.as_mut().expect("graph");
        let center_id = graph.center;
        let node = std::sync::Arc::make_mut(&mut graph.nodes).iter_mut().find(|node| node.id == center_id).expect("center");
        // A whole carrier for another node cannot authenticate the center.
        node.source_policy = source_policy(neighbor);
        assert!(admit_projection_page(&command, &swapped, true).is_err());

        // A valid claim and node for the same native key but a different
        // coordinate must still match the independently admitted page row.
        let mut relabelled = second.clone();
        let graph = relabelled.snapshot.rich_graph.as_mut().expect("graph");
        let mut forged_row = root.row_ref(crate::RowId::Symbol(neighbor)).expect("row").clone();
        forged_row.label = "fixture::different".to_owned();
        let node = std::sync::Arc::make_mut(&mut graph.nodes).iter_mut()
            .find(|node| node.id == crate::GraphNodeId::for_symbol(neighbor)).expect("neighbor");
        node.label = crate::ProductText::new(forged_row.label.clone()).expect("label");
        node.coordinate = Some(crate::ProductText::new(forged_row.label.clone()).expect("coordinate"));
        node.source_policy = source_policy_for_row(&forged_row);
        assert!(graph.admit().is_ok(), "claim and node must be self-consistent before row comparison");
        assert!(admit_projection_page(&command, &relabelled, true).is_err());

        let mut foreign_basis = second.clone();
        let graph = foreign_basis.snapshot.rich_graph.as_mut().expect("graph");
        let center_id = graph.center;
        let node = std::sync::Arc::make_mut(&mut graph.nodes).iter_mut().find(|node| node.id == center_id).expect("center");
        let other_basis = crate::Basis::new(crate::view_state_root(&[]), crate::object_version(b"foreign"));
        node.source_policy = source_policy_at(root.row_ref(crate::RowId::Symbol(center)).expect("center row"), other_basis);
        assert!(admit_projection_page(&command, &foreign_basis, true).is_err());

        let mut extra_witness = second.clone();
        let original = extra_witness.snapshot.rich_graph.as_ref().expect("graph");
        let mut builder = crate::RichGraphBuilder::new(original.revision, original.center);
        for node in original.nodes.iter() { builder.add_node(node.clone()).expect("existing node"); }
        for edge in original.edges.iter() { builder.add_edge(edge.clone()).expect("existing edge"); }
        let extra_row = crate::Row::new(crate::RowId::Symbol(crate::symbol_key("fixture::extra")), selected_basis, "fixture::extra");
        let mut extra = crate::RichGraphNode::new(crate::GraphNodeId::for_row(extra_row.id), extra_row.label.clone(), Some(extra_row.label.clone()), None, crate::GraphAvailability::Ready, None).expect("extra node");
        extra.source_policy = source_policy_for_row(&extra_row);
        builder.add_node(extra.clone()).expect("extra witness");
        extra_witness.snapshot.rich_graph = Some(builder.finish_with_availability(crate::GraphAvailability::Unavailable(crate::ProductText::from_static("observed policy unknown"))).expect("shape-valid graph"));
        assert!(admit_projection_page(&command, &extra_witness, true).is_err());

        // Even a self-consistent replacement center, policy and edge cannot
        // answer a request for a different selected declaration.
        let mut wrong_center = second.clone();
        let original = wrong_center.snapshot.rich_graph.as_ref().expect("graph");
        let mut builder = crate::RichGraphBuilder::new(original.revision, extra.id);
        builder.add_node(extra.clone()).expect("foreign center");
        let neighbor_node = original.nodes.iter().find(|node| node.id == crate::GraphNodeId::for_symbol(neighbor)).expect("neighbor");
        builder.add_node(neighbor_node.clone()).expect("neighbor");
        let relation = crate::GraphRelation::new(extra_row.id, crate::RowId::Symbol(neighbor), crate::SemanticLinkKind::Calls);
        let mut foreign_edge = crate::RichGraphEdge::from_relation(relation, crate::GraphProvenance::semantic(generation, [12; 32], [13; 32], crate::SemanticConfidence::Compiler));
        foreign_edge.source_policy = source_policy_for_row(&extra_row);
        builder.add_edge(foreign_edge).expect("foreign edge");
        wrong_center.snapshot.rich_graph = Some(builder.finish_with_availability(crate::GraphAvailability::Unavailable(crate::ProductText::from_static("observed policy unknown"))).expect("shape-valid foreign graph"));
        assert!(admit_projection_page(&command, &wrong_center, true).is_err());

        let mut orphan = second.clone();
        let graph = orphan.snapshot.rich_graph.as_mut().expect("graph");
        std::sync::Arc::make_mut(&mut graph.edges)[0].to = crate::GraphNodeId::from_bytes([99; 32]);
        assert!(admit_projection_page(&command, &orphan, true).is_err());

        // The normal first page still accepts a typed relation sidecar.
        let revision = crate::RichGraphRevision::new(first.snapshot.root.root().to_bytes(), generation, [12; 32]);
        let mut builder = crate::RichGraphBuilder::new(revision, crate::GraphNodeId::for_symbol(center));
        let row = root.row_ref(crate::RowId::Symbol(center)).expect("center row");
        let mut node = crate::RichGraphNode::new(crate::GraphNodeId::for_symbol(center), row.label.clone(), Some(row.label.clone()), None, crate::GraphAvailability::Ready, None).expect("node");
        node.source_policy = source_policy(center);
        builder.add_node(node).expect("center");
        first.snapshot.rich_graph = Some(builder.finish_with_availability(crate::GraphAvailability::Unavailable(crate::ProductText::from_static("observed policy unknown"))).expect("first graph"));
        first.snapshot.graph_relations = Some(Box::new([]));
        assert!(admit_projection_page(&Command::GraphPage { symbol: address, page: first_request }, &first, true).is_ok());
        let mut legacy = first.clone();
        let graph = legacy.snapshot.rich_graph.as_mut().expect("graph");
        for node in std::sync::Arc::make_mut(&mut graph.nodes) {
            node.source_policy = crate::SourcePolicyQualification::Unknown;
        }
        assert!(admit_projection_page(&Command::GraphPage { symbol: address, page: first_request }, &legacy, false).is_ok());

        // Witness bytes are included in the ordinary graph reply bound.
        let with_witness = ReplyDto::new(1, CommandReply::ProjectionPage(second.clone()));
        let mut without = second.clone();
        without.snapshot.rich_graph = None;
        // The legacy None/None shape remains admissible, but carries no
        // qualified graph edges; consumers must treat graph evidence as
        // unavailable rather than a complete empty graph.
        assert!(without.snapshot.graph_relations.is_none());
        assert!(admit_projection_page(&command, &without, true).is_ok());
        assert!(without.snapshot.rich_graph.is_none());
        let without_witness = ReplyDto::new(1, CommandReply::ProjectionPage(without));
        assert!(reply_memory_bound(&with_witness) > reply_memory_bound(&without_witness));
        let encoded = crate::wire::encode_reply_body_bounded(&with_witness).expect("bounded encoding");
        assert!(encoded.len() <= crate::wire::MAX_REPLY_BODY);
        assert!(encoded.len() <= reply_memory_bound(&with_witness));

        #[cfg(unix)]
        {
            use std::os::unix::{fs::PermissionsExt, net::{UnixListener, UnixStream}};
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos();
            let directory = std::env::temp_dir().join(format!("nudox-graph-page-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&directory).expect("private directory");
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).expect("directory mode");
            let path = directory.join("owner.sock");
            let listener = UnixListener::bind(&path).expect("listener");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("socket mode");
            let stream = UnixStream::connect(&path).expect("connect");
            let (_server, _) = listener.accept().expect("accept");
            let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, &path).expect("authenticate peer");
            let materialized = &second.snapshot.root;
            let selected_basis = materialized.basis();
            let recipe = crate::QueryPageRecipe::graph_window(selected_basis.root, address, request.window());
            let coverage = crate::wire::tests::capability(selected_basis.object);
            let mut certificate = crate::WireCertificate::new()
                .with_claim(crate::WireClaim::KeyBytes {
                    schema: crate::WireSchema::ViewRecipe, id: crate::encode_id(materialized.recipe().as_bytes()),
                    value: recipe.canonical_preimage().to_vec().into_boxed_slice(),
                })
                .with_claim(crate::WireClaim::Version {
                    schema: crate::WireSchema::ViewVersion, id: crate::encode_id(materialized.version().as_bytes()),
                    value: crate::view_version_preimage(materialized.recipe(), selected_basis, materialized.frontier(), materialized.root(), materialized.coverage()).into_boxed_slice(),
                })
                .with_claim(crate::WireClaim::Root {
                    schema: crate::WireSchema::ViewRelation, id: crate::encode_id(materialized.root().as_bytes()),
                    canonical: materialized.canonical_relation_bytes().expect("page relation").into_boxed_slice(),
                })
                .with_claim(crate::WireClaim::Root {
                    schema: crate::WireSchema::ViewRelation, id: crate::encode_id(root.root().as_bytes()),
                    canonical: root.canonical_relation_bytes().expect("selected relation").into_boxed_slice(),
                })
                .with_claim(crate::WireClaim::Version {
                    schema: crate::WireSchema::Object, id: crate::encode_id(selected_basis.object.as_bytes()),
                    value: b"graph-page-witness".to_vec().into_boxed_slice(),
                })
                .with_claim(crate::WireClaim::Coverage {
                    scope: crate::encode_id(selected_basis.object.as_bytes()), observed: crate::encode_id(selected_basis.object.as_bytes()),
                    producer: crate::encode_id(&coverage.producer_identity()), context: crate::encode_id(&coverage.context()),
                    evidence: coverage.evidence().to_vec().into_boxed_slice(),
                })
                .with_claim(crate::WireClaim::Key {
                    schema: crate::WireSchema::Branch, id: crate::encode_id(selected_basis.branch.as_bytes()), value: "main".to_owned(),
                })
                .with_claim(crate::WireClaim::Key {
                    schema: crate::WireSchema::Log, id: crate::encode_id(selected_basis.log.as_bytes()), value: "library".to_owned(),
                });
            for row in materialized.rows() {
                let crate::RowId::Symbol(symbol) = row.id else { panic!("symbol row") };
                certificate = certificate.with_claim(crate::WireClaim::Key {
                    schema: crate::WireSchema::Symbol, id: crate::encode_id(symbol.as_bytes()), value: row.label.clone(),
                });
            }
            let certified = with_witness.clone().with_certificate(certificate);
            let encoded = crate::wire::encode_reply_body_bounded(&certified).expect("certified encoding");
            assert!(crate::decode_reply_body(&encoded).is_err());
            let decoded = crate::decode_reply_body_with_local_source_policy(&encoded, &peer).expect("authenticated local decode");
            let request = CommandDto::new(1, command.clone());
            assert!(crate::admit_reply(&request, &decoded).is_err());
            assert!(crate::admit_reply_with_local_source_policy(&request, &decoded, None, &peer).is_ok());
            std::fs::remove_file(path).expect("remove fixture socket");
            std::fs::remove_dir(directory).expect("remove fixture directory");
        }
    }

    #[test]
    fn index_search_preparation_is_observational_and_never_an_unrelated_surface_reply() {
        let basis = ViewRevision::from_bytes([59; 32]);
        for state in [
            QueryPreparationState::Preparing,
            QueryPreparationState::Retiring,
        ] {
            let reply = ReplyDto::new(
                19,
                CommandReply::Failed(crate::CommandFailure::QueryPreparation { basis, state }),
            );
            let bytes = serde_json::to_vec(&reply).expect("strict preparation codec");
            let decoded = crate::decode_reply_body(&bytes).expect("strict preparation roundtrip");
            let request = CommandDto::new(
                19,
                Command::Surface(crate::SurfaceCommand::IndexSearch {
                    query: crate::ProductText::from_static("pkg:pypi/exact@1"),
                    limit: 3,
                    cursor: None,
                }),
            );
            admit_reply(&request, &decoded).expect("observational current-view readiness");
            assert!(matches!(decoded.reply, CommandReply::Failed(
                crate::CommandFailure::QueryPreparation { basis: observed, state: actual }
            ) if observed == basis && actual == state));
            for command in [crate::SurfaceCommand::Projects, crate::SurfaceCommand::Tree] {
                assert!(
                    admit_reply(&CommandDto::new(19, Command::Surface(command)), &reply).is_err(),
                    "readiness must not widen unrelated surface admission"
                );
            }
        }
    }

    #[test]
    fn query_preparation_reply_is_bound_to_search_or_active_graph_and_exact_basis() {
        let basis = ViewRevision::from_bytes([41; 32]);
        let foreign = ViewRevision::from_bytes([42; 32]);
        let query = GraphQueryRequest::new(
            "{ Package { coordinate @output } }",
            BTreeMap::new(),
            basis,
            QueryLimit::default(),
        )
        .expect("graph request");
        for state in [
            QueryPreparationState::Preparing,
            QueryPreparationState::Retiring,
        ] {
            let reply = ReplyDto::new(
                19,
                CommandReply::Failed(crate::CommandFailure::QueryPreparation { basis, state }),
            );
            for command in [
                Command::Search(Query::new("authentication", basis, QueryLimit::default())),
                Command::GraphQuery(query.clone()),
            ] {
                assert!(admit_reply(&CommandDto::new(19, command), &reply).is_ok());
            }
            for command in [
                Command::Search(Query::new("authentication", foreign, QueryLimit::default())),
                Command::GraphQuery(
                    GraphQueryRequest::new(
                        "{ Package { coordinate @output } }",
                        BTreeMap::new(),
                        foreign,
                        QueryLimit::default(),
                    )
                    .expect("foreign graph"),
                ),
                Command::GraphQuery(query.clone().cancelled()),
                Command::Health,
                Command::Revision,
                Command::Name(NameQuery::new(
                    "authentication",
                    basis,
                    QueryLimit::default(),
                )),
                Command::Resolve {
                    text: "authentication".to_owned(),
                },
                Command::Remove {
                    package: crate::package_key("project"),
                },
            ] {
                assert!(
                    matches!(
                        admit_reply(&CommandDto::new(19, command), &reply),
                        Err(ReplyAdmissionError::Protocol(_))
                    ),
                    "readiness cannot authorize a foreign basis, cancel or unrelated command"
                );
            }
            assert!(matches!(
                admit_reply(
                    &CommandDto::new(
                        20,
                        Command::Search(Query::new("authentication", basis, QueryLimit::default()))
                    ),
                    &reply
                ),
                Err(ReplyAdmissionError::RequestMismatch { .. })
            ));
        }
    }
}
