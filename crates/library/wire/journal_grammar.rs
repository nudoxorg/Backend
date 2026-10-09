//! The maintained grammar of checked BVIEWJ01 version-3 journal frames.
//!
//! Snapshot rows, bases, certificates and compact transition records
//! use the current DTO contract and the maintained historical envelopes 21–24.
//! Their document selections, failure replies, Python metadata and project
//! lockfile membership do not change persisted view fields. Every snapshot and event still passes current strict proof admission.
//! Incompatible persisted fields require a new journal grammar.

use super::DTO_VERSION;
use crate::{CommittedViewDelta, CoverageCapability, Cursor, ViewDto, ViewRoot};

// A live DTO bump must not silently widen the persisted journal domain.
// Review snapshot/event fields and historical compatibility before moving
// this fence, or introduce a new journal grammar for incompatible fields.
const _: () = assert!(
    DTO_VERSION == 25,
    "review persisted view journal compatibility before accepting a new DTO version"
);

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

    /// Checks the current snapshot/compact-event envelope and the explicitly
    /// maintained historical versions. This does not admit a live client or any
    /// payload identity.
    ///
    /// # Errors
    /// Refuses unknown and incompatible persisted envelope versions.
    pub fn check_envelope_version(self, version: u16) -> Result<(), String> {
        if version == DTO_VERSION || matches!(version, 21 | 22 | 23 | 24) {
            Ok(())
        } else {
            Err(format!(
                "unsupported persisted view envelope version {version}; journal grammar 3 reads current {DTO_VERSION} and historical 21, 22, 23 and 24"
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
