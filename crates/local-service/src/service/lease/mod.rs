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
//!
//! # When reclamation runs
//!
//! The owner's idle poll (`OwnerService::serve_one`, driven every poll interval
//! by the listener with or without a client) and the start of every lease
//! request both call [`LeaseTable::reclaim_due`], which is one comparison until
//! something is due. Reclamation is therefore prompt while the owner loop is
//! free, but it cannot run while the loop is inside a long command or a
//! daemon round trip. Expiry never depends on it: every operation re-checks the
//! lease it touches, once to decide and once to commit, so a lease that
//! lapsed during blocking work is refused and released rather than served.

mod host;
mod identity;
mod limits;
#[cfg(test)]
mod model_tests;
mod owner;
#[cfg(test)]
mod owner_tests;
#[cfg(test)]
mod publication_tests;
mod state;
mod table;
#[cfg(test)]
mod tests;

pub(super) use host::LeaseHost;
pub use limits::{LeaseLimitError, SubscriptionLeaseLimits};
pub(super) use owner::OwnerSource;
pub(super) use table::LeaseTable;
