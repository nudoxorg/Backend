//! Canonical effect journal façade.
//!
//! Durable record framing, snapshot selection, pointer publication, and
//! replay are private modules with one typed public persistence façade.

mod admission;
mod compaction;
mod persistence;
mod pointer;
mod record;
mod recovered;
mod replay;
mod snapshot;

pub use crate::schema::EffectLog;
pub use persistence::JournalEffectPersistence;
pub use record::EffectJournalRecord;
pub use recovered::RecoveredEffect;
pub use snapshot::{EffectSnapshotLimits, EffectSnapshotReceipt};

use super::spec::EffectSpec;
use super::state::EffectError;

/// Canonical intent/receipt codec used by a filesystem effect journal.
pub trait EffectCodec<E: EffectSpec>: Send + Sync + 'static {
    /// Encodes one canonical intent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn encode_intent(&self, intent: &E::Intent) -> Result<Vec<u8>, EffectError>;
    /// Decodes one canonical intent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn decode_intent(&self, bytes: &[u8]) -> Result<E::Intent, EffectError>;
    /// Encodes one canonical receipt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn encode_receipt(&self, receipt: &E::Receipt) -> Result<Vec<u8>, EffectError>;
    /// Decodes one canonical receipt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn decode_receipt(&self, bytes: &[u8]) -> Result<E::Receipt, EffectError>;
}
