//! Defines json wire adaptive behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire adaptive invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use backend_execution::adaptive::{
    BudgetAmount, CapabilityDomain, DuplicateInput, FactKey, ObjectDomain, Overload,
    OverloadSubject, Pin, PolicyError, RecoveryCause,
};
use serde::Serialize;

use super::scalar::{CapabilityKind, ContentText, InputClass, ResourceClass, StorageTier};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum AdaptiveDisposition {
    NoAction,
    RetryRemote {
        pin: PinWire,
        cause: RecoveryCauseWire,
        retries_remaining: u8,
    },
    RecoveryExhausted {
        pin: PinWire,
        cause: RecoveryCauseWire,
    },
    Overloaded {
        overload: OverloadWire,
    },
}

#[derive(Serialize)]
pub(super) struct PinWire {
    generation: ContentText<backend_version::RootDomain>,
    snapshot: ContentText<backend_version::IndexSnapshotDomain>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum RecoveryCauseWire {
    Outage,
    Inconsistent { observed: PinWire },
}

#[derive(Serialize)]
pub(super) struct OverloadWire {
    subject: OverloadSubjectWire,
    resource: ResourceClass,
    needed: BudgetAmountWire,
    available: BudgetAmountWire,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OverloadSubjectWire {
    Fact {
        key: FactKeyWire,
    },
    Bundle {
        capability: CapabilityKind,
        bundle: ContentText<CapabilityDomain>,
    },
    Remote {
        pin: PinWire,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BudgetAmountWire {
    Bytes { value: u32 },
    Operations { value: u8 },
    Retries { value: u8 },
}

#[derive(Serialize)]
pub(super) struct FactKeyWire {
    pin: PinWire,
    object: ContentText<ObjectDomain>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum PolicyErrorWire {
    TooManyFacts {
        class: InputClass,
        limit: usize,
        observed: usize,
    },
    PinMismatch {
        class: InputClass,
        expected: PinWire,
        observed: FactKeyWire,
    },
    Duplicate {
        value: DuplicateInputWire,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum DuplicateInputWire {
    Local {
        key: FactKeyWire,
        tier: StorageTier,
    },
    Remote {
        key: FactKeyWire,
    },
    Demand {
        key: FactKeyWire,
    },
    Bundle {
        capability: CapabilityKind,
        bundle: ContentText<CapabilityDomain>,
    },
}

impl From<interface_core::AdaptiveDisposition> for AdaptiveDisposition {
    fn from(disposition: interface_core::AdaptiveDisposition) -> Self {
        match disposition {
            interface_core::AdaptiveDisposition::NoAction => Self::NoAction,
            interface_core::AdaptiveDisposition::RetryRemote {
                pin,
                cause,
                retries_remaining,
            } => Self::RetryRemote {
                pin: pin.into(),
                cause: cause.into(),
                retries_remaining: retries_remaining.into(),
            },
            interface_core::AdaptiveDisposition::RecoveryExhausted { pin, cause } => {
                Self::RecoveryExhausted {
                    pin: pin.into(),
                    cause: cause.into(),
                }
            }
            interface_core::AdaptiveDisposition::Overloaded(overload) => Self::Overloaded {
                overload: overload.into(),
            },
        }
    }
}

impl From<Pin> for PinWire {
    fn from(pin: Pin) -> Self {
        Self {
            generation: ContentText(pin.generation),
            snapshot: ContentText(pin.snapshot),
        }
    }
}

impl From<RecoveryCause> for RecoveryCauseWire {
    fn from(cause: RecoveryCause) -> Self {
        match cause {
            RecoveryCause::Outage => Self::Outage,
            RecoveryCause::Inconsistent { observed } => Self::Inconsistent {
                observed: observed.into(),
            },
        }
    }
}

impl From<Overload> for OverloadWire {
    fn from(overload: Overload) -> Self {
        Self {
            subject: overload.subject.into(),
            resource: overload.resource.into(),
            needed: overload.needed.into(),
            available: overload.available.into(),
        }
    }
}

impl From<OverloadSubject> for OverloadSubjectWire {
    fn from(subject: OverloadSubject) -> Self {
        match subject {
            OverloadSubject::Fact(key) => Self::Fact { key: key.into() },
            OverloadSubject::Bundle { capability, bundle } => Self::Bundle {
                capability: capability.into(),
                bundle: ContentText(bundle),
            },
            OverloadSubject::Remote(pin) => Self::Remote { pin: pin.into() },
        }
    }
}

impl From<BudgetAmount> for BudgetAmountWire {
    fn from(amount: BudgetAmount) -> Self {
        match amount {
            BudgetAmount::Bytes(bytes) => Self::Bytes {
                value: bytes.into(),
            },
            BudgetAmount::Operations(operations) => Self::Operations {
                value: operations.into(),
            },
            BudgetAmount::Retries(retries) => Self::Retries {
                value: retries.into(),
            },
        }
    }
}

impl From<FactKey> for FactKeyWire {
    fn from(key: FactKey) -> Self {
        Self {
            pin: key.pin.into(),
            object: ContentText(key.object),
        }
    }
}

impl From<PolicyError> for PolicyErrorWire {
    fn from(error: PolicyError) -> Self {
        match error {
            PolicyError::TooManyFacts {
                class,
                limit,
                observed,
            } => Self::TooManyFacts {
                class: class.into(),
                limit,
                observed,
            },
            PolicyError::PinMismatch {
                class,
                expected,
                observed,
            } => Self::PinMismatch {
                class: class.into(),
                expected: expected.into(),
                observed: observed.into(),
            },
            PolicyError::Duplicate(value) => Self::Duplicate {
                value: value.into(),
            },
        }
    }
}

impl From<DuplicateInput> for DuplicateInputWire {
    fn from(duplicate: DuplicateInput) -> Self {
        match duplicate {
            DuplicateInput::Local { key, tier } => Self::Local {
                key: key.into(),
                tier: tier.into(),
            },
            DuplicateInput::Remote { key } => Self::Remote { key: key.into() },
            DuplicateInput::Demand { key } => Self::Demand { key: key.into() },
            DuplicateInput::Bundle { capability, bundle } => Self::Bundle {
                capability: capability.into(),
                bundle: ContentText(bundle),
            },
        }
    }
}
