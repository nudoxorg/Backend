//! Canonical transition payloads and checked closure extension.

use super::super::owner::WorkspaceError;
use super::{
    CommitPayloadSchema, DeltaPayloadSchema, ManifestPayloadSchema, PersistedTransition,
    RequestPayloadSchema, TransactionId, TransactionPayloadSchema, TransactionSchema,
};
use backend_store::{
    ClosureManifest, DurableManifest, ManifestChange, ObjectId, RelationAdmissionRegistry,
    TypedObject, WorkspaceClosure,
};
use backend_version::{
    CheckedCommit, CheckedWorkspaceTransition, ObjectKey, Schema, SchemaIdentity, WorkspaceManifest,
};

/// Extends a checked closure with transition payloads using an explicit
/// relation admission registry.
pub(crate) fn materialize_transition_closure_with_registry(
    manifest: &WorkspaceManifest,
    delta: &CheckedWorkspaceTransition,
    commit: &CheckedCommit,
    transaction: TransactionId,
    request: [u8; 32],
    closure: &WorkspaceClosure,
    registry: &RelationAdmissionRegistry,
) -> Result<WorkspaceClosure, WorkspaceError> {
    let payloads = payload_objects(request, transaction, manifest, delta, commit);
    materialize_transition_closure_from_payloads(
        manifest, delta, commit, &payloads, closure, registry,
    )
}

/// Materializes the transition closure and returns the fixed payload identities
/// computed from the same canonical bytes. Callers that retain the prepared
/// transition should cache these identities so later pack/head publication does
/// not re-encode a large manifest or delta.
pub(crate) fn materialize_transition_closure_with_ids(
    manifest: &WorkspaceManifest,
    delta: &CheckedWorkspaceTransition,
    commit: &CheckedCommit,
    transaction: TransactionId,
    request: [u8; 32],
    closure: &WorkspaceClosure,
    registry: &RelationAdmissionRegistry,
) -> Result<(WorkspaceClosure, [ObjectId; TRANSITION_PAYLOAD_COUNT]), WorkspaceError> {
    let payloads = payload_objects(request, transaction, manifest, delta, commit);
    let ids = [
        payloads[1].id(),
        payloads[2].id(),
        payloads[3].id(),
        payloads[4].id(),
        payloads[5].id(),
    ];
    let closure = materialize_transition_closure_from_payloads(
        manifest, delta, commit, &payloads, closure, registry,
    )?;
    Ok((closure, ids))
}

fn materialize_transition_closure_from_payloads(
    manifest: &WorkspaceManifest,
    delta: &CheckedWorkspaceTransition,
    commit: &CheckedCommit,
    payloads: &[TypedObject; TRANSITION_PAYLOAD_COUNT + 1],
    closure: &WorkspaceClosure,
    registry: &RelationAdmissionRegistry,
) -> Result<WorkspaceClosure, WorkspaceError> {
    // Probe each immutable payload by its object identity through the
    // manifest's persistent index.  The explicit membership witness
    // distinguishes an exact retry (the object is already present) from a
    // first publication (the object is absent) without manufacturing a
    // delete delta or conflating `BeforeMismatch` with absence.  The actual
    // update is one sorted path-copy delta.
    let mut changes = Vec::with_capacity(payloads.len().saturating_mul(2));
    let retained = payloads.iter().map(TypedObject::id).collect::<Vec<_>>();
    for previous in closure.manifest().objects() {
        if is_transition_payload(previous.schema()) && !retained.contains(&previous.id()) {
            changes.push(ManifestChange::delete(previous).map_err(WorkspaceError::store)?);
        }
    }
    for payload in payloads {
        if !closure.manifest().contains_object_id(payload.id()) {
            changes.push(ManifestChange::insert(payload).map_err(WorkspaceError::store)?);
        }
    }
    changes.sort_by_key(ManifestChange::key);
    let objects = if changes.is_empty() {
        closure.manifest().clone()
    } else {
        closure
            .manifest()
            .prepare_delta(&changes)
            .map_err(WorkspaceError::store)?
            .commit()
    };
    closure
        .rebind_checked_transition_with_registry(manifest, delta, Some(commit), objects, registry)
        .map_err(WorkspaceError::store)
}

fn is_transition_payload(schema: SchemaIdentity) -> bool {
    [
        SchemaIdentity::new(
            TransactionSchema::DOMAIN,
            TransactionSchema::TYPE,
            TransactionSchema::VERSION,
        ),
        SchemaIdentity::new(
            RequestPayloadSchema::DOMAIN,
            RequestPayloadSchema::TYPE,
            RequestPayloadSchema::VERSION,
        ),
        SchemaIdentity::new(
            ManifestPayloadSchema::DOMAIN,
            ManifestPayloadSchema::TYPE,
            ManifestPayloadSchema::VERSION,
        ),
        SchemaIdentity::new(
            DeltaPayloadSchema::DOMAIN,
            DeltaPayloadSchema::TYPE,
            DeltaPayloadSchema::VERSION,
        ),
        SchemaIdentity::new(
            CommitPayloadSchema::DOMAIN,
            CommitPayloadSchema::TYPE,
            CommitPayloadSchema::VERSION,
        ),
        SchemaIdentity::new(
            TransactionPayloadSchema::DOMAIN,
            TransactionPayloadSchema::TYPE,
            TransactionPayloadSchema::VERSION,
        ),
    ]
    .contains(&schema)
}

fn transaction_bytes(transaction: TransactionId) -> Vec<u8> {
    transaction.as_bytes().to_vec()
}

fn payload_envelope(transaction: TransactionId, payload: &[u8]) -> Vec<u8> {
    let mut envelope = Vec::with_capacity(32 + payload.len());
    envelope.extend_from_slice(&transaction.as_bytes());
    envelope.extend_from_slice(payload);
    envelope
}

/// Reconstructs the bounded persisted transition envelope from the selected
/// store closure's lazy manifest index.  Only the fixed payload identities in
/// the authenticated workspace pack are fetched; unrelated closure objects
/// remain on disk and are admitted by the model or a typed relation loader
/// when needed.
pub(crate) fn persisted_from_store_manifest(
    manifest: &DurableManifest,
    transaction: TransactionId,
    payload_ids: [ObjectId; TRANSITION_PAYLOAD_COUNT],
    auxiliary_ids: &[ObjectId],
) -> Result<PersistedTransition, WorkspaceError> {
    let request =
        payload_from_store_manifest::<RequestPayloadSchema>(manifest, payload_ids[0], transaction)?;
    let manifest_bytes = payload_from_store_manifest::<ManifestPayloadSchema>(
        manifest,
        payload_ids[1],
        transaction,
    )?;
    let delta =
        payload_from_store_manifest::<DeltaPayloadSchema>(manifest, payload_ids[2], transaction)?;
    let commit =
        payload_from_store_manifest::<CommitPayloadSchema>(manifest, payload_ids[3], transaction)?;
    let transaction_payload = payload_from_store_manifest::<TransactionPayloadSchema>(
        manifest,
        payload_ids[4],
        transaction,
    )?;
    if request.len() != 32 || transaction_payload.as_ref() != transaction.as_bytes() {
        return Err(WorkspaceError::Store(
            "workspace transition payload identity mismatch".to_owned(),
        ));
    }

    // Keep a compact compatibility closure containing the bounded direct
    // frontier plus the fixed transition payloads. Relation descendants are
    // intentionally absent here: a model opens them by their checked root
    // through the durable store capability instead of hydrating the closure.
    let mut payload_objects = Vec::with_capacity(
        payload_ids
            .len()
            .checked_add(auxiliary_ids.len())
            .ok_or(WorkspaceError::Bounds)?,
    );
    for object_id in payload_ids {
        let object = manifest
            .get(object_id)
            .map_err(WorkspaceError::store)?
            .ok_or_else(|| WorkspaceError::Store("workspace payload is missing".to_owned()))?;
        payload_objects.push(object);
    }
    for &object_id in auxiliary_ids {
        if payload_ids.contains(&object_id) {
            return Err(WorkspaceError::Corrupt(
                "workspace auxiliary object duplicates transition payload",
            ));
        }
        let object = manifest
            .get(object_id)
            .map_err(WorkspaceError::store)?
            .ok_or_else(|| {
                WorkspaceError::Store("workspace auxiliary object is missing".to_owned())
            })?;
        payload_objects.push(object);
    }
    payload_objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let closure = ClosureManifest::new(payload_objects).map_err(WorkspaceError::store)?;
    let closure_bytes = closure
        .encode(super::MAX_RECORD_BYTES)
        .map_err(WorkspaceError::store)?;
    Ok(PersistedTransition::from_parts(
        request
            .as_ref()
            .try_into()
            .map_err(|_| WorkspaceError::Corrupt("workspace request payload"))?,
        transaction,
        manifest_bytes,
        delta,
        commit,
        closure,
        closure_bytes.into_boxed_slice(),
    ))
}

fn payload_from_store_manifest<S: Schema>(
    manifest: &DurableManifest,
    object_id: ObjectId,
    transaction: TransactionId,
) -> Result<Box<[u8]>, WorkspaceError> {
    let schema = SchemaIdentity::new(S::DOMAIN, S::TYPE, S::VERSION);
    let prefix = transaction.as_bytes();
    let object = manifest
        .get(object_id)
        .map_err(WorkspaceError::store)?
        .ok_or_else(|| WorkspaceError::Store("workspace payload is outside closure".to_owned()))?;
    if object.schema() != schema || object.id() != object_id {
        return Err(WorkspaceError::Store(
            "workspace transition payload identity mismatch".to_owned(),
        ));
    }
    if !object.bytes().starts_with(&prefix) {
        return Err(WorkspaceError::Store(
            "workspace transition payload transaction mismatch".to_owned(),
        ));
    }
    Ok(object.bytes()[prefix.len()..].to_vec().into_boxed_slice())
}

/// Number of fixed payload identities embedded in the workspace pack index.
pub(crate) const TRANSITION_PAYLOAD_COUNT: usize = 5;

fn payload_objects(
    request: [u8; 32],
    transaction: TransactionId,
    manifest: &WorkspaceManifest,
    delta: &CheckedWorkspaceTransition,
    commit: &CheckedCommit,
) -> [TypedObject; TRANSITION_PAYLOAD_COUNT + 1] {
    let manifest_bytes = manifest.encode();
    let delta_bytes = delta.encode();
    let commit_bytes = commit.encode();
    let request_bytes = payload_envelope(transaction, &request);
    let manifest_payload = payload_envelope(transaction, &manifest_bytes);
    let delta_payload = payload_envelope(transaction, &delta_bytes);
    let commit_payload = payload_envelope(transaction, &commit_bytes);
    let transaction_object_bytes = transaction_bytes(transaction);
    let transaction_payload = payload_envelope(transaction, &transaction_object_bytes);
    [
        TypedObject::from_value(
            &ObjectKey::<TransactionSchema>::from_value(&transaction_object_bytes),
            &transaction_object_bytes,
        ),
        TypedObject::from_value(
            &ObjectKey::<RequestPayloadSchema>::from_value(&request_bytes),
            &request_bytes,
        ),
        TypedObject::from_value(
            &ObjectKey::<ManifestPayloadSchema>::from_value(&manifest_payload),
            &manifest_payload,
        ),
        TypedObject::from_value(
            &ObjectKey::<DeltaPayloadSchema>::from_value(&delta_payload),
            &delta_payload,
        ),
        TypedObject::from_value(
            &ObjectKey::<CommitPayloadSchema>::from_value(&commit_payload),
            &commit_payload,
        ),
        TypedObject::from_value(
            &ObjectKey::<TransactionPayloadSchema>::from_value(&transaction_payload),
            &transaction_payload,
        ),
    ]
}
