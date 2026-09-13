//! Typed replay results shared by recovery and compaction.

use super::super::spec::{EffectKey, EffectSpec};
use super::super::state::EffectState;
use std::collections::BTreeMap;

/// One typed effect entry reconstructed from a journal or snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredEffect<I, R> {
    /// Original typed intent.
    pub intent: I,
    /// Last valid phase after replay.
    pub state: EffectState<R>,
}

/// Fixed-width key index retained while replaying or materializing a snapshot.
pub(super) type RecoveredIndex<E> = BTreeMap<
    [u8; 32],
    (
        EffectKey,
        RecoveredEffect<<E as EffectSpec>::Intent, <E as EffectSpec>::Receipt>,
    ),
>;

/// Owned typed entries returned by replay.
pub(super) type RecoveredEffects<E> =
    Vec<RecoveredEffect<<E as EffectSpec>::Intent, <E as EffectSpec>::Receipt>>;
