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
}

impl fmt::Display for RequestAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => formatter.write_str("query text cannot be empty"),
            Self::TextTooLarge => write!(formatter, "query text exceeds {MAX_COMMAND_TEXT} bytes"),
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
    let text = match &request.command {
        Command::Name(query) => Some(query.text()),
        Command::Search(query) => Some(query.text()),
        Command::Resolve { text } => Some(text.as_str()),
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
    admit_reply_shape(&request.command, &reply.reply)?;
    let expected_basis = command_basis(&request.command);
    match &reply.reply {
        CommandReply::Packages(snapshot)
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
            ViewProjection::from_snapshot(
                snapshot,
                materialized_basis,
                command_cursor(&request.command),
            )
            .map(|_| ())
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
        CommandReply::Revision(_) => {}
        CommandReply::Error(_)
        | CommandReply::Added(_)
        | CommandReply::Removed(_)
        | CommandReply::Document(_)
        | CommandReply::Page(_)
        | CommandReply::Outline(_)
        | CommandReply::Resolved(_) => {}
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
        | CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => {
            add_bound(&mut bound, snapshot.root.encoded_size_bound());
        }
        CommandReply::Health(root) => add_bound(&mut bound, root.encoded_size_bound()),
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
        CommandReply::Added(_) | CommandReply::Removed(_) => {}
        CommandReply::Error(message) => {
            add_bound(&mut bound, 512usize.saturating_add(message.len()));
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
    if let Some(kind) = row.kind {
        bytes = bytes.saturating_add(kind.name().len().saturating_add(16));
    }
    if let Some(source) = &row.source {
        bytes = bytes
            .saturating_add(source.path().len())
            .saturating_add(size_of::<u32>() + 16);
    }
    for fragment in &row.document {
        bytes = bytes.saturating_add(match fragment {
            Fragment::Text(value) | Fragment::Code(value) => 64usize.saturating_add(value.len()),
            Fragment::Link { label, .. } => 96usize.saturating_add(label.len()),
            Fragment::Break => 8,
        });
    }
    add_bound(bound, bytes);
}

fn add_document_bound(bound: &mut usize, document: &Document) {
    let mut bytes = 512usize.saturating_add(document.signature.as_deref().map_or(0, str::len));
    for fragment in &document.fragments {
        bytes = bytes.saturating_add(match fragment {
            Fragment::Text(value) | Fragment::Code(value) => 64usize.saturating_add(value.len()),
            Fragment::Link { label, .. } => 96usize.saturating_add(label.len()),
            Fragment::Break => 8,
        });
    }
    add_bound(bound, bytes);
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
}

fn add_claim_bound(bound: &mut usize, claim: &WireClaim) {
    let bytes = match claim {
        WireClaim::Key { value, .. } => 256usize.saturating_add(value.len()),
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
        (_, CommandReply::Error(_))
        | (Command::Packages, CommandReply::Packages(_))
        | (Command::Name(_), CommandReply::Names(_))
        | (Command::Resolve { .. }, CommandReply::Resolved(_))
        | (Command::Search(_), CommandReply::Search(_))
        | (Command::Health, CommandReply::Health(_))
        | (Command::Revision, CommandReply::Revision(_)) => true,
        (Command::Add { package }, CommandReply::Added(intent)) => {
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
            Command::Document(query),
            CommandReply::Document(document) | CommandReply::Page(document),
        ) => {
            if document.symbol != query.symbol()
                || document.basis() != query.basis()
                || query
                    .source_basis()
                    .is_some_and(|source| document.source_basis() != Some(source))
            {
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
        (Command::Graph(query), CommandReply::Graph(snapshot)) => {
            if !query.basis().matches(snapshot.root.basis().root) {
                return Err(ReplyAdmissionError::Protocol(
                    "graph reply source revision does not match the request".to_owned(),
                ));
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

fn command_basis(command: &Command) -> Option<ViewRevision> {
    match command {
        Command::Document(query) => Some(query.basis()),
        Command::Outline(query) => Some(query.basis()),
        Command::Name(query) => Some(query.basis()),
        Command::Search(query) => Some(query.basis()),
        Command::Graph(query) => Some(query.basis()),
        _ => None,
    }
}

fn command_cursor(command: &Command) -> Option<Cursor> {
    match command {
        Command::Name(query) => query.cursor(),
        Command::Search(query) => query.cursor(),
        _ => None,
    }
}
