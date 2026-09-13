//! Immutable authority, discovery, extraction, and fact contracts.

use backend_version::{ObjectVersion, Schema};

macro_rules! schema {
    ($name:ident, $ty:expr) => {
        #[doc = concat!("Canonical schema marker for `", stringify!($name), "` values.")]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct $name {
            _private: (),
        }

        impl Schema for $name {
            const DOMAIN: u8 = 0x44;
            const TYPE: u16 = $ty;
            type Value = [u8];

            fn encode(value: &Self::Value, output: &mut Vec<u8>) {
                output.extend_from_slice(&(value.len() as u64).to_be_bytes());
                output.extend_from_slice(value);
            }
        }
    };
}

schema!(InputContentSchema, 1);
schema!(InputManifestSchema, 2);
schema!(ProducerSchema, 3);
schema!(ToolchainSchema, 4);
schema!(ContractSchema, 5);
schema!(ProfileSchema, 6);
schema!(FlowSchema, 7);
schema!(SemanticBasisSchema, 8);
schema!(SessionSchema, 9);
schema!(FactSchema, 10);
schema!(FactKeySchema, 11);
schema!(FactValueSchema, 12);
schema!(CommandSchema, 13);
schema!(AuthorityEpochSchema, 14);
schema!(SyntaxProducerSchema, 15);

/// Content identity for one input.
pub type InputContentVersion = ObjectVersion<InputContentSchema>;
/// Identity of a complete input manifest.
pub type InputManifestId = ObjectVersion<InputManifestSchema>;
/// Identity of the producer implementation.
pub type ProducerId = ObjectVersion<ProducerSchema>;
/// Identity of an external toolchain.
pub type ToolchainId = ObjectVersion<ToolchainSchema>;
/// Identity of the authority protocol contract.
pub type ContractId = ObjectVersion<ContractSchema>;
/// Identity of a profile.
pub type ProfileId = ObjectVersion<ProfileSchema>;
/// Identity of flow configuration.
pub type FlowId = ObjectVersion<FlowSchema>;
/// Identity of semantic basis.
pub type SemanticBasisId = ObjectVersion<SemanticBasisSchema>;
/// Identity of a session.
pub type SessionId = ObjectVersion<SessionSchema>;
/// Identity of one extracted fact payload.
pub type FactVersion = ObjectVersion<FactSchema>;
/// Stable typed key identity for one extracted fact record.
pub type FactKey = backend_version::ObjectKey<FactKeySchema>;
/// Canonical typed value identity for one extracted fact record.
pub type FactValueVersion = ObjectVersion<FactValueSchema>;
/// Identity of one complete supervised command specification.
pub type CommandId = ObjectVersion<CommandSchema>;
/// Identity of one authority registration epoch.
pub type AuthorityEpoch = ObjectVersion<AuthorityEpochSchema>;
/// Identity of one local syntax producer and its grammar contract.
pub type SyntaxProducerId = ObjectVersion<SyntaxProducerSchema>;

/// Constructs a version-owned identity under a schema marker.
#[must_use]
pub fn typed_of<T: Schema<Value = [u8]>>(bytes: &[u8]) -> ObjectVersion<T> {
    ObjectVersion::from_value(bytes)
}

mod authority;
mod discovery;
mod extraction;
mod facts;
mod session;
mod snapshot;

pub use authority::{Authority, AuthorityError, AuthorityIdentity};
pub use discovery::{
    DiscoveryDelta, DiscoverySnapshot, Input, InputChange, InputKind, InputManifest, ManifestError,
    partial_coverage,
};
pub use extraction::{Extraction, ExtractionError};
pub use facts::{FactChange, FactEvidence, FactKind, FactRecord};
pub use session::SessionKey;
pub use snapshot::{
    FactDelta, FactDeltaChange, FactDeltaError, FactSnapshot, FactSnapshotError, PreparedFactDelta,
};

#[cfg(test)]
pub(crate) use authority::AuthorityAdmissionError;
pub(crate) use authority::{AuthorityFence, AuthorityRegistry, CompleteAuthorityCoverage};
pub(crate) use facts::{
    FactIndex, FactRelation, FactRootRelation, FactSet, FactSetError,
    record_evidence_matches_snapshot,
};

fn manifest_scope(manifest: &InputManifest) -> backend_version::ScopeRoot {
    backend_version::ScopeRoot::from_bytes(manifest.digest().to_bytes())
}
