//! Durable external effect state machines.
//!
//! The subsystem is split at the three boundaries that matter for correctness:
//! typed identity, in-memory phase transitions, and the append-only journal.
//! An effect is never sent to a sink until its intent and execution fence have
//! been synced. If a sink may have accepted a request and confirmation cannot
//! be persisted, the live state becomes [`EffectPhase::Ambiguous`] immediately.

mod coordinator;
mod journal_codec;
mod spec;
mod state;

#[cfg(test)]
mod tests;

pub use coordinator::{EffectCoordinator, EffectHandle};
/// Compatibility spelling retained for the original public name.
pub use journal_codec::JournalEffectPersistence as EffectJournalPersistence;
pub use journal_codec::{
    EffectCodec, EffectJournalRecord, EffectLog, EffectSnapshotLimits, EffectSnapshotReceipt,
    JournalEffectPersistence, RecoveredEffect,
};
pub use spec::{EffectKey, EffectKeySchema, EffectSpec, effect_key};
pub use state::{
    AmbiguousReason, EffectError, EffectFence, EffectPersistence, EffectPhase, EffectRecord,
    EffectSink, EffectState, MemoryPersistence, SinkApply, SinkError, SinkObservation,
};
