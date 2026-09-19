//! Defines json wire envelope behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire envelope invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::interface::{ApplicationDisposition, ApplicationOutcome, ApplicationReply, ReplyBody};
use serde::Serialize;

use super::application::{DiagnosticWire, ReplyBodyWire, TerminalWire};
use crate::protocol::{AdapterError, AdapterErrorCode, AdapterField};

#[derive(Serialize)]
pub(crate) struct ApplicationReplyWire {
    correlation: u64,
    body: ReplyBodyWire,
    terminal: TerminalWire,
    diagnostic: Option<DiagnosticWire>,
}

#[derive(Serialize)]
pub(crate) struct AdapterErrorEnvelope {
    adapter_error: AdapterErrorWire,
}

#[derive(Serialize)]
struct AdapterErrorWire {
    code: AdapterErrorCodeWire,
    field: AdapterField,
    actual: Option<usize>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum AdapterErrorCodeWire {
    UnknownAction,
    MissingField,
    #[serde(rename = "invalid_number_or_json_shape")]
    InvalidNumber,
    #[serde(rename = "invalid_json_shape")]
    InvalidShape,
    FieldTooLong,
    TooManyFields,
    InvalidPackageUrl,
    PackageProfileMismatch,
}

impl From<ApplicationReply> for ApplicationReplyWire {
    fn from(reply: ApplicationReply) -> Self {
        let correlation = reply.correlation.0;
        match reply.outcome {
            ApplicationOutcome::Resolved(body) => Self::resolved(correlation, &body),
            ApplicationOutcome::Failed { diagnostic } => Self {
                correlation,
                body: ReplyBodyWire::Rejected,
                terminal: TerminalWire::Failed,
                diagnostic: Some(diagnostic.into()),
            },
        }
    }
}

impl ApplicationReplyWire {
    fn resolved(correlation: u64, body: &ReplyBody) -> Self {
        Self {
            correlation,
            body: body.clone().into(),
            terminal: ApplicationDisposition::from(body).into(),
            diagnostic: None,
        }
    }
}

impl From<ApplicationDisposition> for TerminalWire {
    fn from(disposition: ApplicationDisposition) -> Self {
        match disposition {
            ApplicationDisposition::Accepted { operation } => Self::Accepted {
                operation: operation.0,
            },
            ApplicationDisposition::Complete { emitted } => Self::Complete { emitted },
            ApplicationDisposition::Partial {
                emitted,
                unavailable,
            } => Self::Partial {
                emitted,
                unavailable: unavailable.into(),
            },
            ApplicationDisposition::Degraded {
                emitted,
                unavailable,
            } => Self::Degraded {
                emitted,
                unavailable: unavailable.into(),
            },
            ApplicationDisposition::Cancelled { emitted } => Self::Cancelled { emitted },
        }
    }
}

impl From<&AdapterError> for AdapterErrorEnvelope {
    fn from(error: &AdapterError) -> Self {
        Self {
            adapter_error: error.into(),
        }
    }
}

impl From<&AdapterError> for AdapterErrorWire {
    fn from(error: &AdapterError) -> Self {
        Self {
            code: error.code.into(),
            field: error.field,
            actual: error.actual,
        }
    }
}

impl From<AdapterErrorCode> for AdapterErrorCodeWire {
    fn from(code: AdapterErrorCode) -> Self {
        match code {
            AdapterErrorCode::UnknownAction => Self::UnknownAction,
            AdapterErrorCode::MissingField => Self::MissingField,
            AdapterErrorCode::InvalidNumber => Self::InvalidNumber,
            AdapterErrorCode::InvalidShape => Self::InvalidShape,
            AdapterErrorCode::FieldTooLong => Self::FieldTooLong,
            AdapterErrorCode::TooManyFields => Self::TooManyFields,
            AdapterErrorCode::InvalidPackageUrl => Self::InvalidPackageUrl,
            AdapterErrorCode::PackageProfileMismatch => Self::PackageProfileMismatch,
        }
    }
}
