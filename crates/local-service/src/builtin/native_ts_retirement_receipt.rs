//! Bounded, owner-sealed claims for a fully witnessed empty native TS program.
//! Decoding or constructing an object does not admit a transition. Cold replay
//! must independently derive every field and check selected object custody.
use super::{BuiltinModelError, native_ts_admission::ValidatedEmptyNativeTsRetirement, owner_receipt_context::OwnerReceiptContext};
use backend_engine::builtin::NativeTsRetirementReceiptSchema;
use backend_library::{NativeOwnerStatement, seal_native_owner_statement, verify_native_owner_statement};
use backend_store::TypedObject;
use backend_version::ObjectKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const MAX_RECEIPT_BYTES: usize = 8192;

/// Inputs derived from the complete selected transition by the owner planner.
/// Commitments use canonical, domain-separated complete maps, including empty
/// partitions and absence; they are not arbitrary subset digests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeTsRetirementReceiptInputs {
    pub(super) project: [u8; 32],
    pub(super) predecessor_root: [u8; 32],
    pub(super) predecessor_closure: [u8; 32],
    pub(super) predecessor_sequence: u64,
    pub(super) predecessor_evidence_root: Option<[u8; 32]>,
    pub(super) project_after_image: [u8; 32],
    pub(super) source_version: [u8; 32],
    pub(super) selected_before: [u8; 32],
    pub(super) selected_after: [u8; 32],
    pub(super) captures_before: [u8; 32],
    pub(super) captures_after: [u8; 32],
    pub(super) native_delta: [u8; 32],
}

/// Issuing root is computed before receipt selection, avoiding a hash cycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeTsRetirementReceiptStatement {
    pub(super) workspace: [u8; 32],
    pub(super) project: [u8; 32],
    pub(super) predecessor_root: [u8; 32],
    pub(super) predecessor_closure: [u8; 32],
    pub(super) predecessor_sequence: u64,
    pub(super) predecessor_evidence_root: Option<[u8; 32]>,
    pub(super) issuing_workspace_root: [u8; 32],
    pub(super) issuing_generation: u64,
    pub(super) target_evidence_root: Option<[u8; 32]>,
    pub(super) project_after_image: [u8; 32],
    pub(super) source_version: [u8; 32],
    pub(super) selected_before: [u8; 32],
    pub(super) selected_after: [u8; 32],
    pub(super) captures_before: [u8; 32],
    pub(super) captures_after: [u8; 32],
    pub(super) native_delta: [u8; 32],
    pub(super) transaction: [u8; 32],
}
impl NativeOwnerStatement for NativeTsRetirementReceiptStatement {
    const DOMAIN: &'static [u8] = b"nudox.native-typescript.empty-retirement-receipt.v1\0";
}
impl NativeTsRetirementReceiptStatement {
    fn valid(&self) -> bool {
        self.workspace != [0; 32] && self.project != [0; 32]
            && self.predecessor_root != [0; 32] && self.predecessor_closure != [0; 32]
            && self.predecessor_evidence_root != Some([0; 32])
            && self.target_evidence_root != Some([0; 32])
            && self.issuing_workspace_root != [0; 32]
            && self.predecessor_sequence.checked_add(1) == Some(self.issuing_generation)
            && self.project_after_image != [0; 32] && self.source_version != [0; 32]
            && self.selected_before != [0; 32] && self.selected_after != [0; 32]
            && self.captures_before != [0; 32] && self.captures_after != [0; 32]
            && self.native_delta != [0; 32] && self.transaction != [0; 32]
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeTsRetirementReceipt {
    statement: NativeTsRetirementReceiptStatement,
    seal: [u8; 32],
}
/// Issuer-only capability retains the live witness. A decoded claim cannot
/// produce this capability, and each grant must revalidate it.
pub(super) struct IssuedNativeTsRetirementReceipt {
    receipt: NativeTsRetirementReceipt,
    validated: Arc<ValidatedEmptyNativeTsRetirement>,
}
impl IssuedNativeTsRetirementReceipt {
    pub(super) fn check_cancelled(&self) -> Result<(), BuiltinModelError> { self.validated.revalidate() }
    pub(super) fn object(&self) -> Result<TypedObject, BuiltinModelError> {
        self.validated.revalidate()?;
        self.receipt.object()
    }
    pub(super) fn statement(&self) -> &NativeTsRetirementReceiptStatement { self.receipt.statement() }
}
impl NativeTsRetirementReceipt {
    pub(super) fn issue(owner: &OwnerReceiptContext, validated: Arc<ValidatedEmptyNativeTsRetirement>, transaction: backend_engine::TransactionId, issuing_workspace_root: [u8; 32], issuing_generation: u64, target_evidence_root: Option<[u8; 32]>) -> Result<IssuedNativeTsRetirementReceipt, BuiltinModelError> {
        validated.revalidate()?;
        let inputs = validated.receipt_inputs()?;
        let receipt = owner.with_bound(|key, workspace| {
            let statement = NativeTsRetirementReceiptStatement {
                workspace, project: inputs.project, predecessor_root: inputs.predecessor_root,
                predecessor_closure: inputs.predecessor_closure, predecessor_sequence: inputs.predecessor_sequence,
                predecessor_evidence_root: inputs.predecessor_evidence_root, issuing_workspace_root,
                issuing_generation, target_evidence_root, project_after_image: inputs.project_after_image,
                source_version: inputs.source_version, selected_before: inputs.selected_before,
                selected_after: inputs.selected_after, captures_before: inputs.captures_before,
                captures_after: inputs.captures_after, native_delta: inputs.native_delta, transaction: transaction.as_bytes(),
            };
            if !statement.valid() { return Err(BuiltinModelError("invalid native TS retirement receipt statement".into())); }
            let seal = seal_native_owner_statement(key, &statement).map_err(|_| BuiltinModelError("native TS retirement receipt sealing failed".into()))?;
            Ok(Self { statement, seal })
        })?;
        validated.revalidate()?;
        Ok(IssuedNativeTsRetirementReceipt { receipt, validated })
    }
    pub(super) fn statement(&self) -> &NativeTsRetirementReceiptStatement { &self.statement }
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, BuiltinModelError> {
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES { return Err(BuiltinModelError("invalid native TS retirement receipt length".into())); }
        let receipt: Self = serde_json::from_slice(bytes).map_err(|_| BuiltinModelError("invalid native TS retirement receipt".into()))?;
        if !receipt.statement.valid() || receipt.seal == [0; 32]
            || serde_json::to_vec(&receipt).map_err(|_| BuiltinModelError("invalid native TS retirement receipt".into()))? != bytes {
            return Err(BuiltinModelError("noncanonical native TS retirement receipt".into()));
        }
        Ok(receipt)
    }
    pub(super) fn object(&self) -> Result<TypedObject, BuiltinModelError> {
        if !self.statement.valid() || self.seal == [0; 32] { return Err(BuiltinModelError("invalid native TS retirement receipt".into())); }
        let bytes = serde_json::to_vec(self).map_err(|_| BuiltinModelError("invalid native TS retirement receipt".into()))?;
        if bytes.len() > MAX_RECEIPT_BYTES { return Err(BuiltinModelError("native TS retirement receipt exceeds bound".into())); }
        let key = ObjectKey::<NativeTsRetirementReceiptSchema>::from_value(&self.statement.project);
        Ok(TypedObject::from_value(&key, &bytes))
    }
    /// The expected statement must be independently recomputed from the
    /// admitted predecessor and target, and the caller must verify custody.
    pub(super) fn verify(&self, owner: &OwnerReceiptContext, expected: &NativeTsRetirementReceiptStatement) -> Result<(), BuiltinModelError> {
        if !self.statement.valid() || &self.statement != expected { return Err(BuiltinModelError("native TS retirement receipt claim mismatch".into())); }
        owner.with_bound(|key, workspace| {
            if self.statement.workspace != workspace { return Err(BuiltinModelError("native TS retirement receipt workspace mismatch".into())); }
            verify_native_owner_statement(key, &self.statement, &self.seal).map_err(|_| BuiltinModelError("native TS retirement receipt seal invalid".into()))
        })
    }
}

/// Commitments independently recomputed from complete authenticated relations.
pub(super) struct ReplayedEmptyNativeTsFrontiers {
    pub(super) capture_keys: Vec<backend_engine::builtin::ProductSemanticPublicationKey>,
    pub(super) selected_before: [u8; 32],
    pub(super) selected_after: [u8; 32],
    pub(super) captures_before: [u8; 32],
    pub(super) captures_after: [u8; 32],
}

/// Replay the complete semantic and capture transformation. The supplied
/// relations must themselves come from authenticated predecessor/target
/// snapshots. The caller must also verify the exact target relation roots,
/// including their metadata, from the predecessor and these deletions.
pub(super) fn replay_empty_native_ts_frontiers(
    owner: [u8; 32],
    base_semantic: &backend_engine::WorkspaceRelationHandle<backend_engine::builtin::ProductSemanticPublicationRelation>,
    target_semantic: &backend_engine::WorkspaceRelationHandle<backend_engine::builtin::ProductSemanticPublicationRelation>,
    base_capture: Option<&backend_engine::WorkspaceRelationHandle<backend_engine::builtin::ProductSemanticCaptureRelation>>,
    target_capture: Option<&backend_engine::WorkspaceRelationHandle<backend_engine::builtin::ProductSemanticCaptureRelation>>,
    shutdown: Option<&crate::listener::ListenerShutdown>,
) -> Result<ReplayedEmptyNativeTsFrontiers, BuiltinModelError> {
    use backend_engine::builtin::{ProductSemanticPublicationRelation as Semantic, ProductSemanticCaptureRelation as Capture, ProductSemanticPublicationRecord, ProductSemanticCaptureOutcome, SemanticUnavailableReason};
    use backend_semantic::vocabulary::LanguageProfile;
    use backend_version::Relation;
    use std::collections::{BTreeMap, BTreeSet};

    let mut work = 0usize;
    let mut total = 0usize;
    // Track all rows so every nonselected row must survive exactly. Charge
    // both source and target scans, including row overhead, against one cap.
    macro_rules! scan {
        ($relation:expr, $ty:ty, $domain:expr, $capture:expr, $target:expr) => {{
            let mut hash = blake3::Hasher::new();
            hash.update($domain); hash.update(&owner);
            let mut count = 0u64;
            let mut selected = BTreeSet::new();
            let mut selected_keys = Vec::new();
            let mut other = BTreeMap::new();
            let mut after = None;
            if let Some(relation) = $relation {
                loop {
                    if shutdown.is_some_and(|s| s.is_requested()) { return Err(BuiltinModelError("empty TypeScript replay cancelled".into())); }
                    let page = relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                        .map_err(|e| BuiltinModelError(format!("scan empty TypeScript replay frontier: {e}")))?;
                    for (key, row) in page.entries() {
                        total = total.checked_add(1).ok_or_else(|| BuiltinModelError("empty TypeScript replay count overflow".into()))?;
                        let mut k = Vec::new(); let mut v = Vec::new();
                        <$ty>::encode_key(key, &mut k); <$ty>::encode_value(row, &mut v);
                        work = work.checked_add(k.len()).and_then(|n| n.checked_add(v.len())).and_then(|n| n.checked_add(64))
                            .ok_or_else(|| BuiltinModelError("empty TypeScript replay work overflow".into()))?;
                        if total > backend_engine::application::MAX_MANIFEST_ENTRIES || work > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES {
                            return Err(BuiltinModelError("empty TypeScript replay frontier exceeds bound".into()));
                        }
                        if key.is_selected() && key.package_key().to_bytes() == owner && matches!(key.profile(), LanguageProfile::TypeScript(_)) {
                            count = count.checked_add(1).ok_or_else(|| BuiltinModelError("empty TypeScript replay count overflow".into()))?;
                            if count > 2 || k.len() > 8192 { return Err(BuiltinModelError("empty TypeScript replay selected frontier exceeds bound".into())); }
                            ($capture)(row)?;
                            hash.update(&(k.len() as u64).to_be_bytes()); hash.update(&k);
                            hash.update(&(v.len() as u64).to_be_bytes()); hash.update(&v);
                            if !selected.insert(k) { return Err(BuiltinModelError("duplicate empty TypeScript selected key".into())); }
                            selected_keys.push(key.clone());
                        } else if other.insert(k, v).is_some() {
                            return Err(BuiltinModelError("duplicate empty TypeScript frontier key".into()));
                        }
                    }
                    match page.next().cloned() { Some(next) => after = Some(next), None => break }
                }
            }
            hash.update(&count.to_be_bytes());
            let _ = $target;
            (selected, selected_keys, other, *hash.finalize().as_bytes())
        }};
    }
    let semantic_check_before = |_: &ProductSemanticPublicationRecord| Ok::<(), BuiltinModelError>(());
    let semantic_check_after = |row: &ProductSemanticPublicationRecord| {
        if row != &ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::ProjectAuthority) {
            return Err(BuiltinModelError("empty TypeScript target semantic remains available".into()));
        }
        Ok(())
    };
    let capture_check_before = |row: &backend_engine::builtin::ProductSemanticCaptureRecord| {
        if matches!(row.outcome(), ProductSemanticCaptureOutcome::Pending { .. }) {
            return Err(BuiltinModelError("cannot replay in-flight TypeScript capture retirement".into()));
        }
        Ok(())
    };
    let capture_check_after = |_: &backend_engine::builtin::ProductSemanticCaptureRecord| Ok::<(), BuiltinModelError>(());
    let (before_keys, _, before_other, selected_before) = scan!(Some(base_semantic), Semantic, b"nudox.empty-ts.selected-frontier.v1\0", semantic_check_before, false);
    let (after_keys, _, after_other, selected_after) = scan!(Some(target_semantic), Semantic, b"nudox.empty-ts.selected-frontier.v1\0", semantic_check_after, true);
    if before_keys != after_keys || before_other != after_other { return Err(BuiltinModelError("empty TypeScript semantic frontier transition mismatch".into())); }
    drop((before_keys, after_keys, before_other, after_other));
    let (_, capture_keys, before_other, captures_before) = scan!(base_capture, Capture, b"nudox.empty-ts.capture-frontier.v1\0", capture_check_before, false);
    let (after_keys, _, after_other, captures_after) = scan!(target_capture, Capture, b"nudox.empty-ts.capture-frontier.v1\0", capture_check_after, true);
    if !after_keys.is_empty() || before_other != after_other { return Err(BuiltinModelError("empty TypeScript capture frontier transition mismatch".into())); }
    if shutdown.is_some_and(|s| s.is_requested()) { return Err(BuiltinModelError("empty TypeScript replay cancelled".into())); }
    Ok(ReplayedEmptyNativeTsFrontiers { capture_keys, selected_before, selected_after, captures_before, captures_after })
}
