//! Sealed availability values crossing the engine/UI boundary.
//!
//! A widget can render a [`Resource`] without guessing whether an absent value
//! is still waiting or permanently unavailable. The state algebra is closed so
//! another module cannot invent a fourth terminal state and bypass reducer
//! error handling.

use crate::core::ids::VersionedRoot;
use std::sync::Arc;

/// Why an otherwise valid identity is unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnavailableReason {
    /// The service does not implement the lane.
    Unsupported,
    /// The identity is outside the selected scope.
    OutOfScope,
}

/// A typed load failure suitable for diagnostics and accessibility output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorValue {
    code: FaultCode,
    message: Arc<str>,
    diagnostic: Option<DiagnosticDetail>,
}

/// Full bounded diagnostic retained independently of the short painted summary.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosticDetail {
    text: Arc<str>,
    truncated: bool,
}

const MAX_ERROR_SUMMARY_BYTES: usize = 512;
const SHORTENED: &str = " … [shortened] … ";

fn sanitize_diagnostic(value: &str) -> String {
    value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// Keeps the operation at the start and the terminal cause at the end. This is
/// a text projection only; no diagnostic words determine authority or Retry.
fn bounded_diagnostic_ends(value: &str, maximum: usize) -> (String, bool) {
    if value.len() <= maximum { return (value.to_owned(), false); }
    let available = maximum - SHORTENED.len();
    let mut head = available / 2;
    while !value.is_char_boundary(head) { head -= 1; }
    let mut tail = value.len() - (available - head);
    while !value.is_char_boundary(tail) { tail += 1; }
    (format!("{}{SHORTENED}{}", &value[..head], &value[tail..]), true)
}

/// Closed fault code vocabulary. Messages remain bounded/redacted at the
/// runtime mapping edge before they enter a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultCode {
    /// Service could not be reached.
    Transport,
    /// A response failed schema/admission checks.
    Protocol,
    /// The requested object was not found.
    Missing,
    /// Persistence failed.
    Persistence,
    /// The selected native identity cannot cross an available boundary.
    Unsupported,
    /// The operation was cancelled or superseded.
    Cancelled,
}

impl ErrorValue {
    /// Creates an error value from a bounded message.
    #[must_use]
    pub fn new(code: FaultCode, message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        let message: String = message
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let message = if message.len() > 512 {
            let mut end = 509;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            let mut bounded = message[..end].to_owned();
            bounded.push('…');
            bounded
        } else {
            message.clone()
        };
        Self {
            code,
            message: Arc::from(message),
            diagnostic: None,
        }
    }

    /// Projects a producer diagnostic without dropping its terminal cause.
    /// The full detail has the existing product-text bound; the painted summary
    /// keeps the existing 512-byte bound. Both remove control characters.
    #[must_use]
    pub fn with_diagnostic(code: FaultCode, operation: &str, detail: &str) -> Self {
        let clean = sanitize_diagnostic(detail);
        let (detail, truncated) = bounded_diagnostic_ends(
            &clean, backend_library::MAX_PRODUCT_TEXT_BYTES,
        );
        let full_summary = format!("{}{detail}", sanitize_diagnostic(operation));
        let (message, _) = bounded_diagnostic_ends(&full_summary, MAX_ERROR_SUMMARY_BYTES);
        Self {
            code,
            message: Arc::from(message),
            diagnostic: Some(DiagnosticDetail { text: Arc::from(detail), truncated }),
        }
    }

    /// Full bounded producer diagnostic for inspection or explicit copying.
    #[must_use]
    pub fn diagnostic_detail(&self) -> Option<&str> {
        self.diagnostic.as_ref().map(|detail| detail.text.as_ref())
    }

    /// Whether even the retained diagnostic exceeded the product-text bound.
    #[must_use]
    pub fn diagnostic_was_truncated(&self) -> bool {
        self.diagnostic.as_ref().is_some_and(|detail| detail.truncated)
    }

    /// Returns the stable diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the closed fault code.
    #[must_use]
    pub const fn code(&self) -> FaultCode {
        self.code
    }
}

/// Work activity orthogonal to terminal coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Activity {
    /// No work is currently running and the value is current.
    Rest,
    /// A request is actively computing.
    Working,
    /// Waiting for an owner, lease, or remote producer.
    Waiting,
    /// Work stopped; a last good value may remain.
    Stopped,
    /// No request has been made yet.
    NotYet,
}

/// Resource with orthogonal activity and retained last-good value.
#[derive(Debug, Eq, PartialEq)]
pub struct Resource<T> {
    value: Option<(Arc<T>, VersionedRoot)>,
    terminal: ResourceTerminal,
    activity: Activity,
    preparation: Option<QueryPreparation>,
}

/// A query refused while its exact immutable basis is being prepared. This
/// is awaiting work, not terminal failure or proof that an index is absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryPreparation {
    /// The exact immutable request basis admitted by the wire reader.
    pub basis: backend_library::ViewRevision,
    /// Whether the current worker is preparing or a predecessor is retiring.
    pub state: backend_library::QueryPreparationState,
}

impl QueryPreparation {
    /// Visible awaiting copy shared by the active query readers.
    pub const fn words(self) -> &'static str {
        match self.state {
            backend_library::QueryPreparationState::Preparing => "Preparing query results…",
            backend_library::QueryPreparationState::Retiring => "Waiting for the previous query preparation to finish…",
        }
    }
}

// Cloning shares the value; it never requires `T: Clone`.
impl<T> Clone for Resource<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            terminal: self.terminal.clone(),
            activity: self.activity,
            preparation: self.preparation,
        }
    }
}

/// Terminal coverage/failure status for a resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceTerminal {
    /// The value is complete.
    Complete,
    /// An intermediate value is readable, but its owner has not finished.
    Partial,
    /// The producer does not provide the value.
    Unavailable(UnavailableReason),
    /// The latest attempt failed.
    Fault(ErrorValue),
}

impl<T> Resource<T> {
    /// Creates a resource with no request yet issued.
    #[must_use]
    pub const fn not_yet() -> Self {
        Self {
            value: None,
            terminal: ResourceTerminal::Complete,
            activity: Activity::NotYet,
            preparation: None,
        }
    }

    /// Creates a current loaded resource at a producer root.
    #[must_use]
    pub fn loaded_at(value: T, root: VersionedRoot) -> Self {
        Self {
            value: Some((Arc::new(value), root)),
            terminal: ResourceTerminal::Complete,
            activity: Activity::Rest,
            preparation: None,
        }
    }

    /// Creates an intermediate value at the producer root. The value can be
    /// read immediately, but it cannot be treated as a complete saved page.
    #[must_use]
    pub fn partial_at(value: T, root: VersionedRoot) -> Self {
        Self {
            value: Some((Arc::new(value), root)),
            terminal: ResourceTerminal::Partial,
            activity: Activity::Working,
            preparation: None,
        }
    }

    /// Creates a current loaded resource around an already shared value.
    #[must_use]
    pub fn loaded_arc_at(value: Arc<T>, root: VersionedRoot) -> Self {
        Self {
            value: Some((value, root)),
            terminal: ResourceTerminal::Complete,
            activity: Activity::Rest,
            preparation: None,
        }
    }

    /// Marks a request as waiting while retaining a last-good value.
    #[must_use]
    pub fn waiting(self) -> Self {
        Self {
            activity: Activity::Waiting,
            preparation: None,
            ..self
        }
    }

    /// Marks a request as working while retaining a last-good value.
    #[must_use]
    pub fn working(self) -> Self {
        Self {
            activity: Activity::Working,
            preparation: None,
            ..self
        }
    }

    /// Marks the resource at rest after work ended without a new result,
    /// retaining any last-good value and terminal. A resource that never held
    /// a value and never failed returns to [`Activity::NotYet`], so the next
    /// request is not mistaken for a retry.
    #[must_use]
    pub fn resting(self) -> Self {
        let activity =
            if self.value.is_none() && matches!(self.terminal, ResourceTerminal::Complete) {
                Activity::NotYet
            } else {
                Activity::Rest
            };
        Self { activity, preparation: None, ..self }
    }

    /// Marks work stopped while retaining a last-good value.
    #[must_use]
    pub fn stopped(self) -> Self {
        Self {
            activity: Activity::Stopped,
            preparation: None,
            ..self
        }
    }

    /// Marks the resource unavailable without discarding a retained value.
    #[must_use]
    pub const fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            value: None,
            terminal: ResourceTerminal::Unavailable(reason),
            activity: Activity::Rest,
            preparation: None,
        }
    }

    /// Marks this resource unavailable while retaining any last-good value.
    #[must_use]
    pub fn mark_unavailable(mut self, reason: UnavailableReason) -> Self {
        self.terminal = ResourceTerminal::Unavailable(reason);
        self.activity = Activity::Stopped;
        self.preparation = None;
        self
    }

    /// Marks a redacted, typed fault without discarding a retained value.
    #[must_use]
    pub fn error(code: FaultCode, message: impl Into<Arc<str>>) -> Self {
        Self {
            value: None,
            terminal: ResourceTerminal::Fault(ErrorValue::new(code, message)),
            activity: Activity::Stopped,
            preparation: None,
        }
    }

    /// Marks this resource failed while retaining any last-good value.
    #[must_use]
    pub fn mark_error(self, code: FaultCode, message: impl Into<Arc<str>>) -> Self {
        self.mark_fault(ErrorValue::new(code, message))
    }

    /// Retains the complete typed fault and any last-good value at its original
    /// root. Failure stops work; it never confirms the retained value.
    #[must_use]
    pub fn mark_fault(mut self, error: ErrorValue) -> Self {
        self.terminal = ResourceTerminal::Fault(error);
        self.activity = Activity::Stopped;
        self.preparation = None;
        self
    }

    /// Retain the admitted presentation and its original root while awaiting
    /// preparation. No retained bytes acquire current action authority.
    #[must_use]
    pub fn awaiting_query(mut self, preparation: QueryPreparation) -> Self {
        if matches!(self.terminal, ResourceTerminal::Fault(_) | ResourceTerminal::Unavailable(_)) {
            self.terminal = ResourceTerminal::Complete;
        }
        self.activity = Activity::Waiting;
        self.preparation = Some(preparation);
        self
    }

    #[must_use]
    pub const fn query_preparation(&self) -> Option<QueryPreparation> {
        self.preparation
    }

    /// Returns the retained loaded value, including while stale/working.
    #[must_use]
    pub fn loaded_value(&self) -> Option<&T> {
        self.value.as_ref().map(|(value, _)| value.as_ref())
    }

    /// Returns the retained value's shared allocation (to keep it, or to
    /// save it without copying).
    #[must_use]
    pub fn loaded_arc(&self) -> Option<&Arc<T>> {
        self.value.as_ref().map(|(value, _)| value)
    }

    /// The same value, now known to be current at `root`: nothing a view
    /// draws changes (W-Open I2, a launch snapshot the owner confirmed).
    #[must_use]
    pub fn rebased(mut self, root: VersionedRoot) -> Self {
        if let Some((_, at)) = &mut self.value {
            *at = root;
        }
        self
    }

    /// Returns the producer root of the retained value.
    #[must_use]
    pub fn value_root(&self) -> Option<VersionedRoot> {
        self.value.as_ref().map(|(_, root)| *root)
    }

    /// Returns the orthogonal activity.
    #[must_use]
    pub const fn activity(&self) -> Activity {
        self.activity
    }

    /// Returns terminal coverage/failure.
    #[must_use]
    pub const fn terminal(&self) -> &ResourceTerminal {
        &self.terminal
    }
}

impl<T> Resource<T> {
    /// Returns whether a current value is available.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.preparation.is_none()
            && self.loaded_value().is_some()
            && matches!(self.terminal, ResourceTerminal::Complete)
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn root() -> VersionedRoot {
        VersionedRoot::synthetic(
            backend_library::view_state_root(&[("state".to_owned(), "one".to_owned())]),
            1,
        )
    }

    #[test]
    fn preparing_query_retains_presentation_without_terminal_failure_or_current_authority() {
        let before = Resource::loaded_at(7_u32, root());
        let Some(allocation) = before.loaded_arc().cloned() else {
            panic!("the last-good allocation must exist");
        };
        for state in [backend_library::QueryPreparationState::Preparing, backend_library::QueryPreparationState::Retiring] {
            let preparation = QueryPreparation { basis: root().root().into(), state };
            let awaiting = before.clone().mark_error(FaultCode::Transport, "previous failure")
                .working().awaiting_query(preparation);
            assert_eq!(awaiting.query_preparation(), Some(preparation));
            assert_eq!(awaiting.activity(), Activity::Waiting);
            assert_eq!(awaiting.terminal(), &ResourceTerminal::Complete);
            assert_eq!(awaiting.value_root(), Some(root()));
            assert!(awaiting.loaded_arc().is_some_and(|retained| Arc::ptr_eq(retained, &allocation)));
            assert!(!awaiting.is_loaded(), "awaiting query bytes cannot be captured as a completed reading");
            assert!(matches!(crate::core::admit_resource(&awaiting, root(), true),
                crate::core::ResourceAdmission::Retained { reason: crate::core::ReadHoldReason::Reading, .. }));
            assert!(awaiting.clone().waiting().query_preparation().is_none());
            assert!(awaiting.mark_fault(ErrorValue::new(FaultCode::Protocol, "real failure")).query_preparation().is_none());
        }
    }

    #[test]
    fn activity_and_terminal_coverage_are_orthogonal() {
        let waiting = Resource::<u32>::not_yet().waiting();
        assert_eq!(waiting.activity(), Activity::Waiting);
        assert!(matches!(waiting.terminal(), ResourceTerminal::Complete));
        assert!(waiting.loaded_value().is_none());

        let stale = Resource::loaded_at(7_u32, root())
            .working()
            .mark_unavailable(UnavailableReason::Unsupported);
        assert_eq!(stale.loaded_value(), Some(&7));
        assert_eq!(stale.activity(), Activity::Stopped);
        assert!(matches!(
            stale.terminal(),
            ResourceTerminal::Unavailable(UnavailableReason::Unsupported)
        ));
    }

    #[test]
    fn faults_are_typed_and_bounded_without_dropping_last_good_value() {
        let message = "x".repeat(2_000);
        let resource = Resource::loaded_at(7_u32, root()).mark_error(FaultCode::Transport, message);
        assert_eq!(resource.loaded_value(), Some(&7));
        let ResourceTerminal::Fault(error) = resource.terminal() else {
            panic!("expected fault");
        };
        assert_eq!(error.code(), FaultCode::Transport);
        assert!(error.message().len() <= 512);
    }

    #[test]
    fn a_typed_fault_keeps_its_diagnostic_and_predecessor_root() {
        let diagnostic = format!(
            "open compiler owner: {}caused by: journal root cause 日本語\0",
            "intermediate 原因\t".repeat(90)
        );
        let fault = ErrorValue::with_diagnostic(
            FaultCode::Persistence,
            "The index could not start. ",
            &diagnostic,
        );
        assert!(
            fault
                .diagnostic_detail()
                .is_some_and(|detail| detail.len() > 512)
        );
        let previous = Resource::loaded_at(7_u32, root());
        let Some(allocation) = previous.loaded_arc().cloned() else {
            panic!("last-good allocation")
        };
        let failed = previous.working().mark_fault(fault.clone());
        assert_eq!(failed.terminal(), &ResourceTerminal::Fault(fault));
        assert_eq!(failed.activity(), Activity::Stopped);
        assert!(
            !failed.is_loaded(),
            "retained bytes are not current fault-free coverage"
        );
        assert_eq!(failed.value_root(), Some(root()));
        assert!(
            failed
                .loaded_arc()
                .is_some_and(|value| Arc::ptr_eq(value, &allocation))
        );
    }
}
