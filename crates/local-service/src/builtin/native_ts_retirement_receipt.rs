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
#[derive(Clone)]
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
