//! How this window weighs, titles, and acts on a shared fault.
//!
//! The fault *value* is [`backend_present::Fault`]: one closed slug, the exact
//! typed operand, one cause sentence in the producer's own words, and one
//! typed affordance. That is the same value `backend search` prints and the
//! same one an MCP tool returns, which is the point — a reader who sees
//! `endpoint` in this window and `endpoint` in a terminal is being told the
//! same thing about the same failure.
//!
//! Three things are added here, and only three. A **severity**, because a
//! window has colour and a terminal does not, and `unconfigured` deserves less
//! ink than `the daemon is gone`. A **headline**, because a window has room
//! for one line above the cause and a three-line grammar does not include one.
//! And the desktop's own subscription failures, which are not
//! [`backend_client::ClientError`] values at all — the certified lease loop has
//! its own error type — lowered into the same shared shape so a dropped feed
//! and a dropped request render identically.

use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};

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

/// Returns how loudly one fault should be drawn.
///
/// The cause decides, not the class: a lane that is *unconfigured* is a
/// standing fact about this build and should read as a note, while the same
/// lane being *unreachable* is a failure happening right now.
pub(crate) const fn severity(fault: &Fault) -> Severity {
    match fault.cause().slug() {
        CauseSlug::Unconfigured | CauseSlug::NotCaptured => Severity::Note,
        CauseSlug::Moved | CauseSlug::Indexing | CauseSlug::NotResident => Severity::Caution,
        CauseSlug::Absent
        | CauseSlug::Malformed
        | CauseSlug::Unreachable
        | CauseSlug::Oversized
        | CauseSlug::Unproven
        | CauseSlug::Refused => Severity::Fault,
    }
}

/// Returns the one-line statement of what did not happen.
pub(crate) const fn headline(fault: &Fault) -> &'static str {
    match fault.slug() {
        FaultSlug::NotFound => "Nothing on the shelf has this identity",
        FaultSlug::WrongBasis => "The shelf moved while this was being read",
        FaultSlug::InvalidQuery => "The service would not run this query",
        FaultSlug::CursorMismatch => "This page no longer follows the one before it",
        FaultSlug::IncoherentView => "The view is being rebuilt",
        FaultSlug::SequenceOverflow => "The service ran out of sequence space",
        FaultSlug::MutationRequiresOwner => "This change needs the durable owner",
        FaultSlug::Endpoint => "The local service did not answer",
        FaultSlug::Protocol => "The service sent something this build cannot admit",
        FaultSlug::Transport => "The reply exceeded a transport bound",
        FaultSlug::RequestMismatch => "A reply arrived out of order",
        FaultSlug::Freshness => "The reply did not prove it was current",
        FaultSlug::LaneUnavailable => "This lane produced no result",
        FaultSlug::LanePartial => "These results are incomplete",
        FaultSlug::SourceUnavailable => "No source text is available",
        FaultSlug::Rejected => "The engine refused this",
        FaultSlug::Usage => "This request does not match the command grammar",
    }
}

/// Returns the short label drawn before an operand's spelling.
pub(crate) const fn operand_role(operand: &Operand) -> &'static str {
    match operand {
        Operand::Coordinate(_) => "coordinate",
        Operand::Path(_) => "path",
        Operand::Lane(_) => "lane",
        Operand::Basis { .. } => "basis",
        Operand::Revision(_) => "revision",
        Operand::Argument(_) => "argument",
        Operand::Text(_) => "detail",
        Operand::Whole => "request",
    }
}

/// Returns the exact operand spelling, which is always copyable.
pub(crate) fn operand_spelling(operand: &Operand) -> String {
    operand.render()
}

/// Returns whether one affordance draws a button at all.
pub(crate) const fn is_actionable(affordance: &Affordance) -> bool {
    !matches!(affordance, Affordance::None | Affordance::WaitForReadiness)
}

/// Returns the button label for one typed affordance.
pub(crate) fn affordance_label(affordance: &Affordance) -> String {
    match affordance {
        Affordance::Retry => "Retry".to_owned(),
        Affordance::Reindex { .. } => "Re-index".to_owned(),
        Affordance::OpenFolder { .. } => "Open folder".to_owned(),
        Affordance::UseCommand { name, .. } => format!("Run {name}"),
        Affordance::WaitForReadiness => "Recovering…".to_owned(),
        Affordance::None => "No action".to_owned(),
    }
}

/// Returns the omnibar text that runs one `UseCommand` affordance.
///
/// A command affordance is the same value the CLI prints as a shell line; the
/// window's idiom for it is the palette, pre-filled and one Return away.
pub(crate) fn affordance_palette_text(name: &str, args: &[String]) -> String {
    let mut text = format!("> {name}");
    for argument in args {
        text.push(' ');
        text.push_str(argument);
    }
    text
}

/// Lowers one certified-subscription failure into the shared fault shape.
///
/// The lease loop has its own error type because it admits frames the request
/// client never sees. Its failures still reach a reader as faults, and they
/// must be the same faults, so they are lowered here rather than rendered by a
/// second code path in the status bar.
pub(crate) fn from_subscription(
    error: &crate::transport::error::ClientError,
    endpoint: &str,
) -> Fault {
    use crate::transport::error::ClientError as Feed;
    let operand = Operand::Path(endpoint.to_owned());
    let (slug, cause, sentence, affordance) = match error {
        Feed::Io(detail) => (
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            format!("the live feed disconnected: {detail}"),
            Affordance::Retry,
        ),
        Feed::Protocol(detail) => (
            FaultSlug::Protocol,
            CauseSlug::Unproven,
            format!("the live feed sent a frame this build cannot admit: {detail}"),
            Affordance::Retry,
        ),
        Feed::Transport(detail) => (
            FaultSlug::Transport,
            CauseSlug::Oversized,
            format!("a live batch exceeded its declared credit: {detail}"),
            Affordance::Retry,
        ),
        Feed::IncoherentRoot => (
            FaultSlug::IncoherentView,
            CauseSlug::Unproven,
            "the replacement root arrived without producer-admitted complete coverage".to_owned(),
            Affordance::WaitForReadiness,
        ),
        Feed::BasisMismatch { .. } => (
            FaultSlug::WrongBasis,
            CauseSlug::Moved,
            "the service rebased onto another source stream; a full re-read is in flight"
                .to_owned(),
            Affordance::WaitForReadiness,
        ),
        Feed::CursorMismatch => (
            FaultSlug::CursorMismatch,
            CauseSlug::Moved,
            "the next batch did not follow the cursor held here; hydrating from a fresh snapshot"
                .to_owned(),
            Affordance::WaitForReadiness,
        ),
    };
    Fault::new(slug, operand, Cause::new(cause, sentence), affordance)
}

/// Returns the fault shown when a reply admitted but had the wrong shape.
pub(crate) fn wrong_shape(operand: Operand, expected: &str) -> Fault {
    Fault::new(
        FaultSlug::Protocol,
        operand,
        Cause::new(
            CauseSlug::Unproven,
            format!("the service answered with a reply this build cannot admit as {expected}"),
        ),
        Affordance::Retry,
    )
}

/// Returns the fault shown when a coordinate names no project on the shelf.
pub(crate) fn project_missing(coordinate: &str) -> Fault {
    Fault::new(
        FaultSlug::NotFound,
        Operand::Coordinate(backend_present::Coordinate::new(coordinate)),
        Cause::new(
            CauseSlug::Absent,
            "no row on the current revision answers to this project coordinate",
        ),
        Affordance::Reindex {
            path: coordinate.to_owned(),
        },
    )
}
