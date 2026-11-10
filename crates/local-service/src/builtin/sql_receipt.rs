//! Bounded SQL receipt wire claims. Parsing and object construction do not
//! grant custody; verification requires the selected owner and transition.
use super::{BuiltinModelError, owner_receipt_context::OwnerReceiptContext, sql_admission::{ValidatedSqlDelta, PreparedSqlRelationUpdate, prepare_sql_relation_update}};
use backend_engine::builtin::SqlCheckedReceiptSchema;
use backend_library::{NativeOwnerStatement, seal_native_owner_statement, verify_native_owner_statement};
use backend_store::TypedObject;
use backend_version::ObjectKey;
use serde::{Deserialize, Serialize};

const MAX_RECEIPT_BYTES: usize = 8192;

/// Transition fields must be independently rederived from selected state.
/// `checked_capture` is an owner-attested commitment; cold replay cannot
/// reconstruct a worker response from a digest. The containing closure is
/// intentionally excluded to avoid a hash cycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SqlReceiptStatement {
    pub(super) workspace: [u8; 32],
    pub(super) project: [u8; 32],
    pub(super) predecessor_root: [u8; 32],
    pub(super) predecessor_closure: [u8; 32],
    pub(super) predecessor_sequence: u64,
    pub(super) predecessor_sql_root: Option<[u8; 32]>,
    /// Issuing workspace root/generation identify a later authenticated
    /// PUBLISHED journal descriptor. They are not the containing closure.
    pub(super) issuing_workspace_root: [u8; 32],
    pub(super) issuing_generation: u64,
    pub(super) target_sql_root: [u8; 32],
    pub(super) project_after_image: [u8; 32],
    pub(super) changes: [u8; 32],
    pub(super) checked_capture: [u8; 32],
    pub(super) project_rows: [u8; 32],
    pub(super) transaction: [u8; 32],
}
impl NativeOwnerStatement for SqlReceiptStatement {
    const DOMAIN: &'static [u8] = b"nudox.sql.checked-capture-receipt.v1\0";
}
impl SqlReceiptStatement {
    fn valid(&self) -> bool {
        self.workspace != [0; 32] && self.project != [0; 32]
            && self.predecessor_root != [0; 32] && self.predecessor_closure != [0; 32]
            && self.predecessor_sequence != u64::MAX
            && self.predecessor_sql_root != Some([0; 32]) && self.issuing_workspace_root != [0; 32]
            && self.predecessor_sequence.checked_add(1) == Some(self.issuing_generation) && self.target_sql_root != [0; 32]
            && self.project_after_image != [0; 32] && self.changes != [0; 32]
            && self.checked_capture != [0; 32] && self.project_rows != [0; 32] && self.transaction != [0; 32]
    }
}
/// Deserialized values remain untrusted claims, including their seal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SqlReceipt {
    statement: SqlReceiptStatement,
    seal: [u8; 32],
}
/// Issuer-only capability; decoding a receipt never constructs this type.
pub(super) struct IssuedSqlReceipt { receipt: SqlReceipt, cancellation: std::sync::Arc<std::sync::atomic::AtomicBool> }
impl IssuedSqlReceipt {
    pub(super) fn project(&self) -> [u8; 32] { self.receipt.statement.project }
    pub(super) fn matches_basis(&self, basis: super::sql_admission::SqlAnalysisBasis) -> bool {
        let statement = &self.receipt.statement;
        statement.predecessor_root == basis.workspace_root && statement.predecessor_closure == basis.closure
            && statement.predecessor_sequence == basis.sequence && statement.predecessor_sql_root == basis.analysis_root
    }
    pub(super) fn check_cancelled(&self) -> Result<(), BuiltinModelError> {
        if self.cancellation.load(std::sync::atomic::Ordering::Acquire) { return Err(BuiltinModelError("SQL receipt cancelled".into())); }
        Ok(())
    }
    pub(super) fn object(&self) -> Result<TypedObject, BuiltinModelError> { self.check_cancelled()?; self.receipt.object() }
}
impl SqlReceipt {
    /// Mint only from an exact private validated live delta and the actual
    /// freshly prepared SQL target. The resulting object is still unpublished;
    /// the owner must revalidate at grant and select it in the same closure.
    pub(super) fn issue(owner: &OwnerReceiptContext, validated: &ValidatedSqlDelta<'_>, transaction: backend_engine::TransactionId, issuing_workspace_root: [u8; 32], issuing_generation: u64) -> Result<(PreparedSqlRelationUpdate, IssuedSqlReceipt), BuiltinModelError> {
        let inputs = validated.receipt_inputs().map_err(BuiltinModelError)?;
        let update = prepare_sql_relation_update(validated).map_err(BuiltinModelError)?;
        let target: [u8; 32] = update.pointer.as_ref().ok_or_else(|| BuiltinModelError("SQL receipt requires changed target".into()))?.bytes().try_into().map_err(|_| BuiltinModelError("invalid SQL target root".into()))?;
        let receipt = owner.with_bound(|key, workspace| {
            let statement = SqlReceiptStatement {
                workspace, project: inputs.project, predecessor_root: inputs.basis.workspace_root,
                predecessor_closure: inputs.basis.closure, predecessor_sequence: inputs.basis.sequence,
                predecessor_sql_root: inputs.basis.analysis_root, issuing_workspace_root, issuing_generation, target_sql_root: target,
                project_after_image: inputs.project_after_image, changes: inputs.changes,
                checked_capture: inputs.checked_capture, project_rows: inputs.project_rows, transaction: transaction.as_bytes(),
            };
            if !statement.valid() { return Err(BuiltinModelError("invalid SQL receipt statement".into())); }
            let seal = seal_native_owner_statement(key, &statement).map_err(|_| BuiltinModelError("SQL receipt sealing failed".into()))?;
            Ok(Self { statement, seal })
        })?;
        if validated.cancellation().load(std::sync::atomic::Ordering::Acquire) { return Err(BuiltinModelError("SQL receipt cancelled".into())); }
        Ok((update, IssuedSqlReceipt { receipt, cancellation: std::sync::Arc::clone(validated.cancellation()) }))
    }
    /// Read-only unverified claim; all fields remain untrusted until admission.
    pub(super) fn statement(&self) -> &SqlReceiptStatement { &self.statement }
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, BuiltinModelError> {
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
            return Err(BuiltinModelError("invalid SQL receipt length".into()));
        }
        let receipt: Self = serde_json::from_slice(bytes).map_err(|_| BuiltinModelError("invalid SQL receipt".into()))?;
        if !receipt.statement.valid() || receipt.seal == [0; 32]
            || serde_json::to_vec(&receipt).map_err(|_| BuiltinModelError("invalid SQL receipt".into()))? != bytes {
            return Err(BuiltinModelError("noncanonical SQL receipt".into()));
        }
        Ok(receipt)
    }
    /// Produces an immutable object claim; its presence alone is not proof.
    pub(super) fn object(&self) -> Result<TypedObject, BuiltinModelError> {
        if !self.statement.valid() || self.seal == [0; 32] { return Err(BuiltinModelError("invalid SQL receipt".into())); }
        let bytes = serde_json::to_vec(self).map_err(|_| BuiltinModelError("invalid SQL receipt".into()))?;
        if bytes.len() > MAX_RECEIPT_BYTES { return Err(BuiltinModelError("SQL receipt exceeds bound".into())); }
        let key = ObjectKey::<SqlCheckedReceiptSchema>::from_value(&self.statement.project);
        Ok(TypedObject::from_value(&key, &bytes))
    }
    /// Checks a MAC and an exact caller-supplied expected statement only. The
    /// caller must admit this object's key/version/ID in the selected closure
    /// and independently derive its transition fields; checked_capture is an
    /// attested commitment, not a reconstructed worker response.
    pub(super) fn verify(&self, owner: &OwnerReceiptContext, expected: &SqlReceiptStatement) -> Result<(), BuiltinModelError> {
        if !self.statement.valid() || &self.statement != expected { return Err(BuiltinModelError("SQL receipt claim mismatch".into())); }
        owner.with_bound(|key, workspace| {
            if self.statement.workspace != workspace { return Err(BuiltinModelError("SQL receipt workspace mismatch".into())); }
            verify_native_owner_statement(key, &self.statement, &self.seal)
                .map_err(|_| BuiltinModelError("SQL receipt seal invalid".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_owner_mac_rejects_wrong_key_and_transition() {
        let temp = tempfile::tempdir().expect("private test root");
        let workspace = temp.path().join("workspace");
        let lease = backend_engine::OwnerLease::acquire(&workspace).expect("owner");
        let admission = lease.directory_admission();
        admission.admit("objects").expect("objects");
        let store = backend_engine::FileStore::open(workspace.join("objects"), 1024 * 1024).expect("store");
        let owner = OwnerReceiptContext::new([42; 32], workspace.clone());
        owner.bind(&admission, &store).expect("bind");
        let receipt = owner.with_bound(|key, workspace| {
            let statement = SqlReceiptStatement { workspace, project: [2; 32], predecessor_root: [3; 32],
                predecessor_closure: [4; 32], predecessor_sequence: 2, predecessor_sql_root: None,
                issuing_workspace_root: [13; 32], issuing_generation: 3, target_sql_root: [6; 32], project_after_image: [7; 32], changes: [8; 32],
                checked_capture: [9; 32], project_rows: [12; 32], transaction: [10; 32] };
            let seal = seal_native_owner_statement(key, &statement).expect("test seal");
            Ok(SqlReceipt { statement, seal })
        }).expect("test receipt");
        assert!(receipt.verify(&owner, receipt.statement()).is_ok());
        let mut changed = receipt.statement.clone(); changed.target_sql_root[0] ^= 1;
        assert!(receipt.verify(&owner, &changed).is_err());
        let wrong_owner = OwnerReceiptContext::new([43; 32], workspace);
        wrong_owner.bind(&admission, &store).expect("wrong-key test context");
        assert!(receipt.verify(&wrong_owner, receipt.statement()).is_err());
    }
    #[test]
    fn raw_receipt_codec_is_strict_and_does_not_admit_a_seal() {
        let claim = SqlReceiptStatement {
            workspace: [1; 32], project: [2; 32], predecessor_root: [3; 32],
            predecessor_closure: [4; 32], predecessor_sequence: 2,
            predecessor_sql_root: Some([5; 32]), issuing_workspace_root: [13; 32], issuing_generation: 3, target_sql_root: [6; 32],
            project_after_image: [7; 32], changes: [8; 32], checked_capture: [9; 32],
            transaction: [10; 32], project_rows: [12; 32],
        };
        let receipt = SqlReceipt { statement: claim, seal: [11; 32] };
        let bytes = serde_json::to_vec(&receipt).expect("serialize receipt");
        assert_eq!(SqlReceipt::decode(&bytes).expect("decode claim"), receipt);
        assert!(receipt.object().is_ok());
        let mut trailing = bytes.clone(); trailing.push(b' ');
        assert!(SqlReceipt::decode(&trailing).is_err());
        assert!(SqlReceipt::decode(&vec![b'x'; MAX_RECEIPT_BYTES + 1]).is_err());
        let missing_owner = OwnerReceiptContext::new([42; 32], std::path::PathBuf::from("unbound"));
        assert!(receipt.verify(&missing_owner, &receipt.statement).is_err());
        let mut altered = receipt.statement.clone(); altered.target_sql_root[0] ^= 1;
        assert!(receipt.verify(&missing_owner, &altered).is_err());
    }
}

/// Capability for an exact selected object whose receipt MAC matched the
/// caller's expected statement. The caller remains responsible for deriving
/// expected fields from the selected transition and current project rows.
pub(super) struct VerifiedSqlReceipt {
    receipt: SqlReceipt,
    owner_epoch: u64,
    owner_fence: [u8; 32],
    workspace_root: [u8; 32],
    workspace_sequence: u64,
    closure: [u8; 32],
}
impl VerifiedSqlReceipt {
    pub(super) fn statement(&self) -> &SqlReceiptStatement { self.receipt.statement() }
    pub(super) fn object(&self) -> Result<TypedObject, BuiltinModelError> { self.receipt.object() }
    /// Exact source snapshot binding; source_capture is the new staged
    /// request and therefore deliberately not part of the predecessor check.
    pub(super) fn matches_stage(&self, basis: super::staged_intent::StageBasis) -> bool {
        self.owner_epoch == basis.owner_epoch && self.owner_fence == basis.owner_fence
            && self.workspace_root == basis.workspace_root && self.workspace_sequence == basis.workspace_sequence
            && self.closure == basis.closure_id
    }
}

/// Authenticate the supplied snapshot's index pointer/root, indexed object
/// and owner MAC. The caller must bind this snapshot to the active owner and
/// derive `expected` from that same selected transition. Missing data is
/// unavailable, never a valid empty result.
pub(super) fn selected_receipt(
    snapshot: &backend_engine::WorkspaceSnapshot,
    store: &backend_engine::FileStore,
    owner: &OwnerReceiptContext,
    expected: &SqlReceiptStatement,
) -> Result<VerifiedSqlReceipt, BuiltinModelError> {
    selected_receipt_with(snapshot, store, owner, expected.project, |claim| {
        if claim != expected { return Err(BuiltinModelError("SQL receipt claim mismatch".into())); }
        Ok(())
    })
}

/// Retain only from an already owner-admitted predecessor after the caller
/// proves its complete selected project row partition and computes its digest.
/// Previous transition admission, not this helper, establishes that the
/// original receipt was selected rather than an abandoned signed attempt.
pub(super) fn selected_retained_receipt(
    snapshot: &backend_engine::WorkspaceSnapshot,
    store: &backend_engine::FileStore,
    owner: &OwnerReceiptContext,
    project: [u8; 32],
    project_rows: [u8; 32],
) -> Result<VerifiedSqlReceipt, BuiltinModelError> {
    selected_receipt_with(snapshot, store, owner, project, |claim| {
        if claim.project != project || claim.project_rows != project_rows {
            return Err(BuiltinModelError("retained SQL receipt project rows changed".into()));
        }
        Ok(())
    })
}

fn selected_receipt_with(
    snapshot: &backend_engine::WorkspaceSnapshot,
    store: &backend_engine::FileStore,
    owner: &OwnerReceiptContext,
    project: [u8; 32],
    check_claim: impl FnOnce(&SqlReceiptStatement) -> Result<(), BuiltinModelError>,
) -> Result<VerifiedSqlReceipt, BuiltinModelError> {
    use backend_engine::builtin::{SqlReceiptIndexRootSchema, sql_receipt_index_relation, sql_receipt_index_root_pointer_key};
    use backend_version::{Schema, SchemaIdentity};
    owner.with_bound(|_, _| {
        owner.verify_store(store)?;
        let relation = sql_receipt_index_relation(snapshot)
            .map_err(|_| BuiltinModelError("SQL receipt index unavailable".into()))?
            .ok_or_else(|| BuiltinModelError("SQL receipt index missing".into()))?;
        let schema = SchemaIdentity::new(SqlReceiptIndexRootSchema::DOMAIN, SqlReceiptIndexRootSchema::TYPE, SqlReceiptIndexRootSchema::VERSION);
        let key = sql_receipt_index_root_pointer_key();
        let pointer = snapshot.closure().control_manifest().objects().iter()
            .find(|object| object.schema() == schema && object.key() == &key)
            .ok_or_else(|| BuiltinModelError("SQL receipt index pointer missing".into()))?;
        let root = relation.root_object().map_err(|_| BuiltinModelError("SQL receipt index root unavailable".into()))?;
        if !snapshot.closure().contains(pointer.id()).map_err(|_| BuiltinModelError("SQL receipt closure unavailable".into()))?
            || !snapshot.closure().contains(root.id()).map_err(|_| BuiltinModelError("SQL receipt closure unavailable".into()))? {
            return Err(BuiltinModelError("SQL receipt index outside selected closure".into()));
        }
        let row = relation.lookup(&project).map_err(|_| BuiltinModelError("SQL receipt index lookup failed".into()))?
            .ok_or_else(|| BuiltinModelError("SQL project receipt missing".into()))?;
        let key = ObjectKey::<SqlCheckedReceiptSchema>::from_value(&project);
        // Verify schema/key/version and the tight receipt payload cap on the
        // borrowed object view before making our own payload copy.
        let (object_id, bytes) = store.with_verified_object_claim(
            backend_store::UntrustedObjectId::from_bytes(row.object_id), |view| {
                if view.schema() != SchemaIdentity::new(SqlCheckedReceiptSchema::DOMAIN, SqlCheckedReceiptSchema::TYPE, SqlCheckedReceiptSchema::VERSION)
                    || view.key() != key.as_bytes() || view.version() != &row.object_version
                    || view.bytes().len() > MAX_RECEIPT_BYTES {
                    return Err(backend_store::StoreError::Corrupt);
                }
                Ok((view.id(), view.bytes().to_vec()))
            }).map_err(|_| BuiltinModelError("SQL receipt object unavailable".into()))?;
        if !snapshot.closure().contains(object_id).map_err(|_| BuiltinModelError("SQL receipt closure unavailable".into()))? {
            return Err(BuiltinModelError("SQL receipt object not selected".into()));
        }
        let receipt = SqlReceipt::decode(&bytes)?;
        if receipt.statement.project != project { return Err(BuiltinModelError("SQL receipt project mismatch".into())); }
        check_claim(receipt.statement())?;
        receipt.verify(owner, receipt.statement())?;
        owner.verify_store(store)?;
        Ok(VerifiedSqlReceipt { receipt, owner_epoch: snapshot.owner_epoch(),
            owner_fence: *snapshot.closure().binding().proof(), workspace_root: *snapshot.root().as_bytes(),
            workspace_sequence: snapshot.sequence(), closure: *snapshot.closure().membership_id().as_bytes() })
    })
}

/// Storage-codec fixture only. This does not construct a verified or issued
/// receipt capability and its arbitrary seal cannot authenticate an owner.
#[cfg(test)]
pub(super) fn unverified_test_receipt_object() -> TypedObject {
    SqlReceipt { statement: SqlReceiptStatement {
        workspace: [1; 32], project: [2; 32], predecessor_root: [3; 32], predecessor_closure: [4; 32],
        predecessor_sequence: 1, predecessor_sql_root: None, issuing_workspace_root: [13; 32], issuing_generation: 2, target_sql_root: [5; 32],
        project_after_image: [6; 32], changes: [7; 32], checked_capture: [8; 32],
        project_rows: [9; 32], transaction: [10; 32],
    }, seal: [11; 32] }.object().expect("bounded unverified test claim")
}
