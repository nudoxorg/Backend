//! Tests for every layer that does not need a window, plus entity tests.
//! Pure state and projections live in `state`; GPUI entity tests in `gpui`.
//! The transport and reducer proofs are unchanged from before this rewrite.

mod state;
mod transport;
