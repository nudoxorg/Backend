//! Bounded, version-aware replication transport.
//!
//! Replication owns wire claims, negotiation, immutable object transfer, and
//! generic envelopes. Execution retains ownership of admitted recipe, read,
//! authority, work-key, output-equivalence, receipt, and fence semantics.
#![deny(unsafe_code)]
// Test fixtures intentionally use compact `expect` assertions to keep each
// protocol invariant local. Production code remains subject to the workspace
// panic/expect lints above; this test-only allowance keeps the all-target
// quality gate focused on implementation diagnostics.
#![cfg_attr(test, allow(clippy::expect_used, clippy::too_many_lines))]

mod authority;
mod codec;
mod coverage;
mod execution;
mod identities;
mod local_peer;
mod negotiation;
mod reconcile;
mod transfer;
mod transport;
mod unix_endpoint;

pub use authority::*;
pub use codec::{decode_message, encode_message};
pub use coverage::*;
pub use execution::*;
pub use identities::*;
pub use local_peer::*;
pub use negotiation::*;
pub use reconcile::*;
pub use transfer::*;
pub use transport::*;
pub use unix_endpoint::*;

#[cfg(test)]
mod tests;
