//! Closed workflow event vocabulary.
#![allow(
    missing_docs,
    reason = "the compact closed event vocabulary is documented at enum level"
)]

use crate::{StageKey, key::StageOutput};
use strum::FromRepr;

/// Opaque event-format version; unknown versions must be preserved and rejected.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct WorkflowVersion(u16);

impl WorkflowVersion {
    /// Only supported Wave 1 event format.
    pub const WAVE1: Self = Self(1);
}
impl From<u16> for WorkflowVersion {
    fn from(value: u16) -> Self {
        Self(value)
    }
}
impl core::ops::Deref for WorkflowVersion {
    type Target = u16;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Closed Wave 1 staging steps.
#[derive(Clone, Copy, Debug, Eq, FromRepr, Hash, PartialEq)]
#[repr(u8)]
pub enum StageId {
    /// Verify and make one promised object ready for publication.
    HydrateObject = 1,
}

impl From<StageId> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "StageId is a closed repr(u8) protocol discriminant"
    )]
    fn from(stage: StageId) -> Self {
        stage as Self
    }
}

/// Closed classified failure outcome.
#[derive(Clone, Copy, Debug, Eq, FromRepr, Hash, PartialEq)]
#[repr(u8)]
pub enum FailureCode {
    /// Bounded runtime rejected or failed the concrete stage.
    Runtime = 1,
    /// Verification rejected malformed or incomplete immutable data.
    Verification = 2,
}

impl From<FailureCode> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "FailureCode is a closed repr(u8) protocol discriminant"
    )]
    fn from(code: FailureCode) -> Self {
        code as Self
    }
}

/// Durable transition recorded for one compact stage key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    /// Request durably committed.
    Requested,
    /// Runtime admission durably committed.
    Admitted,
    /// Stage outcome durably committed.
    Staged { output: StageOutput },
    /// Stage output verification durably committed.
    Verified { output: StageOutput },
    /// Publication attempt durably committed before external publication begins.
    PublicationStarted { output: StageOutput },
    /// Immutable publication outcome durably committed.
    Published { output: StageOutput },
    /// Failure durably committed. A failure after publication begins becomes a publication-unknown
    /// state rather than a cancellation or a claim that publication did not occur.
    Failed { code: FailureCode },
    /// Pre-publication cancellation durably committed.
    Cancelled,
}

impl EventKind {
    pub(crate) const fn name(self) -> EventName {
        match self {
            Self::Requested => EventName::Requested,
            Self::Admitted => EventName::Admitted,
            Self::Staged { .. } => EventName::Staged,
            Self::Verified { .. } => EventName::Verified,
            Self::PublicationStarted { .. } => EventName::PublicationStarted,
            Self::Published { .. } => EventName::Published,
            Self::Failed { .. } => EventName::Failed,
            Self::Cancelled => EventName::Cancelled,
        }
    }
}

/// Closed event name used in transition diagnostics.
#[derive(Clone, Copy, Debug, Eq, FromRepr, PartialEq)]
#[repr(u8)]
pub enum EventName {
    Requested = 0,
    Admitted = 1,
    Staged = 2,
    Verified = 3,
    PublicationStarted = 4,
    Published = 5,
    Failed = 6,
    Cancelled = 7,
}

impl From<EventName> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "EventName is a closed repr(u8) protocol discriminant"
    )]
    fn from(name: EventName) -> Self {
        name as Self
    }
}

/// Compact durable workflow event. It repeats only a 32-byte [`StageKey`], never root/content config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowEvent {
    /// Explicit format version.
    pub version: WorkflowVersion,
    /// Plane-independent derived idempotency key.
    pub key: StageKey,
    /// Recorded transition.
    pub kind: EventKind,
}
