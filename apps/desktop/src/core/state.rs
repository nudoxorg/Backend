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
            message.to_string()
        };
        Self {
            code,
            message: Arc::from(message),
        }
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resource<T> {
    value: Option<(Arc<T>, VersionedRoot)>,
    terminal: ResourceTerminal,
    activity: Activity,
}

/// Terminal coverage/failure status for a resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceTerminal {
    /// The value is complete.
    Complete,
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
        }
    }

    /// Creates a current loaded resource at a producer root.
    #[must_use]
    pub fn loaded_at(value: T, root: VersionedRoot) -> Self {
        Self {
            value: Some((Arc::new(value), root)),
            terminal: ResourceTerminal::Complete,
            activity: Activity::Rest,
        }
    }

    /// Marks a request as waiting while retaining a last-good value.
    #[must_use]
    pub fn waiting(self) -> Self {
        Self {
            activity: Activity::Waiting,
            ..self
        }
    }

    /// Marks a request as working while retaining a last-good value.
    #[must_use]
    pub fn working(self) -> Self {
        Self {
            activity: Activity::Working,
            ..self
        }
    }

    /// Marks work stopped while retaining a last-good value.
    #[must_use]
    pub fn stopped(self) -> Self {
        Self {
            activity: Activity::Stopped,
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
        }
    }

    /// Marks this resource unavailable while retaining any last-good value.
    #[must_use]
    pub fn mark_unavailable(mut self, reason: UnavailableReason) -> Self {
        self.terminal = ResourceTerminal::Unavailable(reason);
        self.activity = Activity::Stopped;
        self
    }

    /// Marks a redacted, typed fault without discarding a retained value.
    #[must_use]
    pub fn error(code: FaultCode, message: impl Into<Arc<str>>) -> Self {
        Self {
            value: None,
            terminal: ResourceTerminal::Fault(ErrorValue::new(code, message)),
            activity: Activity::Stopped,
        }
    }

    /// Marks this resource failed while retaining any last-good value.
    #[must_use]
    pub fn mark_error(mut self, code: FaultCode, message: impl Into<Arc<str>>) -> Self {
        self.terminal = ResourceTerminal::Fault(ErrorValue::new(code, message));
        self.activity = Activity::Stopped;
        self
    }

    /// Returns the retained loaded value, including while stale/working.
    #[must_use]
    pub fn loaded_value(&self) -> Option<&T> {
        self.value.as_ref().map(|(value, _)| value.as_ref())
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
        self.loaded_value().is_some() && matches!(self.terminal, ResourceTerminal::Complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> VersionedRoot {
        VersionedRoot::new(
            backend_library::view_state_root(&[("state".to_owned(), "one".to_owned())]),
            1,
        )
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
}
