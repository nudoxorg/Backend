//! Defines key behavior for `server-workflow`, whose purpose is to reduce durable workflow events into deterministic recovery state.
//! This module owns the key invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Compact canonical stage-key construction.

use core::{borrow::Borrow, mem::size_of, ops::Deref};

use heart_identity::{
    ContentHasher, ContentId, Domain, FixedCanonicalRecord, GenerationId, ObjectDomain,
    StageKeyDomain, TAG_BYTES,
};
use heart_schema::OperationId;
use zerocopy::{
    Immutable, IntoBytes,
    byteorder::{LittleEndian, U32},
};

use crate::StageId;

/// Protocol-owned content domain for plane-independent capability facts.
pub use heart_identity::CapabilityDomain;
/// Capability identity is ordinary typed canonical content, not duplicate raw-byte machinery.
pub type CapabilityId = ContentId<CapabilityDomain>;

/// Protocol-owned content domain for normalized configuration facts.
pub use heart_identity::ConfigurationDomain;
/// Configuration identity is ordinary typed canonical content.
pub type ConfigurationId = ContentId<ConfigurationDomain>;

/// Verbose typed construction input. It is never retained in a repeated durable event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StageInput<ContentDomain> {
    /// Immutable generation pinned by the request.
    pub generation: GenerationId,
    /// Required canonical content.
    pub content: ContentId<ContentDomain>,
}

/// Compact 32-byte idempotency key committing every canonical stage fact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct StageKey([u8; 32]);

#[repr(C)]
#[derive(Immutable, IntoBytes)]
struct StageKeyRecord {
    operation: U32<LittleEndian>,
    stage: u8,
    content_domain: [u8; TAG_BYTES],
    generation: [u8; 32],
    content: [u8; 32],
    capability: [u8; 32],
    configuration: [u8; 32],
}

const STAGE_KEY_RECORD_BYTES: usize = size_of::<StageKeyRecord>();

impl FixedCanonicalRecord<STAGE_KEY_RECORD_BYTES> for StageKeyRecord {
    fn canonical_bytes(&self) -> &[u8; STAGE_KEY_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

impl StageKey {
    /// Streams fixed-width canonical inputs into a typed stage-key identity without allocation.
    #[must_use]
    pub fn derive<ContentDomain: Domain>(
        operation: OperationId,
        stage: StageId,
        input: &StageInput<ContentDomain>,
        capability: CapabilityId,
        configuration: ConfigurationId,
    ) -> Self {
        let record = StageKeyRecord {
            operation: U32::new(u32::from(operation)),
            stage: u8::from(stage),
            content_domain: *ContentDomain::TAG,
            generation: *input.generation,
            content: *input.content,
            capability: *capability,
            configuration: *configuration,
        };
        let mut hasher = ContentHasher::<StageKeyDomain>::new();
        hasher.write_record(&record);
        Self(*hasher.finalize().as_ref())
    }
}

impl From<[u8; 32]> for StageKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Deref for StageKey {
    type Target = [u8; 32];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl AsRef<[u8; 32]> for StageKey {
    fn as_ref(&self) -> &[u8; 32] {
        self
    }
}
impl Borrow<[u8; 32]> for StageKey {
    fn borrow(&self) -> &[u8; 32] {
        self
    }
}

/// Fixed Wave 1 result identity; concrete output prevents unbounded durable event payloads.
pub type StageOutput = ContentId<ObjectDomain>;
