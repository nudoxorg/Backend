//! Project typed engine failures into surface faults.

use super::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use crate::coverage::lane_name;
use crate::identity::KeyTag;
use backend_client::ClientError;
use backend_library::{
    CommandFailure, Coverage, Lane, ProductAdmissionError, Reason, SourceAvailability,
    SourceExcerpt, ViewRevision,
};

impl Fault {
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
            ClientError::StaleCursor => Self::new(
                FaultSlug::CursorMismatch,
                operand,
                Cause::new(
                    CauseSlug::Moved,
                    "the continuation belongs to an older immutable revision; restart the query",
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
        (moved.to_string() == message)
            .then(|| Self::from_command_failure(&moved, operand.clone()).about(operand.clone()))
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
            ClientError::StaleCursor => (
                FaultSlug::CursorMismatch,
                CauseSlug::Moved,
                "the continuation belongs to an older immutable revision; restart the query"
                    .to_owned(),
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
        Reason::NoIndex => {
            format!("the {lane} lane has no local materialization for this revision")
        }
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
