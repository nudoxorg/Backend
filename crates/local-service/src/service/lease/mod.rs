//! Durable subscription leases retained by the owner.
//!
//! A lease lets a holder detach from the owner and later resume exactly where
//! it was, and lets the owner keep a reset root alive while the holder pages
//! through it. Both are retention, and retention that nothing reclaims is a
//! leak: a holder that vanishes mid-reset would otherwise pin its root for the
//! life of the process. This module therefore owns three things together.
//!
//! * [`LeaseTable`]: what is retained, and the single comparison the idle owner
//!   poll makes to learn that nothing is due.
//! * `state`: the immutable lease values and the only transitions that may move
//!   a deadline.
//! * [`LeaseHost`]: the Open/Resume/Credit/Ack/Renew/Cancel/Page protocol over
//!   a [`LeaseSource`], so every rule is testable against a scripted daemon and
//!   an injected clock.
//!
//! The numbers the holder relies on are not defined here; they come from
//! `backend_client::lease_contract`, the one place both ends import them from.

mod host;
mod identity;
mod limits;
mod owner;
#[cfg(test)]
mod owner_tests;
mod state;
mod table;
#[cfg(test)]
mod tests;

pub(super) use host::LeaseHost;
pub use limits::{LeaseLimitError, SubscriptionLeaseLimits};
pub(super) use owner::OwnerSource;
pub(super) use table::LeaseTable;
