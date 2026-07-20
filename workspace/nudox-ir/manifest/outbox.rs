//! Outbox: a staging area for sealed [`GenerationStamp`]s awaiting downstream
//! processing (e.g. registry ingestion, search indexing, artifact publication).
//!
//! # Design K12 / Issue 5 — structural Hash ①/② discipline
//!
//! The entire point of this module is that [`OutboxEntry::generation`] has type
//! [`GenerationStamp`], **not** [`crate::change::CasKey`]. Those two types are
//! distinct newtypes over the same 32-byte representation, so it is a
//! **compile error** to pass a `CasKey` (Hash ②, the CAS address of a manifest
//! blob) where a `GenerationStamp` (Hash ①, the logical generation identity) is
//! required.
//!
//! # Trait contract
//!
//! [`Outbox`] is an `object-safe`, `Send + Sync` trait so it can be used as a
//! `dyn Outbox` in async runtimes. Implementations must be safe to share across
//! threads (both `append` and `pending` take `&self`).
//!
//! [`InMemoryOutbox`] is the canonical in-process implementation used in tests
//! and single-node deployments.

use std::sync::Mutex;

use crate::change::{ChangeId, ChannelName, GenerationStamp, PackageLineageId};
use thiserror::Error;

/// A pending generation that has been sealed but not yet consumed by the
/// downstream pipeline (registry ingestion, search, artifact publication).
///
/// `generation` stores the logical identity (Hash ①). Never store a
/// `CasKey` (Hash ②) here — the type system prevents it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    /// Package lineage (ecosystem + name) that was sealed.
    pub package: PackageLineageId,
    /// Logical identity of the sealed generation (Hash ①).
    ///
    /// This is a [`GenerationStamp`], not a [`crate::change::CasKey`] —
    /// the type difference enforces the K12 Hash ①/② discipline at
    /// compile time.
    pub generation: GenerationStamp,
    /// Channel the generation was sealed into.
    pub channel: ChannelName,
    /// The most-recently applied change at seal time, if the channel is
    /// non-empty. Used by consumers to gate downstream jobs on tip alignment.
    pub tip_change: Option<ChangeId>,
}

/// Errors returned by [`Outbox`] operations.
#[derive(Debug, Error)]
pub enum OutboxError {
    /// The outbox's internal lock was poisoned (a previous holder panicked).
    #[error("outbox lock poisoned")]
    LockPoisoned,
    /// A downstream storage error (for persistent outbox implementations).
    #[error("outbox storage error: {0}")]
    Storage(String),
}

/// A staging area for sealed generation stamps awaiting downstream processing.
///
/// # Object safety
///
/// Both methods take `&self` so the trait is object-safe and can be used as
/// `dyn Outbox`. Implementations must be `Send + Sync`.
///
/// # Ordering guarantees
///
/// `pending()` returns entries in the order they were `append()`ed. Consumers
/// must not assume global ordering across process restarts; a persistent
/// implementation should use a monotonic sequence number.
pub trait Outbox: Send + Sync {
    /// Stage a new [`OutboxEntry`].
    ///
    /// Implementations must be idempotent with respect to the `generation`
    /// stamp when possible (de-duplicate on `OutboxEntry::generation`), though
    /// the in-memory implementation does not enforce this.
    fn append(&self, entry: OutboxEntry) -> Result<(), OutboxError>;

    /// Return all pending entries in insertion order.
    ///
    /// This does **not** drain the outbox; callers that want drain-on-ack
    /// semantics must implement a separate acknowledgement call.
    fn pending(&self) -> Result<Vec<OutboxEntry>, OutboxError>;
}

/// An in-process, in-memory outbox implementation.
///
/// Intended for tests and single-process deployments where durability across
/// restarts is not required. All entries are held in a `Mutex<Vec<_>>`; reads
/// and writes are O(n) in the number of pending entries.
///
/// # Thread safety
///
/// Both `append` and `pending` acquire the mutex for the duration of the call.
/// There is no separate drain operation; callers should read `pending()` and
/// then call `clear()` under application-level coordination if needed.
#[derive(Debug, Default)]
pub struct InMemoryOutbox {
    entries: Mutex<Vec<OutboxEntry>>,
}

impl InMemoryOutbox {
    /// Create a new, empty in-memory outbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remove all pending entries and return them in insertion order.
    ///
    /// This is the drain operation — it atomically empties the outbox and
    /// returns what was in it. Use this when you want consume-once semantics.
    pub fn drain(&self) -> Result<Vec<OutboxEntry>, OutboxError> {
        let mut guard = self.entries.lock().map_err(|_| OutboxError::LockPoisoned)?;
        Ok(std::mem::take(&mut *guard))
    }

    /// Return the number of pending entries without cloning them.
    pub fn len(&self) -> Result<usize, OutboxError> {
        let guard = self.entries.lock().map_err(|_| OutboxError::LockPoisoned)?;
        Ok(guard.len())
    }

    /// Return `true` if there are no pending entries.
    pub fn is_empty(&self) -> Result<bool, OutboxError> {
        Ok(self.len()? == 0)
    }
}

impl Outbox for InMemoryOutbox {
    fn append(&self, entry: OutboxEntry) -> Result<(), OutboxError> {
        let mut guard = self.entries.lock().map_err(|_| OutboxError::LockPoisoned)?;
        guard.push(entry);
        Ok(())
    }

    fn pending(&self) -> Result<Vec<OutboxEntry>, OutboxError> {
        let guard = self.entries.lock().map_err(|_| OutboxError::LockPoisoned)?;
        Ok(guard.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::change::{EcosystemId, PackageName};

    use super::*;

    fn gen_stamp(byte: u8) -> GenerationStamp {
        // Build a trivial deterministic stamp from raw bytes for test purposes.
        // Production code always uses generation_stamp_v3(); this shortcut is
        // test-only.
        GenerationStamp::from_domain("test.gen", &[byte; 32])
    }

    fn lineage(eco: &str, name: &str) -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new(eco), PackageName::new(name))
    }

    fn entry(byte: u8) -> OutboxEntry {
        OutboxEntry {
            package: lineage("cargo", "my-crate"),
            generation: gen_stamp(byte),
            channel: ChannelName::new("main"),
            tip_change: None,
        }
    }

    #[test]
    fn in_memory_outbox_append_and_pending() {
        let outbox = InMemoryOutbox::new();
        assert!(outbox.is_empty().unwrap());

        outbox.append(entry(1)).unwrap();
        outbox.append(entry(2)).unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].generation, gen_stamp(1));
        assert_eq!(pending[1].generation, gen_stamp(2));

        // pending() must not drain
        let pending2 = outbox.pending().unwrap();
        assert_eq!(pending2.len(), 2);
    }

    #[test]
    fn in_memory_outbox_drain() {
        let outbox = InMemoryOutbox::new();
        outbox.append(entry(10)).unwrap();
        outbox.append(entry(20)).unwrap();

        let drained = outbox.drain().unwrap();
        assert_eq!(drained.len(), 2);
        assert!(outbox.is_empty().unwrap(), "drain must empty the outbox");
    }

    #[test]
    fn in_memory_outbox_preserves_insertion_order() {
        let outbox = InMemoryOutbox::new();
        for i in 0u8..5 {
            outbox.append(entry(i)).unwrap();
        }
        let pending = outbox.pending().unwrap();
        for (i, e) in pending.iter().enumerate() {
            assert_eq!(e.generation, gen_stamp(i as u8));
        }
    }

    /// Confirm that `OutboxEntry::generation` is a `GenerationStamp` and that a
    /// `CasKey` cannot be used in its place (type-level — won't compile if you
    /// try). We just verify the Debug tag here.
    #[test]
    fn outbox_entry_stores_generation_stamp_not_cas_key() {
        let e = entry(0xAB);
        let debug = format!("{:?}", e.generation);
        assert!(
            debug.starts_with("gen:"),
            "generation field must be a GenerationStamp (gen: prefix), got: {debug}"
        );
    }

    /// An outbox can be used as `dyn Outbox` — verifies object safety.
    #[test]
    fn outbox_is_object_safe() {
        let outbox: Box<dyn Outbox> = Box::new(InMemoryOutbox::new());
        outbox.append(entry(5)).unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
    }
}
