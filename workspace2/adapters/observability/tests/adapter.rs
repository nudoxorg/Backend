//! Public adapter journey, exact signal contract, and bounded exporter proofs.

#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "adapter tests retain exact scenario and SDK failures without heap-erasing sources"
)]

#[path = "adapter/overload.rs"]
mod overload;
#[path = "adapter/signal_contract.rs"]
mod signal_contract;
#[path = "adapter/support.rs"]
mod support;
