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
use backend_library::Lane;
use core::fmt;

mod project;

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

