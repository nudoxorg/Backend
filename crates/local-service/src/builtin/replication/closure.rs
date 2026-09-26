//! Closure and input transfer phase of the owner remote state machine.

use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinReplication, BuiltinSemanticRelation,
    BuiltinValidator, PendingClosure, TransportMessage, WireIdentity,
};
use crate::reconcile::{product_frames_for_version_from_source, semantic_input_frame};
use backend_engine::ImmutableObjectSchema;

#[path = "closure/phase.rs"]
mod phase;

fn checked_missing_versions(
    source: Option<&mut crate::reconcile::ProductPageSource<BuiltinSemanticRelation>>,
    missing: &[WireIdentity],
) -> Result<std::collections::BTreeSet<[u8; 32]>, crate::protocol::ProtocolError> {
    let source = source.ok_or(crate::protocol::ProtocolError::InvalidControl(
        "missing closure source",
    ))?;
    missing
        .iter()
        .map(|identity| {
            let version = identity.as_bytes();
            let object_claim = backend_engine::schema_object_version_identity_claim::<
                ImmutableObjectSchema,
            >(&version)
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidCommand(
                    "worker requested a malformed object identity".to_owned(),
                )
            })?;
            if object_claim.context() != identity.context() {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker requested an object under the wrong schema context".to_owned(),
                ));
            }
            if source.object_bytes(version).is_none() {
                return Err(crate::protocol::ProtocolError::InvalidCommand(
                    "worker requested an object outside the admitted closure".to_owned(),
                ));
            }
            Ok(version)
        })
        .collect()
}
