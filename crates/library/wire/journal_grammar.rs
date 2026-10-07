//! The maintained grammar of checked BVIEWJ01 version-3 journal frames.
//!
//! Snapshot rows, bases, certificates and compact transition records have
//! identical fields and admission rules in envelopes 21, 22 and 23. Document
//! selections, failure replies and Python metadata changed only live replies.
//! Incompatible persisted fields require a new journal grammar; increasing
//! the live DTO version never extends this domain implicitly.

use crate::{CommittedViewDelta, CoverageCapability, Cursor, ViewDto, ViewRoot};

/// Codec domain selected after a host checks a BVIEWJ01 version-3 frame and
/// its container, checksum, workspace and capability provenance.
///
/// This marker supplies no authority: snapshots still require the live
/// producer capability, and events still require their checked predecessor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalViewGrammarV3 {
    _private: (),
}

impl JournalViewGrammarV3 {
    /// Version of the checked journal container described by this domain.
    pub const CONTAINER_VERSION: u8 = 3;

    /// Selects this fixed grammar after the host validates its journal frame.
    ///
    /// # Errors
    /// Refuses every container version outside this maintained grammar.
    pub fn from_checked_container(version: u8) -> Result<Self, String> {
        if version != Self::CONTAINER_VERSION {
            return Err("unsupported persisted view journal grammar".to_owned());
        }
        Ok(Self { _private: () })
    }

    /// Checks the explicitly maintained snapshot/compact-event envelope
    /// versions. This does not admit a live client or any payload identity.
    ///
    /// # Errors
    /// Refuses unknown and incompatible persisted envelope versions.
    pub fn check_envelope_version(self, version: u16) -> Result<(), String> {
        if matches!(version, 21 | 22 | 23) {
            Ok(())
        } else {
            Err(format!(
                "unsupported persisted view envelope version {version}; journal grammar 3 reads 21, 22 and 23"
            ))
        }
    }

    /// Re-admits a persisted snapshot through the shared strict row/root
    /// certificate decoder under its current producer capability.
    ///
    /// # Errors
    /// Refuses malformed fields, unknown versions and invalid proof/basis.
    pub fn decode_snapshot(
        self,
        bytes: &[u8],
        capability: CoverageCapability,
    ) -> Result<ViewDto, String> {
        super::command::decode_journal_view(bytes, capability, self)
    }

    /// Re-admits a persisted compact event through the shared strict checked
    /// transition decoder against the exact retained predecessor.
    ///
    /// # Errors
    /// Refuses malformed fields, unknown versions and invalid transitions.
    pub fn decode_event(
        self,
        bytes: &[u8],
        previous: Cursor,
        base: &ViewRoot,
    ) -> Result<(Cursor, CommittedViewDelta), String> {
        super::event::decode_journal_compact_view_event(bytes, previous, base, self)
    }
}
