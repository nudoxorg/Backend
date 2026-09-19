//! One typed value for every failure a product surface can observe.
//!
//! A fault is content, not a log line. It always names three things:
//!
//! ```text
//! ✗ not-found /abs/project::src/lib.rs:999::nothing
//!   no declaration is published at that coordinate in this revision
//!   → backend search nothing
//! ```
//!
//! * the **slug** — a closed, greppable identity for the failure class;
//! * the **operand** — the exact typed thing that failed, never elided;
//! * the **cause** — one sentence a reader can act on, with its own slug;
//! * the **affordance** — the next step, rendered by each surface in its own
//!   idiom (a shell command for the CLI, a tool call for MCP, a button for a
//!   desktop) from the same typed value.
//!
//! Every constructor here starts from an already-typed engine value, so a
//! surface cannot invent a failure the engine did not report, and cannot lose
//! the operand on the way to the reader.

use crate::coverage::lane_name;
use crate::identity::{Coordinate, KeyTag};
use backend_client::ClientError;
use backend_library::{
    CommandFailure, Coverage, Lane, ProductAdmissionError, Reason, SourceAvailability,
    SourceExcerpt, ViewRevision,
};
use core::fmt;

/// The closed failure classes a surface can render.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FaultSlug {
    /// The requested package, symbol, or document does not exist.
    NotFound,
    /// The request was pinned to a revision the owner has moved past.
    WrongBasis,
    /// The query failed bounded semantic validation.
    InvalidQuery,
    /// A continuation cursor belongs to another recipe or revision.
    CursorMismatch,
    /// The retained view could not satisfy an invariant.
    IncoherentView,
    /// The owner cannot represent another event in its sequence space.
    SequenceOverflow,
    /// The operation must be submitted to the durable engine owner.
    MutationRequiresOwner,
    /// The local endpoint could not be reached or used.
    Endpoint,
    /// A frame, DTO, or identity proof failed admission.
    Protocol,
    /// A bounded transport allocation was rejected.
    Transport,
    /// The daemon answered a different request.
    RequestMismatch,
    /// A reply did not prove accepted freshness.
    Freshness,
    /// A retrieval lane produced no authoritative result.
    LaneUnavailable,
    /// A retrieval lane completed only part of its declared scope.
    LanePartial,
    /// The requested source text is not available.
    SourceUnavailable,
    /// The engine rejected the intent behind an otherwise valid request.
    Rejected,
    /// The caller's arguments did not match the command grammar.
    Usage,
}

impl FaultSlug {
    /// Returns the stable kebab-case slug shared by every surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::WrongBasis => "wrong-basis",
            Self::InvalidQuery => "invalid-query",
            Self::CursorMismatch => "cursor-mismatch",
            Self::IncoherentView => "incoherent-view",
            Self::SequenceOverflow => "sequence-overflow",
            Self::MutationRequiresOwner => "mutation-requires-owner",
            Self::Endpoint => "endpoint",
            Self::Protocol => "protocol",
            Self::Transport => "transport",
            Self::RequestMismatch => "request-mismatch",
            Self::Freshness => "freshness",
            Self::LaneUnavailable => "lane-unavailable",
            Self::LanePartial => "lane-partial",
            Self::SourceUnavailable => "source-unavailable",
            Self::Rejected => "rejected",
            Self::Usage => "usage",
        }
    }

    /// Returns whether this class means the request itself was malformed.
    #[must_use]
    pub const fn is_usage(self) -> bool {
        matches!(self, Self::Usage)
    }
}

impl fmt::Display for FaultSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The exact typed thing one fault is about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operand {
    /// A declaration or package coordinate.
    Coordinate(Coordinate),
    /// A filesystem path, such as a project root or endpoint.
    Path(String),
    /// One retrieval lane.
    Lane(Lane),
    /// A revision the caller pinned, and the one the owner holds.
    Basis {
        /// Revision the owner currently holds.
        expected: KeyTag,
        /// Revision the request was pinned to.
        observed: KeyTag,
    },
    /// A single revision.
    Revision(KeyTag),
    /// The name of a command or argument the caller supplied.
    Argument(String),
    /// Free text the engine returned as the operand.
    Text(String),
    /// The failure is about the request as a whole.
    Whole,
}

impl Operand {
    /// Renders the operand exactly, never abbreviated and never wrapped.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Coordinate(coordinate) => coordinate.as_str().to_owned(),
            Self::Path(path) | Self::Text(path) | Self::Argument(path) => path.clone(),
            Self::Lane(lane) => lane_name(*lane).to_owned(),
            Self::Basis { expected, observed } => format!("{observed} (owner holds {expected})"),
            Self::Revision(revision) => revision.to_string(),
            Self::Whole => String::new(),
        }
    }
}

/// Why the failure happened, as a closed slug and one readable sentence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Cause {
    slug: CauseSlug,
    sentence: String,
}

impl Cause {
    /// Pairs a closed cause slug with the sentence a reader sees.
    #[must_use]
    pub fn new(slug: CauseSlug, sentence: impl Into<String>) -> Self {
        Self {
            slug,
            sentence: sentence.into(),
        }
    }

    /// Returns the closed cause slug.
    #[must_use]
    pub const fn slug(&self) -> CauseSlug {
        self.slug
    }

    /// Returns the readable sentence.
    #[must_use]
    pub fn sentence(&self) -> &str {
        &self.sentence
    }
}

/// The closed set of reasons a fault happened.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CauseSlug {
    /// Nothing is published at the named identity in this revision.
    Absent,
    /// The owner's revision moved while the request was in flight.
    Moved,
    /// The request did not satisfy the command grammar.
    Malformed,
    /// The deployment never configured the capability the request needs.
    Unconfigured,
    /// The work is still in progress.
    Indexing,
    /// The local daemon could not be reached.
    Unreachable,
    /// The payload exceeded a bounded allocation.
    Oversized,
    /// A producer proof did not admit.
    Unproven,
    /// The source bytes exist but are not resident locally.
    NotResident,
    /// The producer never captured the requested evidence.
    NotCaptured,
    /// The engine refused the intent behind the request.
    Refused,
}

impl CauseSlug {
    /// Returns the stable kebab-case slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Moved => "moved",
            Self::Malformed => "malformed",
            Self::Unconfigured => "unconfigured",
            Self::Indexing => "indexing",
            Self::Unreachable => "unreachable",
            Self::Oversized => "oversized",
            Self::Unproven => "unproven",
            Self::NotResident => "not-resident",
            Self::NotCaptured => "not-captured",
            Self::Refused => "refused",
        }
    }
}

impl fmt::Display for CauseSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The next step a reader can take, typed so each surface renders its idiom.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Affordance {
    /// Run the same request again.
    Retry,
    /// Index or refresh one project path.
    Reindex {
        /// Project path to index.
        path: String,
    },
    /// Open one path in the reader's file browser.
    OpenFolder {
        /// Path to reveal.
        path: String,
    },
    /// Run another product command.
    UseCommand {
        /// Registry command name.
        name: &'static str,
        /// Positional operands in grammar order.
        args: Box<[String]>,
    },
    /// Wait for the current index to reach readiness.
    WaitForReadiness,
    /// Nothing the reader can do changes the outcome.
    None,
}

impl Affordance {
    /// Suggests re-reading the shelf status.
    #[must_use]
    pub fn status() -> Self {
        Self::UseCommand {
            name: "health",
            args: Box::new([]),
        }
    }

    /// Suggests searching for the text the caller addressed.
    #[must_use]
    pub fn search(text: impl Into<String>) -> Self {
        Self::UseCommand {
            name: "search",
            args: Box::new([text.into()]),
        }
    }

    /// Renders the affordance as the exact shell command a CLI reader runs.
    #[must_use]
    pub fn shell(&self) -> Option<String> {
        match self {
            Self::Retry => Some("re-run the same command".to_owned()),
            Self::Reindex { path } => Some(format!("backend index {}", quote(path))),
            Self::OpenFolder { path } => Some(format!("open {}", quote(path))),
            Self::UseCommand { name, args } => {
                let mut line = format!("backend {name}");
                for argument in args {
                    line.push(' ');
                    line.push_str(&quote(argument));
                }
                Some(line)
            }
            Self::WaitForReadiness => Some("backend health  (wait for ~lanes to read ready)".to_owned()),
            Self::None => None,
        }
    }

    /// Renders the affordance as the exact MCP tool call an agent makes next.
    #[must_use]
    pub fn tool_call(&self) -> Option<serde_json::Value> {
        match self {
            Self::Retry | Self::None => None,
            Self::Reindex { path } => Some(serde_json::json!({
                "name": "backend.index",
                "arguments": { "path": path }
            })),
            Self::OpenFolder { path } => Some(serde_json::json!({
                "name": "backend.packages",
                "arguments": { "path": path }
            })),
            Self::UseCommand { name, args } => Some(serde_json::json!({
                "name": format!("backend.{}", name.replace('-', "_")),
                "arguments": tool_arguments(name, args)
            })),
            Self::WaitForReadiness => Some(serde_json::json!({
                "name": "backend.status",
                "arguments": {}
            })),
        }
    }
}

fn tool_arguments(name: &str, args: &[String]) -> serde_json::Value {
    let field = match name {
        "search" | "index-search" | "resolve" => "query",
        "outline" | "packages" | "add" | "remove" => "path",
        _ => "coordinate",
    };
    args.first().map_or_else(
        || serde_json::json!({}),
        |value| serde_json::json!({ field: value }),
    )
}

fn quote(value: &str) -> String {
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_owned()
    }
}

/// One typed failure, complete enough to render on any surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fault {
    slug: FaultSlug,
    operand: Operand,
    cause: Cause,
    affordance: Affordance,
}

impl Fault {
    /// Assembles one fault from its typed parts.
    #[must_use]
    pub const fn new(
        slug: FaultSlug,
        operand: Operand,
        cause: Cause,
        affordance: Affordance,
    ) -> Self {
        Self {
            slug,
            operand,
            cause,
            affordance,
        }
    }

    /// Returns the closed failure class.
    #[must_use]
    pub const fn slug(&self) -> FaultSlug {
        self.slug
    }

    /// Returns the exact typed operand.
    #[must_use]
    pub const fn operand(&self) -> &Operand {
        &self.operand
    }

    /// Returns the cause slug and sentence.
    #[must_use]
    pub const fn cause(&self) -> &Cause {
        &self.cause
    }

    /// Returns the next step a reader can take.
    #[must_use]
    pub const fn affordance(&self) -> &Affordance {
        &self.affordance
    }

    /// Returns a fault with a different affordance, keeping its identity.
    #[must_use]
    pub fn with_affordance(mut self, affordance: Affordance) -> Self {
        self.affordance = affordance;
        self
    }

    /// Returns a fault whose operand names the coordinate the caller supplied.
    #[must_use]
    pub fn about(mut self, operand: Operand) -> Self {
        self.operand = operand;
        self
    }

    /// Builds the fault for one malformed argument.
    #[must_use]
    pub fn usage(argument: impl Into<String>, sentence: impl Into<String>) -> Self {
        Self::new(
            FaultSlug::Usage,
            Operand::Argument(argument.into()),
            Cause::new(CauseSlug::Malformed, sentence),
            Affordance::None,
        )
    }

    /// Builds the fault for a lane that produced no authoritative result.
    #[must_use]
    pub fn lane(lane: Lane, reason: Reason) -> Self {
        let affordance = match reason {
            Reason::NoIndex | Reason::Incomplete => Affordance::WaitForReadiness,
            Reason::Unconfigured => Affordance::None,
            Reason::Offline | Reason::Cancelled => Affordance::Retry,
        };
        Self::new(
            FaultSlug::LaneUnavailable,
            Operand::Lane(lane),
            Cause::new(lane_cause(reason), lane_sentence(lane, reason)),
            affordance,
        )
    }

    /// Builds the fault for one bounded lane result, when there is one.
    #[must_use]
    pub fn from_coverage(coverage: Coverage) -> Option<Self> {
        match coverage {
            Coverage::Complete => None,
            Coverage::Unavailable { lane, reason } => Some(Self::lane(lane, reason)),
            Coverage::Partial {
                lane,
                completed,
                total,
            } => Some(Self::new(
                FaultSlug::LanePartial,
                Operand::Lane(lane),
                Cause::new(
                    CauseSlug::Indexing,
                    format!(
                        "the {} lane finished {completed} of {total} shards for this revision",
                        lane_name(lane)
                    ),
                ),
                Affordance::WaitForReadiness,
            )),
        }
    }

    /// Builds the fault for source text a reader asked for and cannot have.
    #[must_use]
    pub fn source(availability: &SourceAvailability, operand: Operand) -> Option<Self> {
        let (cause, sentence, affordance) = match availability {
            SourceAvailability::Captured(_) => return None,
            SourceAvailability::NotCaptured => (
                CauseSlug::NotCaptured,
                "the producer retained no source span for this declaration",
                Affordance::None,
            ),
            SourceAvailability::NotHydrated => (
                CauseSlug::NotResident,
                "a source span exists, but its bytes are not resident locally",
                Affordance::WaitForReadiness,
            ),
            SourceAvailability::Unconfigured => (
                CauseSlug::Unconfigured,
                "this deployment has no source provider for the declaration's origin",
                Affordance::None,
            ),
        };
        Some(Self::new(
            FaultSlug::SourceUnavailable,
            operand,
            Cause::new(cause, sentence),
            affordance,
        ))
    }

    /// Builds the fault for excerpt text a reader asked for and cannot have.
    #[must_use]
    pub fn excerpt(excerpt: &SourceExcerpt, operand: Operand) -> Option<Self> {
        let availability = match excerpt {
            SourceExcerpt::Captured { .. } => return None,
            SourceExcerpt::NotCaptured => SourceAvailability::NotCaptured,
            SourceExcerpt::NotHydrated => SourceAvailability::NotHydrated,
            SourceExcerpt::Unconfigured => SourceAvailability::Unconfigured,
        };
        Self::source(&availability, operand)
    }

    /// Lowers one closed application-service failure into a fault.
    ///
    /// The operand is supplied by the caller because only the caller knows
    /// which coordinate, path, or argument its request was about; the engine
    /// reports the class, not the thing.
    #[must_use]
    pub fn from_command_failure(failure: &CommandFailure, operand: Operand) -> Self {
        let (slug, cause, sentence, affordance) = match failure {
            CommandFailure::NotFound => (
                FaultSlug::NotFound,
                CauseSlug::Absent,
                "no record is published at that identity in this revision".to_owned(),
                Affordance::None,
            ),
            CommandFailure::WrongBasis { .. } => (
                FaultSlug::WrongBasis,
                CauseSlug::Moved,
                "the owner published a newer revision while this request was in flight".to_owned(),
                Affordance::Retry,
            ),
            CommandFailure::InvalidQuery(detail) => (
                FaultSlug::InvalidQuery,
                CauseSlug::Malformed,
                detail.clone(),
                Affordance::None,
            ),
            CommandFailure::CursorMismatch => (
                FaultSlug::CursorMismatch,
                CauseSlug::Moved,
                "the continuation cursor belongs to another recipe, revision, or position"
                    .to_owned(),
                Affordance::Retry,
            ),
            CommandFailure::IncoherentView(detail) => (
                FaultSlug::IncoherentView,
                CauseSlug::Unproven,
                format!("the retained view could not satisfy an invariant: {detail}"),
                Affordance::WaitForReadiness,
            ),
            CommandFailure::SequenceOverflow => (
                FaultSlug::SequenceOverflow,
                CauseSlug::Refused,
                "the owner cannot represent another event in its sequence space".to_owned(),
                Affordance::None,
            ),
            CommandFailure::MutationRequiresOwner => (
                FaultSlug::MutationRequiresOwner,
                CauseSlug::Refused,
                "this operation must be submitted to the durable engine owner".to_owned(),
                Affordance::None,
            ),
        };
        let operand = match failure {
            CommandFailure::WrongBasis { expected, observed } => Operand::Basis {
                expected: revision_tag(*expected),
                observed: revision_tag(*observed),
            },
            _ => operand,
        };
        Self::new(slug, operand, Cause::new(cause, sentence), affordance)
    }

    /// Lowers one shared-client transport or admission failure into a fault.
    #[must_use]
    pub fn from_client_error(error: &ClientError, operand: Operand) -> Self {
        match error {
            ClientError::CommandFailed(failure) => Self::from_command_failure(failure, operand),
            ClientError::Protocol(message) => Self::recovered(message, &operand)
                .unwrap_or_else(|| Self::from_simple_client_error(error, operand)),
            ClientError::BasisMismatch { expected, observed } => Self::new(
                FaultSlug::WrongBasis,
                Operand::Basis {
                    expected: KeyTag::from_key(expected.as_bytes()),
                    observed: KeyTag::from_key(observed.as_bytes()),
                },
                Cause::new(
                    CauseSlug::Moved,
                    "the reply is based on a different materialized revision",
                ),
                Affordance::Retry,
            ),
            ClientError::RequestMismatch { expected, observed } => Self::new(
                FaultSlug::RequestMismatch,
                Operand::Text(format!("request {observed} answered request {expected}")),
                Cause::new(
                    CauseSlug::Unproven,
                    "the daemon answered a request this session did not send",
                ),
                Affordance::Retry,
            ),
            other => Self::from_simple_client_error(other, operand),
        }
    }

    /// Recovers a typed refusal that a peer flattened into a message.
    ///
    /// `CommandReply::Error` is the pre-typed reply schema, and a producer that
    /// still answers with it sends only the failure's own `Display` text. The
    /// shared client can do nothing but call that a protocol failure — so the
    /// commonest refusal an agent can provoke, asking for a coordinate no
    /// revision publishes, reaches a reader as *"a frame, DTO, or identity
    /// proof failed admission: library record not found"*. That is both wrong
    /// and unactionable.
    ///
    /// The comparison below is against [`CommandFailure`]'s own rendering, not
    /// against a spelling invented here, so it cannot silently stop matching if
    /// the library rewords a failure: the candidate text is computed from the
    /// same value the producer stringified. A message that matches nothing stays
    /// a protocol failure, which is the honest answer for a peer this model does
    /// not understand.
    ///
    /// The real repair belongs to the producer — `CommandFailure` already
    /// converts from the library's error type — and this recovery becomes dead
    /// weight the day it lands. It is here because a surface must not print an
    /// admission failure at a person who mistyped a name.
    fn recovered(message: &str, operand: &Operand) -> Option<Self> {
        let closed = [
            CommandFailure::NotFound,
            CommandFailure::CursorMismatch,
            CommandFailure::SequenceOverflow,
            CommandFailure::MutationRequiresOwner,
        ];
        if let Some(failure) = closed
            .into_iter()
            .find(|failure| failure.to_string() == message)
        {
            return Some(Self::from_command_failure(&failure, operand.clone()));
        }
        let invalid = CommandFailure::InvalidQuery(String::new()).to_string();
        if let Some(detail) = message.strip_prefix(&invalid) {
            return Some(Self::from_command_failure(
                &CommandFailure::InvalidQuery(detail.to_owned()),
                operand.clone(),
            ));
        }
        // A flattened wrong basis lost both revisions on the way out, so the
        // operand stays the one the caller supplied rather than two invented
        // digests.
        let moved = CommandFailure::WrongBasis {
            expected: ViewRevision::from(backend_library::view_state_root(&[])),
            observed: ViewRevision::from(backend_library::view_state_root(&[])),
        };
        (moved.to_string() == message).then(|| {
            Self::from_command_failure(&moved, operand.clone()).about(operand.clone())
        })
    }

    fn from_simple_client_error(error: &ClientError, operand: Operand) -> Self {
        let (slug, cause, sentence, affordance) = match error {
            ClientError::Io(detail) => (
                FaultSlug::Endpoint,
                CauseSlug::Unreachable,
                format!("the local endpoint could not be used: {detail}"),
                Affordance::Retry,
            ),
            ClientError::Disconnected(kind) => (
                FaultSlug::Endpoint,
                CauseSlug::Unreachable,
                format!(
                    "the local endpoint closed this connection ({kind}); \
                     a fresh connection was opened, so retry the command"
                ),
                Affordance::Retry,
            ),
            ClientError::Protocol(detail) => (
                FaultSlug::Protocol,
                CauseSlug::Unproven,
                format!("a frame, DTO, or identity proof failed admission: {detail}"),
                Affordance::None,
            ),
            ClientError::Transport(detail) => (
                FaultSlug::Transport,
                CauseSlug::Oversized,
                format!("a bounded transport allocation was rejected: {detail}"),
                Affordance::None,
            ),
            ClientError::IncoherentView => (
                FaultSlug::IncoherentView,
                CauseSlug::Unproven,
                "the daemon returned a view that is not internally coherent".to_owned(),
                Affordance::WaitForReadiness,
            ),
            ClientError::FreshnessMismatch => (
                FaultSlug::Freshness,
                CauseSlug::Unproven,
                "the reply did not prove the freshness this request required".to_owned(),
                Affordance::Retry,
            ),
            _ => (
                FaultSlug::CursorMismatch,
                CauseSlug::Moved,
                "the daemon returned a continuation that does not identify its root".to_owned(),
                Affordance::Retry,
            ),
        };
        Self::new(slug, operand, Cause::new(cause, sentence), affordance)
    }

    /// Builds the fault for one rejected product operand.
    #[must_use]
    pub fn admission(error: ProductAdmissionError, operand: Operand) -> Self {
        Self::new(
            FaultSlug::Usage,
            operand,
            Cause::new(CauseSlug::Malformed, error.to_string()),
            Affordance::None,
        )
    }

    /// Renders the shared three-line grammar with an explicit affordance line.
    #[must_use]
    pub fn render(&self, affordance: Option<&str>) -> String {
        let operand = self.operand.render();
        let mut out = if operand.is_empty() {
            format!("✗ {}", self.slug)
        } else {
            format!("✗ {} {operand}", self.slug)
        };
        out.push('\n');
        out.push_str("  ");
        out.push_str(self.cause.sentence());
        if let Some(affordance) = affordance {
            out.push('\n');
            out.push_str("  → ");
            out.push_str(affordance);
        }
        out
    }
}

const fn lane_cause(reason: Reason) -> CauseSlug {
    match reason {
        Reason::NoIndex => CauseSlug::Absent,
        Reason::Unconfigured => CauseSlug::Unconfigured,
        Reason::Offline => CauseSlug::Unreachable,
        Reason::Cancelled => CauseSlug::Refused,
        Reason::Incomplete => CauseSlug::Indexing,
    }
}

fn lane_sentence(lane: Lane, reason: Reason) -> String {
    let lane = lane_name(lane);
    match reason {
        Reason::NoIndex => format!("the {lane} lane has no local materialization for this revision"),
        Reason::Unconfigured => {
            format!("this deployment did not configure the {lane} lane, so it answered nothing")
        }
        Reason::Offline => format!("the {lane} lane needs remote work and this host is offline"),
        Reason::Cancelled => {
            format!("the {lane} lane was cancelled before it produced a complete result")
        }
        Reason::Incomplete => {
            format!("the source facts behind the {lane} lane are not complete yet")
        }
    }
}

fn revision_tag(revision: ViewRevision) -> KeyTag {
    KeyTag::from_key(revision.as_bytes())
}
