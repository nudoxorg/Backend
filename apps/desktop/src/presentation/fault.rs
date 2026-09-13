//! Every typed failure, projected into one renderable shape.
//! A fault names the operand it happened to, states its cause in the exact
//! words the producer used, and offers at least one thing the reader can do.
//! Nothing in this application may say "something went wrong".
//!
//! The three parts are not decoration. The operand is what makes a failure
//! locatable — a path, a coordinate, a lane, an endpoint — and it is always
//! copyable. The cause is the producer's own typed error, never a paraphrase.
//! The affordance is the reason a fault is worth rendering at all: a failure
//! the reader cannot act on is a dead end, so every fault carries a way out,
//! even if that way out is only "copy this and retry".

use backend_library::{CommandFailure, Coverage, Lane, Reason};

/// How much of the reader's attention a fault deserves.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Severity {
    /// A deliberate absence: this deployment never had the capability.
    Note,
    /// Degraded but usable: partial coverage, a stale basis, a retry in flight.
    Caution,
    /// The operation did not happen.
    Fault,
}

/// What a failure happened to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Operand {
    /// A project folder or package coordinate being indexed or read.
    Project {
        /// Readable project name.
        name: String,
        /// Exact path or coordinate, as the producer spelled it.
        spelling: String,
    },
    /// One declaration coordinate.
    Declaration {
        /// Exact coordinate, as the producer spelled it.
        spelling: String,
    },
    /// One retrieval lane.
    Lane {
        /// The lane whose result is missing.
        lane: Lane,
    },
    /// The local service endpoint.
    Endpoint {
        /// Exact socket path.
        path: String,
    },
    /// The text the reader typed.
    Query {
        /// Exact query text.
        text: String,
    },
    /// The immutable revision the view is pinned to.
    Revision,
    /// A named capability: a compiler oracle or the embedding model.
    Capability {
        /// Readable capability name.
        name: String,
    },
}

impl Operand {
    /// Returns the short label drawn before the operand's spelling.
    pub(crate) const fn role(&self) -> &'static str {
        match self {
            Self::Project { .. } => "project",
            Self::Declaration { .. } => "declaration",
            Self::Lane { .. } => "lane",
            Self::Endpoint { .. } => "endpoint",
            Self::Query { .. } => "query",
            Self::Revision => "revision",
            Self::Capability { .. } => "capability",
        }
    }

    /// Returns the exact spelling of the operand, which is always copyable.
    pub(crate) fn spelling(&self) -> String {
        match self {
            Self::Project { spelling, .. } | Self::Declaration { spelling } => spelling.clone(),
            Self::Lane { lane } => lane_name(*lane).to_owned(),
            Self::Endpoint { path } => path.clone(),
            Self::Query { text } => text.clone(),
            Self::Revision => "current revision".to_owned(),
            Self::Capability { name } => name.clone(),
        }
    }
}

/// Something the reader can do about a fault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Affordance {
    /// Run the same operation again.
    Retry,
    /// Re-index one project from scratch.
    Reindex {
        /// Coordinate to submit.
        coordinate: String,
    },
    /// Reveal a folder in the platform file manager.
    OpenFolder {
        /// Absolute folder path.
        path: String,
    },
    /// Copy exact text to the clipboard.
    Copy {
        /// Button label.
        label: String,
        /// Text placed on the clipboard.
        text: String,
    },
    /// Reconnect to the local service.
    Reconnect,
    /// Re-read the current revision and repeat the request.
    Refresh,
    /// Take the project off the shelf.
    Remove {
        /// Coordinate to remove.
        coordinate: String,
    },
    /// Open the settings view at the capability section.
    OpenSettings,
    /// Wait: the operation is already recovering on its own.
    Waiting,
}

impl Affordance {
    /// Returns the button label.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Retry => "Retry".to_owned(),
            Self::Reindex { .. } => "Re-index".to_owned(),
            Self::OpenFolder { .. } => "Open folder".to_owned(),
            Self::Copy { label, .. } => label.clone(),
            Self::Reconnect => "Reconnect".to_owned(),
            Self::Refresh => "Refresh".to_owned(),
            Self::Remove { .. } => "Remove".to_owned(),
            Self::OpenSettings => "Capabilities".to_owned(),
            Self::Waiting => "Recovering…".to_owned(),
        }
    }

    /// Returns whether this affordance runs an action or only reports state.
    pub(crate) const fn is_actionable(&self) -> bool {
        !matches!(self, Self::Waiting)
    }
}

/// One renderable failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fault {
    severity: Severity,
    operand: Operand,
    headline: String,
    cause: String,
    affordances: Vec<Affordance>,
}

impl Fault {
    /// Composes a fault from its three required parts.
    pub(crate) fn new(
        severity: Severity,
        operand: Operand,
        headline: impl Into<String>,
        cause: impl Into<String>,
        affordances: Vec<Affordance>,
    ) -> Self {
        let mut affordances = affordances;
        if affordances.is_empty() {
            affordances.push(Affordance::Retry);
        }
        Self {
            severity,
            operand,
            headline: headline.into(),
            cause: cause.into(),
            affordances,
        }
    }

    /// Returns how much attention this fault deserves.
    pub(crate) const fn severity(&self) -> Severity {
        self.severity
    }

    /// Returns what the failure happened to.
    pub(crate) const fn operand(&self) -> &Operand {
        &self.operand
    }

    /// Returns the one-line statement of what did not happen.
    pub(crate) fn headline(&self) -> &str {
        &self.headline
    }

    /// Returns the producer's own words for why.
    pub(crate) fn cause(&self) -> &str {
        &self.cause
    }

    /// Returns the things the reader can do, in order of usefulness.
    pub(crate) fn affordances(&self) -> &[Affordance] {
        &self.affordances
    }

    /// Adds an affordance that the call site knows about and the cause does not.
    pub(crate) fn with(mut self, affordance: Affordance) -> Self {
        if !self.affordances.contains(&affordance) {
            self.affordances.push(affordance);
        }
        self
    }
}

/// Projects a shared-client failure against the operand it happened to.
pub(crate) fn from_client(error: &backend_client::ClientError, operand: Operand) -> Fault {
    match error {
        backend_client::ClientError::CommandFailed(failure) => from_failure(failure, operand),
        backend_client::ClientError::Io(message) => Fault::new(
            Severity::Fault,
            operand,
            "The local service did not answer",
            message.clone(),
            vec![Affordance::Reconnect, Affordance::Retry],
        ),
        backend_client::ClientError::Protocol(message) => from_protocol(message, operand),
        backend_client::ClientError::Transport(transport) => Fault::new(
            Severity::Fault,
            operand,
            "The reply exceeded a transport bound",
            transport.to_string(),
            vec![Affordance::Retry],
        ),
        backend_client::ClientError::IncoherentView => Fault::new(
            Severity::Fault,
            operand,
            "The service returned a view it could not vouch for",
            "the reply's rows did not match its own committed root",
            vec![Affordance::Refresh, Affordance::Retry],
        ),
        backend_client::ClientError::BasisMismatch { .. } => basis_fault(operand),
        backend_client::ClientError::FreshnessMismatch => Fault::new(
            Severity::Caution,
            operand,
            "The reply did not prove it was current",
            "the service answered without a freshness proof for this revision",
            vec![Affordance::Refresh],
        ),
        backend_client::ClientError::RequestMismatch { expected, observed } => Fault::new(
            Severity::Fault,
            operand,
            "A reply arrived out of order",
            format!("request {observed} answered while {expected} was outstanding"),
            vec![Affordance::Reconnect, Affordance::Retry],
        ),
        backend_client::ClientError::CursorMismatch => Fault::new(
            Severity::Caution,
            operand,
            "The live feed lost its place",
            "the service returned a cursor that does not follow the one held here",
            vec![Affordance::Waiting, Affordance::Reconnect],
        ),
    }
}

/// Projects a closed application failure.
pub(crate) fn from_failure(failure: &CommandFailure, operand: Operand) -> Fault {
    match failure {
        CommandFailure::NotFound => Fault::new(
            Severity::Fault,
            operand.clone(),
            "Nothing on the shelf has this identity",
            format!("no record answers to this {}", operand.role()),
            vec![
                Affordance::Copy {
                    label: "Copy identity".to_owned(),
                    text: operand.spelling(),
                },
                Affordance::Refresh,
            ],
        ),
        CommandFailure::WrongBasis { .. } => basis_fault(operand),
        CommandFailure::InvalidQuery(message) => Fault::new(
            Severity::Caution,
            operand,
            "The service would not run this query",
            message.clone(),
            vec![Affordance::Retry],
        ),
        CommandFailure::CursorMismatch => Fault::new(
            Severity::Caution,
            operand,
            "This page no longer follows the one before it",
            "the continuation belongs to a different revision or query",
            vec![Affordance::Refresh],
        ),
        CommandFailure::IncoherentView(message) => Fault::new(
            Severity::Caution,
            operand,
            "The view is being rebuilt",
            message.clone(),
            vec![Affordance::Waiting, Affordance::Refresh],
        ),
        CommandFailure::SequenceOverflow => Fault::new(
            Severity::Fault,
            operand,
            "The service ran out of sequence space",
            "the owner cannot represent another event and must be restarted",
            vec![Affordance::Reconnect],
        ),
        CommandFailure::MutationRequiresOwner => Fault::new(
            Severity::Fault,
            operand,
            "This change needs the durable owner",
            "the request reached a reader rather than the process that owns the workspace",
            vec![Affordance::Reconnect, Affordance::Retry],
        ),
    }
}

/// Projects the desktop's own subscription failure.
pub(crate) fn from_subscription(error: &crate::transport::error::ClientError, endpoint: &str) -> Fault {
    let operand = Operand::Endpoint {
        path: endpoint.to_owned(),
    };
    match error {
        crate::transport::error::ClientError::Io(message) => Fault::new(
            Severity::Fault,
            operand,
            "The live feed disconnected",
            message.clone(),
            vec![Affordance::Waiting, Affordance::Reconnect],
        ),
        crate::transport::error::ClientError::Protocol(message) => Fault::new(
            Severity::Fault,
            operand,
            "The live feed sent something this build cannot admit",
            message.clone(),
            vec![Affordance::Reconnect],
        ),
        crate::transport::error::ClientError::Transport(transport) => Fault::new(
            Severity::Fault,
            operand,
            "A live batch exceeded its credit",
            transport.to_string(),
            vec![Affordance::Reconnect],
        ),
        crate::transport::error::ClientError::IncoherentRoot => Fault::new(
            Severity::Fault,
            operand,
            "The live feed offered a root it could not vouch for",
            "the replacement root arrived without producer-admitted complete coverage",
            vec![Affordance::Reconnect],
        ),
        crate::transport::error::ClientError::BasisMismatch { .. } => Fault::new(
            Severity::Caution,
            operand,
            "The live feed moved to a different source",
            "the service rebased onto another source stream; a full re-read is in flight",
            vec![Affordance::Waiting],
        ),
        crate::transport::error::ClientError::CursorMismatch => Fault::new(
            Severity::Caution,
            operand,
            "The live feed lost its place",
            "the next batch did not follow the cursor held here; hydrating from a fresh snapshot",
            vec![Affordance::Waiting],
        ),
    }
}

/// Projects an honest lane result that is not complete.
///
/// A complete lane yields no fault at all: absence of results is a result.
pub(crate) fn from_coverage(coverage: Coverage) -> Option<Fault> {
    match coverage {
        Coverage::Complete => None,
        Coverage::Partial {
            lane,
            completed,
            total,
        } => Some(Fault::new(
            Severity::Caution,
            Operand::Lane { lane },
            format!("{} results are incomplete", lane_name(lane)),
            format!("{completed} of {total} shards answered before the bound was reached"),
            vec![Affordance::Retry],
        )),
        Coverage::Unavailable { lane, reason } => Some(unavailable_lane(lane, reason)),
    }
}

fn unavailable_lane(lane: Lane, reason: Reason) -> Fault {
    let (severity, cause, affordance) = match reason {
        Reason::NoIndex => (
            Severity::Caution,
            "nothing has been indexed for this lane yet",
            Affordance::Retry,
        ),
        Reason::Unconfigured => (
            Severity::Note,
            "this build has no provider configured for the lane",
            Affordance::OpenSettings,
        ),
        Reason::Offline => (
            Severity::Caution,
            "the lane needs remote work and this host is offline",
            Affordance::Retry,
        ),
        Reason::Cancelled => (
            Severity::Caution,
            "the lane was cancelled before it produced a complete result",
            Affordance::Retry,
        ),
        Reason::Incomplete => (
            Severity::Caution,
            "the source facts this lane needs are not complete",
            Affordance::Retry,
        ),
    };
    Fault::new(
        severity,
        Operand::Lane { lane },
        format!("No {} results", lane_name(lane)),
        cause,
        vec![affordance],
    )
}

fn from_protocol(message: &str, operand: Operand) -> Fault {
    if message.contains("relation node was rejected") {
        return engine_rejection(message, operand);
    }
    Fault::new(
        Severity::Fault,
        operand,
        "The service rejected this request",
        message.to_owned(),
        vec![Affordance::Retry],
    )
}

fn engine_rejection(message: &str, operand: Operand) -> Fault {
    let mut affordances = vec![Affordance::Copy {
        label: "Copy report".to_owned(),
        text: format!("{}: {message}", operand.spelling()),
    }];
    if let Operand::Project { spelling, .. } = &operand {
        affordances.push(Affordance::Reindex {
            coordinate: spelling.clone(),
        });
        affordances.push(Affordance::OpenFolder {
            path: spelling.clone(),
        });
    }
    Fault::new(
        Severity::Fault,
        operand,
        "Indexing stopped inside the engine",
        format!("{message} — this build cannot commit the workspace relation for this project"),
        affordances,
    )
}

fn basis_fault(operand: Operand) -> Fault {
    Fault::new(
        Severity::Caution,
        operand,
        "The shelf moved while this was being read",
        "the request was pinned to a revision the service has already replaced",
        vec![Affordance::Refresh, Affordance::Retry],
    )
}

/// Returns the reader-facing name of one retrieval lane.
pub(crate) const fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Exact => "exact",
        Lane::Names => "names",
        Lane::Graph => "graph",
        Lane::Semantic => "semantic",
    }
}

/// Returns the reader-facing name of one unavailability reason.
pub(crate) const fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "no index",
        Reason::Unconfigured => "unconfigured",
        Reason::Offline => "offline",
        Reason::Cancelled => "cancelled",
        Reason::Incomplete => "incomplete",
    }
}
