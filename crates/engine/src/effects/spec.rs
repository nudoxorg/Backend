//! Typed effect identity and effect-specific validation.

use super::state::EffectError;
use blake3::Hasher;

/// Schema marker for deterministic effect idempotency keys.
#[derive(Debug)]
pub struct EffectKeySchema;

impl backend_version::Schema for EffectKeySchema {
    const DOMAIN: u8 = 0x82;
    const TYPE: u16 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Stable idempotency key for an effect intent.
pub type EffectKey = backend_version::ObjectVersion<EffectKeySchema>;

/// The semantic operation defines how an intent becomes a sink request.
pub trait EffectSpec: Send + Sync + 'static {
    /// Durable intent carried by this effect kind.
    type Intent: Send + Sync + 'static;
    /// Request accepted by the external sink.
    type Request: Clone + Send + Sync + 'static;
    /// Receipt returned by the sink.
    type Receipt: Clone + Send + Sync + 'static;

    /// Computes an idempotency key from the semantic operation.
    fn key(&self, intent: &Self::Intent) -> EffectKey;
    /// Converts the intent into the sink request.
    fn request(&self, intent: &Self::Intent) -> Self::Request;
    /// Validates a receipt against the exact key and request.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn validate_receipt(
        &self,
        key: EffectKey,
        request: &Self::Request,
        receipt: &Self::Receipt,
    ) -> Result<(), EffectError>;
}

/// Computes a key when a small adapter has canonical intent bytes available.
#[must_use]
pub fn effect_key(bytes: &[u8]) -> EffectKey {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.engine.effect.v2\0");
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    EffectKey::from_value(hasher.finalize().as_bytes())
}
