//! Exercises the `heart-telemetry` tests adapter contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Public adapter journey, exact signal contract, and bounded exporter proofs.

#[path = "adapter/overload.rs"]
mod overload;
#[path = "adapter/signal_contract.rs"]
mod signal_contract;
#[path = "adapter/signal_fixture.rs"]
mod signal_fixture;
#[path = "adapter/support.rs"]
mod support;
