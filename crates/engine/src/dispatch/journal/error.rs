//! Error and stable reason codes for dispatch replay.

use super::record::{DispatchLog, DispatchRecordError};
use crate::fault::InjectedCrash;
use crate::journal::{JournalError, JournalReceipt};
use std::fmt;

/// Stable reason used when a lease expired during restart.
pub const RESTART_LEASE_EXPIRED: u16 = 1;
/// Stable reason used when an owner authority epoch no longer permits reuse.
pub const RESTART_AUTHORITY_REVOKED: u16 = 2;
/// Stable reason used when an attempt was already fenced by a prior restart.
pub const RESTART_ALREADY_FENCED: u16 = 3;
/// Stable reason used when a newer owner epoch has taken over the attempt.
pub const RESTART_OWNER_TAKEOVER: u16 = 4;

/// Error returned by dispatch journal open, append, or replay.
#[derive(Debug)]
pub enum DispatchJournalError {
    /// Generic hash-chain journal failure.
    Journal(JournalError),
    /// Record grammar or state-machine failure.
    Record(DispatchRecordError),
    /// A configured fault boundary requested an abort.
    Fault(InjectedCrash),
}

impl fmt::Display for DispatchJournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journal(error) => write!(f, "dispatch journal error: {error}"),
            Self::Record(error) => write!(f, "dispatch journal record error: {error}"),
            Self::Fault(error) => write!(f, "dispatch journal fault: {error}"),
        }
    }
}

impl std::error::Error for DispatchJournalError {}

impl From<JournalError> for DispatchJournalError {
    fn from(error: JournalError) -> Self {
        Self::Journal(error)
    }
}

impl From<DispatchRecordError> for DispatchJournalError {
    fn from(error: DispatchRecordError) -> Self {
        Self::Record(error)
    }
}

impl From<InjectedCrash> for DispatchJournalError {
    fn from(error: InjectedCrash) -> Self {
        Self::Fault(error)
    }
}

impl DispatchJournalError {
    /// Returns the candidate receipt from an uncertain append.
    #[must_use]
    pub fn uncertain_receipt(&self) -> Option<JournalReceipt<DispatchLog>> {
        match self {
            Self::Journal(error) => error.uncertain_receipt(),
            Self::Record(_) | Self::Fault(_) => None,
        }
    }
}
