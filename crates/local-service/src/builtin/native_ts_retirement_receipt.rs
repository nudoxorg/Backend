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
pub(super) const MAX_RETIREMENT_KEY_BYTES: usize = 2 * 4096 + 64;

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
    pub(super) operation_key: Option<[u8; 32]>,
    pub(super) request_identity: [u8; 32],
    pub(super) retired_profiles: Vec<[u8; 2]>,
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
            && self.operation_key != Some([0; 32]) && self.request_identity != [0; 32]
            && !self.retired_profiles.is_empty() && self.retired_profiles.len() <= 2
            && self.retired_profiles.windows(2).all(|p| p[0] < p[1])
            && self.retired_profiles.iter().all(|p| matches!(backend_semantic::vocabulary::LanguageProfile::try_from(*p), Ok(backend_semantic::vocabulary::LanguageProfile::TypeScript(_))))
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
    pub(super) fn issue(owner: &OwnerReceiptContext, validated: Arc<ValidatedEmptyNativeTsRetirement>, target: &super::profile::EmptyNativeTsPreparedTarget) -> Result<IssuedNativeTsRetirementReceipt, BuiltinModelError> {
        validated.revalidate()?;
        let inputs = validated.receipt_inputs()?;
        let receipt = owner.with_bound(|key, workspace| {
            let statement = NativeTsRetirementReceiptStatement {
                workspace, project: inputs.project, predecessor_root: inputs.predecessor_root,
                predecessor_closure: inputs.predecessor_closure, predecessor_sequence: inputs.predecessor_sequence,
                predecessor_evidence_root: inputs.predecessor_evidence_root, issuing_workspace_root: target.workspace_root(),
                issuing_generation: target.generation(), target_evidence_root: Some(target.evidence_root()), project_after_image: inputs.project_after_image,
                source_version: inputs.source_version, selected_before: inputs.selected_before,
                selected_after: inputs.selected_after, captures_before: inputs.captures_before,
                captures_after: inputs.captures_after, native_delta: inputs.native_delta,
                operation_key: target.operation_key(), request_identity: target.request_identity(),
                retired_profiles: target.retired_profiles().to_vec(), transaction: target.transaction().as_bytes(),
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
    base_semantic: &backend_engine::workspace::WorkspacePreflightRelationHandle<backend_engine::builtin::ProductSemanticPublicationRelation>,
    target_semantic: &backend_engine::workspace::WorkspacePreflightRelationHandle<backend_engine::builtin::ProductSemanticPublicationRelation>,
    base_capture: Option<&backend_engine::workspace::WorkspacePreflightRelationHandle<backend_engine::builtin::ProductSemanticCaptureRelation>>,
    target_capture: Option<&backend_engine::workspace::WorkspacePreflightRelationHandle<backend_engine::builtin::ProductSemanticCaptureRelation>>,
    budget: &mut usize,
    shutdown: Option<&crate::listener::ListenerShutdown>,
) -> Result<ReplayedEmptyNativeTsFrontiers, BuiltinModelError> {
    use backend_engine::builtin::{ProductSemanticPublicationRelation as Semantic, ProductSemanticCaptureRelation as Capture, ProductSemanticPublicationRecord, ProductSemanticCaptureOutcome, SemanticUnavailableReason};
    use backend_semantic::vocabulary::LanguageProfile;
    use backend_version::Relation;
    use std::collections::BTreeSet;

    let mut work = 0usize;
    let mut total = 0usize;
    // Stream canonical nonselected partitions; only at-most-two selected
    // keys per frontier escape each visitor, charged before cloning.
    macro_rules! scan {
        ($relation:expr, $ty:ty, $domain:expr, $check:expr, $visit:ident) => {{
            let mut hash = blake3::Hasher::new(); hash.update($domain); hash.update(&owner);
            let mut other = blake3::Hasher::new(); other.update(b"nudox.empty-ts.unchanged-frontier.v1\0"); other.update($domain); other.update(&owner);
            let mut count = 0u64; let mut other_count = 0u64; let mut selected = BTreeSet::new(); let mut selected_keys = Vec::new(); let mut after = None;
            if let Some(relation) = $relation {
                loop {
                    if shutdown.is_some_and(|s| s.is_requested()) { return Err(BuiltinModelError("empty TypeScript replay cancelled".into())); }
                    let next = backend_engine::builtin::$visit(relation, after.as_ref(), 1, budget, 64, &mut |key, row, remaining| {
                        if shutdown.is_some_and(|s| s.is_requested()) { return Err(backend_store::StoreError::Bounds); }
                        const ENCODE_SCRATCH: usize = 64 * 1024;
                        *remaining = remaining.checked_sub(ENCODE_SCRATCH).ok_or(backend_store::StoreError::Bounds)?;
                        let mut k = Vec::new(); let mut v = Vec::new(); <$ty>::encode_key(key, &mut k); <$ty>::encode_value(&row, &mut v);
                        total = total.checked_add(1).ok_or(backend_store::StoreError::Bounds)?;
                        work = work.checked_add(k.capacity()).and_then(|n| n.checked_add(v.capacity())).and_then(|n| n.checked_add(512)).ok_or(backend_store::StoreError::Bounds)?;
                        if total > backend_engine::application::MAX_MANIFEST_ENTRIES || work > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES { return Err(backend_store::StoreError::Bounds); }
                        if key.is_selected() && key.package_key().to_bytes() == owner && matches!(key.profile(), LanguageProfile::TypeScript(_)) {
                            count = count.checked_add(1).ok_or(backend_store::StoreError::Bounds)?;
                            if count > 2 || key.package().as_str().len() > 4096 || key.coordinate().as_str().len() > 4096 || k.len() > MAX_RETIREMENT_KEY_BYTES { return Err(backend_store::StoreError::Bounds); }
                            ($check)(&row).map_err(|_| backend_store::StoreError::Bounds)?;
                            hash.update(&(k.len() as u64).to_be_bytes()); hash.update(&k); hash.update(&(v.len() as u64).to_be_bytes()); hash.update(&v);
                            let retained = k.len().checked_mul(4).and_then(|n| n.checked_add(4096)).ok_or(backend_store::StoreError::Bounds)?;
                            *remaining = remaining.checked_sub(retained).ok_or(backend_store::StoreError::Bounds)?;
                            if !selected.insert(k.clone()) { return Err(backend_store::StoreError::Bounds); }
                            selected_keys.push(key.clone());
                        } else {
                            other_count = other_count.checked_add(1).ok_or(backend_store::StoreError::Bounds)?;
                            other.update(&(k.len() as u64).to_be_bytes()); other.update(&k); other.update(&(v.len() as u64).to_be_bytes()); other.update(&v);
                        }
                        drop((k, v)); *remaining = remaining.checked_add(ENCODE_SCRATCH).ok_or(backend_store::StoreError::Bounds)?;
                        Ok(())
                    }).map_err(|e| BuiltinModelError(format!("scan empty TypeScript replay frontier: {e}")))?;
                    if let Some(previous) = after.take() { backend_engine::builtin::release_semantic_cursor(previous, budget, 64).map_err(|e| BuiltinModelError(format!("release replay cursor: {e:?}")))?; }
                    match next { Some(next) => after = Some(next), None => break }
                }
            }
            hash.update(&count.to_be_bytes()); other.update(&other_count.to_be_bytes());
            (selected, selected_keys, *other.finalize().as_bytes(), *hash.finalize().as_bytes())
        }};
    }
    let semantic_check_before = |_: &ProductSemanticPublicationRecord| Ok::<(), BuiltinModelError>(());
    let semantic_check_after = |row: &ProductSemanticPublicationRecord| {
        if row != &ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::ProjectAuthority) { return Err(BuiltinModelError("empty TypeScript target semantic remains available".into())); }
        Ok(())
    };
    let capture_check_before = |row: &backend_engine::builtin::ProductSemanticCaptureRecord| {
        if matches!(row.outcome(), ProductSemanticCaptureOutcome::Pending { .. }) { return Err(BuiltinModelError("cannot replay in-flight TypeScript capture retirement".into())); } Ok(())
    };
    let capture_check_after = |_: &backend_engine::builtin::ProductSemanticCaptureRecord| Ok::<(), BuiltinModelError>(());
    let (before_keys, _, before_other, selected_before) = scan!(Some(base_semantic), Semantic, b"nudox.empty-ts.selected-frontier.v1\0", semantic_check_before, semantic_visit_preflighted);
    let (after_keys, _, after_other, selected_after) = scan!(Some(target_semantic), Semantic, b"nudox.empty-ts.selected-frontier.v1\0", semantic_check_after, semantic_visit_preflighted);
    if before_keys != after_keys || before_other != after_other { return Err(BuiltinModelError("empty TypeScript semantic frontier transition mismatch".into())); }
    drop((before_keys, after_keys, before_other, after_other));
    let (_, capture_keys, before_other, captures_before) = scan!(base_capture, Capture, b"nudox.empty-ts.capture-frontier.v1\0", capture_check_before, capture_visit_preflighted);
    let (after_keys, _, after_other, captures_after) = scan!(target_capture, Capture, b"nudox.empty-ts.capture-frontier.v1\0", capture_check_after, capture_visit_preflighted);
    if !after_keys.is_empty() || before_other != after_other { return Err(BuiltinModelError("empty TypeScript capture frontier transition mismatch".into())); }
    if shutdown.is_some_and(|s| s.is_requested()) { return Err(BuiltinModelError("empty TypeScript replay cancelled".into())); }
    Ok(ReplayedEmptyNativeTsFrontiers { capture_keys, selected_before, selected_after, captures_before, captures_after })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::{FileStore, OwnerLease};
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    fn statement(workspace: [u8; 32]) -> NativeTsRetirementReceiptStatement {
        NativeTsRetirementReceiptStatement {
            workspace,
            project: [2; 32],
            predecessor_root: [3; 32],
            predecessor_closure: [4; 32],
            predecessor_sequence: 2,
            predecessor_evidence_root: None,
            issuing_workspace_root: [13; 32],
            issuing_generation: 3,
            target_evidence_root: Some([6; 32]),
            project_after_image: [7; 32],
            source_version: [8; 32],
            selected_before: [9; 32],
            selected_after: [10; 32],
            captures_before: [11; 32],
            captures_after: [12; 32],
            native_delta: [14; 32],
            operation_key: None,
            request_identity: [15; 32],
            retired_profiles: vec![<[u8; 2]>::from(LanguageProfile::TypeScript(
                TypeScriptSource::TypeScript,
            ))],
            transaction: [16; 32],
        }
    }

    #[test]
    fn receipt_owner_mac_rejects_wrong_key_and_transition() {
        let temp = tempfile::tempdir().expect("private test root");
        let workspace = temp.path().join("workspace");
        let lease = OwnerLease::acquire(&workspace).expect("owner");
        let admission = lease.directory_admission();
        admission.admit("objects").expect("objects");
        let store = FileStore::open(workspace.join("objects"), 1024 * 1024).expect("store");
        let owner = OwnerReceiptContext::new([42; 32], workspace.clone());
        owner.bind(&admission, &store).expect("bind");
        let receipt = owner
            .with_bound(|key, workspace| {
                let statement = statement(workspace);
                let seal = seal_native_owner_statement(key, &statement).expect("test seal");
                Ok(NativeTsRetirementReceipt { statement, seal })
            })
            .expect("test receipt");
        assert!(receipt.verify(&owner, receipt.statement()).is_ok());

        let mut changed = receipt.statement.clone();
        changed.target_evidence_root = Some([17; 32]);
        assert!(receipt.verify(&owner, &changed).is_err());

        let mut changed_seal = receipt.clone();
        changed_seal.seal[0] ^= 1;
        assert!(changed_seal.verify(&owner, receipt.statement()).is_err());

        let wrong_owner = OwnerReceiptContext::new([43; 32], workspace);
        wrong_owner
            .bind(&admission, &store)
            .expect("wrong-key test context");
        assert!(receipt.verify(&wrong_owner, receipt.statement()).is_err());
    }

    #[test]
    fn raw_receipt_codec_is_strict_and_does_not_admit_a_seal() {
        let receipt = NativeTsRetirementReceipt {
            statement: statement([1; 32]),
            seal: [11; 32],
        };
        let bytes = serde_json::to_vec(&receipt).expect("serialize receipt");
        assert_eq!(
            NativeTsRetirementReceipt::decode(&bytes).expect("decode claim"),
            receipt
        );
        assert!(receipt.object().is_ok());

        let mut trailing = bytes;
        trailing.push(b' ');
        assert!(NativeTsRetirementReceipt::decode(&trailing).is_err());
        assert!(NativeTsRetirementReceipt::decode(&vec![b'x'; MAX_RECEIPT_BYTES + 1]).is_err());

        let missing_owner =
            OwnerReceiptContext::new([42; 32], std::path::PathBuf::from("unbound"));
        assert!(receipt.verify(&missing_owner, receipt.statement()).is_err());
        let mut altered = receipt.statement.clone();
        altered.native_delta[0] ^= 1;
        assert!(receipt.verify(&missing_owner, &altered).is_err());

        let mut zero_seal = receipt;
        zero_seal.seal = [0; 32];
        assert!(zero_seal.object().is_err());
    }
}
