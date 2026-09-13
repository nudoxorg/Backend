//! Defines probe behavior for `heart-observe`, whose purpose is to define allocation-free observation events and probe capabilities.
//! This module owns the probe invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The generic lazy probe seam.

/// Receives a caller-owned event without requiring allocation or dynamic dispatch.
///
/// The event builder is deliberately lazy: implementations that discard observations must not
/// invoke it. That keeps disabled instrumentation behaviorally inert and allocation-free.
pub trait Probe<Event> {
    /// Records one event if this probe elects to retain it.
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> Event;
}

impl<Event> Probe<Event> for () {
    fn record_with<Build>(&mut self, _build: Build)
    where
        Build: FnOnce() -> Event,
    {
    }
}
