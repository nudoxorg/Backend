//! Defines json wire compiler native io behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler native io invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_vocabulary::NativeWorkPhase;
use interface_core::{NativeIoFact, NativeIoPhase};
use serde::{Serialize, Serializer};

use super::super::super::scalar::serialize_error_kind;

/// Remote serde definition for a portable native I/O fact.
#[derive(Serialize)]
#[serde(remote = "interface_core::NativeIoFact")]
pub(crate) struct NativeIoFactWire {
    #[serde(serialize_with = "serialize_error_kind")]
    kind: std::io::ErrorKind,
    raw_os_code: Option<i32>,
}

/// Remote serde definition for the closed native I/O phase vocabulary.
#[derive(Serialize)]
#[serde(remote = "interface_core::NativeIoPhase", rename_all = "snake_case")]
pub(crate) enum NativeIoPhaseWire {
    Start,
    Input,
    Terminate,
    Wait,
    DiagnosticRead,
}

pub(crate) struct NativeIoFactRef<'value>(pub(crate) &'value NativeIoFact);

impl Serialize for NativeIoFactRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeIoFactWire::serialize(self.0, serializer)
    }
}

pub(crate) struct NativeIoFactOptionRef<'value>(pub(crate) &'value Option<NativeIoFact>);

impl Serialize for NativeIoFactOptionRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        match self.0.as_ref() {
            Some(fact) => NativeIoFactWire::serialize(fact, serializer),
            None => serializer.serialize_none(),
        }
    }
}

pub(crate) struct NativeIoPhaseRef<'value>(pub(crate) &'value NativeIoPhase);

impl Serialize for NativeIoPhaseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        NativeIoPhaseWire::serialize(self.0, serializer)
    }
}

const _: fn(&NativeIoFact) = |_| {};
const _: fn(&NativeIoPhase) = |_| {};
const _: fn(&NativeWorkPhase) = |_| {};
