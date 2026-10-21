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
    admit_reply_shape(&request.command, &reply.reply)?;
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
                admit_projection_page(&request.command, page)?;
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

fn admit_reply_shape(command: &Command, reply: &CommandReply) -> Result<(), ReplyAdmissionError> {
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
            Command::Add { package, .. },
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
            admit_surface_reply(command, reply)?;
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
            .admit(command.id())
            .map_err(|error| ReplyAdmissionError::Protocol(error.to_string()))?;
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
    if let (crate::SurfaceCommand::ProjectTree { root }, crate::SurfaceReply::ProjectTree(tree)) =
        (command, reply)
        && !tree.request_binding().is_some_and(|binding| {
            binding.matches_requested_root(std::path::Path::new(root.as_str()))
        })
    {
        return Err(ReplyAdmissionError::Protocol(
            "project Tree observation does not match the exact requested directory".to_owned(),
        ));
    }
    Ok(())
}

/// Internal consistency cannot authorize another caller's receipt.
fn admit_index_reply(
    command: &crate::SurfaceCommand,
    reply: &crate::SurfaceReply,
) -> Result<(), ReplyAdmissionError> {
    use crate::{
        IndexJobObservation, IndexOperationObservation, IndexStartResult, SurfaceCommand,
        SurfaceReply,
    };

    let operation = match (command, reply) {
        (
            SurfaceCommand::IndexOperationStart {
                operation_key,
                package,
                execution_intent,
            },
            SurfaceReply::IndexOperationStarted(observation),
        ) => Some((
            *operation_key,
            Some((package, *execution_intent)),
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
        (SurfaceCommand::IndexStart { package, .. }, SurfaceReply::IndexStarted(result)) => {
            let observed = match result {
                IndexStartResult::Started { ticket, .. } => ticket.package(),
                IndexStartResult::Terminal(terminal) => terminal.ticket.package(),
            };
            return if package == observed {
                Ok(())
            } else {
                Err(ReplyAdmissionError::Protocol(
                    "index start reply does not match the requested package".to_owned(),
                ))
            };
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
        if let Some((package, execution_intent)) = request {
            let digest = crate::index_operation_request_digest(package, execution_intent);
            let matches = match observation {
                IndexOperationObservation::Known(status) => {
                    status.package == *package
                        && status.execution_intent == execution_intent
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
