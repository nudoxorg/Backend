//! Typed candidate, evaluation, and integration decision receipts.
//!
//! Candidate output identity and evaluator identity are intentionally
//! different schemas.  A candidate can therefore be re-evaluated when the
//! holdout or environment changes without spending another implementation
//! run, while a decision can advance an integration head only after an exact
//! compare-and-swap check.

use crate::ControlError;
use crate::ids::{
    CandidateReceiptId, CandidateReceiptSchema, CandidateTreeRoot, CandidateTreeSchema, CellId,
    CellSchema, ContractRoot, ContractSchema, EvaluationKeyId, EvaluationKeySchema, EvaluationRoot,
    EvaluationSchema, EvidenceDecisionReceiptId, EvidenceEvaluationReceiptId,
    EvidenceEvaluationReceiptSchema, EvidenceRoot, EvidenceSchema, FenceId, FenceSchema, Identity,
    InputRoot, InputSchema, IntegrationHead, IntegrationHeadSchema, ModelId, ModelSchema,
    OutputRoot, OutputSchema, RoleId, RoleSchema, ToolchainRoot, ToolchainSchema, append_identity,
    append_version,
};
use crate::record::Authority;

/// Maximum number of immutable references retained by one receipt.
pub const MAX_RECEIPT_REFS: usize = 1024;

/// Maximum canonical bytes retained by one candidate/evaluation/decision
/// receipt.  This keeps receipt admission independent of the underlying
/// filesystem envelope limit.
pub const MAX_TYPED_RECEIPT_BYTES: usize = 64 * 1024;

/// Candidate output identity, parent head, and controller-owned checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateReceipt {
    id: CandidateReceiptId,
    cell: CellId,
    candidate_tree: CandidateTreeRoot,
    parent_head: IntegrationHead,
    contract_roots: Box<[ContractRoot]>,
    toolchain_root: ToolchainRoot,
    role_contract: RoleId,
    model: ModelId,
    controller_checks: Box<[EvidenceRoot]>,
}

impl CandidateReceipt {
    /// Admits a controller candidate from its complete immutable references.
    ///
    /// Contract and check identities are sorted before hashing.  The
    /// resulting receipt is history independent and cannot be confused with
    /// either the candidate tree or a later evaluation receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] for oversized canonical inputs,
    /// [`ControlError::EvidenceBinding`] for missing or duplicate references.
    /// The canonical receipt is bounded by [`MAX_TYPED_RECEIPT_BYTES`].
    #[allow(
        clippy::too_many_arguments,
        reason = "a candidate receipt commits each independent contract input"
    )]
    pub fn new(
        cell: CellId,
        candidate_tree: CandidateTreeRoot,
        parent_head: IntegrationHead,
        contract_roots: impl IntoIterator<Item = ContractRoot>,
        toolchain_root: ToolchainRoot,
        role_contract: RoleId,
        model: ModelId,
        controller_checks: impl IntoIterator<Item = EvidenceRoot>,
    ) -> Result<Self, ControlError> {
        let contract_roots = canonical_refs(contract_roots)?;
        let controller_checks = canonical_refs(controller_checks)?;
        if controller_checks.is_empty() {
            return Err(ControlError::EvidenceBinding);
        }
        let bytes = candidate_bytes(
            cell,
            candidate_tree,
            parent_head,
            &contract_roots,
            toolchain_root,
            role_contract,
            model,
            &controller_checks,
        )?;
        let id = CandidateReceiptId::from_value(&bytes);
        Ok(Self {
            id,
            cell,
            candidate_tree,
            parent_head,
            contract_roots,
            toolchain_root,
            role_contract,
            model,
            controller_checks,
        })
    }

    /// Returns the immutable candidate receipt identity.
    #[must_use]
    pub const fn id(&self) -> CandidateReceiptId {
        self.id
    }

    /// Returns the work cell bound by the candidate.
    #[must_use]
    pub const fn cell(&self) -> CellId {
        self.cell
    }

    /// Returns the exact candidate tree identity.
    #[must_use]
    pub const fn candidate_tree(&self) -> CandidateTreeRoot {
        self.candidate_tree
    }

    /// Returns the integration head from which the candidate was built.
    #[must_use]
    pub const fn parent_head(&self) -> IntegrationHead {
        self.parent_head
    }

    /// Returns controller contract identities in canonical order.
    pub fn contract_roots(&self) -> impl Iterator<Item = ContractRoot> + '_ {
        self.contract_roots.iter().copied()
    }

    /// Returns the immutable toolchain root.
    #[must_use]
    pub const fn toolchain_root(&self) -> ToolchainRoot {
        self.toolchain_root
    }

    /// Returns the role contract identity.
    #[must_use]
    pub const fn role_contract(&self) -> RoleId {
        self.role_contract
    }

    /// Returns the model family identity.
    #[must_use]
    pub const fn model(&self) -> ModelId {
        self.model
    }

    /// Returns controller-owned check receipts.
    pub fn controller_checks(&self) -> impl Iterator<Item = EvidenceRoot> + '_ {
        self.controller_checks.iter().copied()
    }
}

/// Immutable identity of one evaluator assignment for a frozen candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvaluationKey {
    id: EvaluationKeyId,
    candidate_receipt: CandidateReceiptId,
    cell: CellId,
    candidate_tree: CandidateTreeRoot,
    evaluator_root: EvaluationRoot,
    holdout: EvidenceRoot,
    environment: InputRoot,
}

impl EvaluationKey {
    /// Creates an evaluation identity without changing the candidate run.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] if the canonical identity preimage is
    /// oversized.
    pub fn new(
        candidate: &CandidateReceipt,
        evaluator_root: EvaluationRoot,
        holdout: EvidenceRoot,
        environment: InputRoot,
    ) -> Result<Self, ControlError> {
        let candidate_receipt = candidate.id();
        let cell = candidate.cell();
        let candidate_tree = candidate.candidate_tree();
        let bytes =
            evaluation_key_bytes(cell, candidate_tree, evaluator_root, holdout, environment);
        let id = Identity::<EvaluationKeySchema>::from_bytes(bytes)?;
        Ok(Self {
            id: id.id(),
            candidate_receipt,
            cell,
            candidate_tree,
            evaluator_root,
            holdout,
            environment,
        })
    }

    /// Returns the immutable evaluator assignment identity.
    #[must_use]
    pub const fn id(self) -> EvaluationKeyId {
        self.id
    }

    /// Returns the candidate receipt being evaluated.
    #[must_use]
    pub const fn candidate_receipt(self) -> CandidateReceiptId {
        self.candidate_receipt
    }

    /// Returns the candidate cell.
    #[must_use]
    pub const fn cell(self) -> CellId {
        self.cell
    }

    /// Returns the candidate tree bound by this evaluation.
    #[must_use]
    pub const fn candidate_tree(self) -> CandidateTreeRoot {
        self.candidate_tree
    }

    /// Returns the evaluator implementation identity.
    #[must_use]
    pub const fn evaluator_root(self) -> EvaluationRoot {
        self.evaluator_root
    }

    /// Returns the committed holdout identity.
    #[must_use]
    pub const fn holdout(self) -> EvidenceRoot {
        self.holdout
    }

    /// Returns the evaluation environment identity.
    #[must_use]
    pub const fn environment(self) -> InputRoot {
        self.environment
    }
}

/// Whether an evaluator run passed, failed, or remains pending.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EvaluationOutcome {
    /// The evaluator and its required holdout checks passed.
    Verified,
    /// The candidate failed at least one evaluator check.
    Failed,
    /// The evaluation has been assigned but has no terminal result yet.
    Pending,
}

impl EvaluationOutcome {
    const fn tag(self) -> u8 {
        match self {
            Self::Verified => 0,
            Self::Failed => 1,
            Self::Pending => 2,
        }
    }
}

/// Reviewer/evaluator evidence bound to exactly one candidate assignment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationReceipt {
    id: EvidenceEvaluationReceiptId,
    evaluation_key: EvaluationKeyId,
    candidate_receipt: CandidateReceiptId,
    authority: Authority,
    fence: Identity<FenceSchema>,
    outcome: EvaluationOutcome,
    artifact_digests: Box<[EvidenceRoot]>,
    coverage: Box<[EvidenceRoot]>,
    result_root: Identity<OutputSchema>,
}

impl EvaluationReceipt {
    /// Mints evaluator evidence after checking candidate/evaluation binding.
    /// Controller and Sol authorities cannot impersonate an evaluator or
    /// reviewer receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::EvidenceAuthority`] for a non-evaluator
    /// authority, [`ControlError::EvidenceBinding`] for missing references,
    /// or a bounds/identity error for an oversized receipt.
    pub fn new(
        evaluation: EvaluationKey,
        authority: Authority,
        fence: Identity<FenceSchema>,
        outcome: EvaluationOutcome,
        artifact_digests: impl IntoIterator<Item = EvidenceRoot>,
        coverage: impl IntoIterator<Item = EvidenceRoot>,
        result_root: Identity<OutputSchema>,
    ) -> Result<Self, ControlError> {
        if !matches!(authority, Authority::Evaluator | Authority::Reviewer) {
            return Err(ControlError::EvidenceAuthority);
        }
        let artifact_digests = canonical_refs(artifact_digests)?;
        let coverage = canonical_refs(coverage)?;
        if artifact_digests.is_empty() || coverage.is_empty() {
            return Err(ControlError::EvidenceBinding);
        }
        let bytes = evaluation_receipt_bytes(
            evaluation,
            authority,
            &fence,
            outcome,
            &artifact_digests,
            &coverage,
            &result_root,
        )?;
        let id = EvidenceEvaluationReceiptId::from_value(&bytes);
        Ok(Self {
            id,
            evaluation_key: evaluation.id(),
            candidate_receipt: evaluation.candidate_receipt(),
            authority,
            fence,
            outcome,
            artifact_digests,
            coverage,
            result_root,
        })
    }

    /// Returns the immutable evaluation receipt identity.
    #[must_use]
    pub const fn id(&self) -> EvidenceEvaluationReceiptId {
        self.id
    }

    /// Returns the assignment identity.
    #[must_use]
    pub const fn evaluation_key(&self) -> EvaluationKeyId {
        self.evaluation_key
    }

    /// Returns the candidate receipt identity.
    #[must_use]
    pub const fn candidate_receipt(&self) -> CandidateReceiptId {
        self.candidate_receipt
    }

    /// Returns the authority that produced this evidence.
    #[must_use]
    pub const fn authority(&self) -> Authority {
        self.authority
    }

    /// Returns the exact attempt fence bound by this evidence.
    #[must_use]
    pub const fn fence(&self) -> FenceId {
        self.fence.id()
    }

    /// Returns the terminal evaluation outcome.
    #[must_use]
    pub const fn outcome(&self) -> EvaluationOutcome {
        self.outcome
    }

    /// Returns artifact evidence identities.
    pub fn artifact_digests(&self) -> impl Iterator<Item = EvidenceRoot> + '_ {
        self.artifact_digests.iter().copied()
    }

    /// Returns proven coverage identities.
    pub fn coverage(&self) -> impl Iterator<Item = EvidenceRoot> + '_ {
        self.coverage.iter().copied()
    }

    /// Returns the immutable evaluator result root.
    #[must_use]
    pub const fn result_root(&self) -> OutputRoot {
        self.result_root.id()
    }
}

/// Decision written by Sol after reviewing independent evaluation receipts.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Decision {
    /// The candidate may advance the protected integration head.
    Verified,
    /// The candidate remains available for diagnosis but cannot advance.
    Rejected,
    /// The candidate is retained outside the reusable set.
    Quarantined,
}

impl Decision {
    const fn tag(self) -> u8 {
        match self {
            Self::Verified => 0,
            Self::Rejected => 1,
            Self::Quarantined => 2,
        }
    }
}

/// Sol-owned decision and exact integration-head CAS precondition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionReceipt {
    id: EvidenceDecisionReceiptId,
    candidate_receipt: CandidateReceiptId,
    evaluation_receipts: Box<[EvidenceEvaluationReceiptId]>,
    expected_head: IntegrationHead,
    accepted_head: IntegrationHead,
    decision: Decision,
    authority: Authority,
    fence: Identity<FenceSchema>,
}

impl DecisionReceipt {
    /// Builds a Sol decision from receipts tied to one candidate.
    ///
    /// A verified decision requires every cited evaluation receipt to be
    /// verified.  Receipt identities are canonicalized and duplicates are
    /// rejected before the decision identity is minted.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::EvidenceAuthority`] for a non-Sol authority,
    /// [`ControlError::EvidenceBinding`] for mixed or incomplete evaluations,
    /// or a bounds/identity error for an oversized decision.
    pub fn new(
        candidate: &CandidateReceipt,
        evaluations: impl IntoIterator<Item = EvaluationReceipt>,
        expected_head: IntegrationHead,
        accepted_head: IntegrationHead,
        decision: Decision,
        authority: Authority,
        fence: Identity<FenceSchema>,
    ) -> Result<Self, ControlError> {
        if authority != Authority::Sol {
            return Err(ControlError::EvidenceAuthority);
        }
        let mut evaluations = evaluations.into_iter().collect::<Vec<_>>();
        if evaluations.is_empty() || evaluations.len() > MAX_RECEIPT_REFS {
            return Err(ControlError::Bounds);
        }
        if evaluations
            .iter()
            .any(|receipt| receipt.candidate_receipt() != candidate.id())
        {
            return Err(ControlError::EvidenceBinding);
        }
        if decision == Decision::Verified
            && evaluations
                .iter()
                .any(|receipt| receipt.outcome() != EvaluationOutcome::Verified)
        {
            return Err(ControlError::EvidenceBinding);
        }
        evaluations.sort_unstable_by_key(EvaluationReceipt::id);
        if evaluations
            .windows(2)
            .any(|window| window[0].id() == window[1].id())
        {
            return Err(ControlError::EvidenceBinding);
        }
        let evaluation_receipts = evaluations
            .iter()
            .map(EvaluationReceipt::id)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let bytes = decision_receipt_bytes(
            candidate.id(),
            &evaluation_receipts,
            expected_head,
            accepted_head,
            decision,
            authority,
            &fence,
        )?;
        let id = EvidenceDecisionReceiptId::from_value(&bytes);
        Ok(Self {
            id,
            candidate_receipt: candidate.id(),
            evaluation_receipts,
            expected_head,
            accepted_head,
            decision,
            authority,
            fence,
        })
    }

    /// Returns the immutable decision identity.
    #[must_use]
    pub const fn id(&self) -> EvidenceDecisionReceiptId {
        self.id
    }

    /// Returns the candidate being decided.
    #[must_use]
    pub const fn candidate_receipt(&self) -> CandidateReceiptId {
        self.candidate_receipt
    }

    /// Returns cited evaluation identities in canonical order.
    pub fn evaluation_receipts(&self) -> impl Iterator<Item = EvidenceEvaluationReceiptId> + '_ {
        self.evaluation_receipts.iter().copied()
    }

    /// Returns the exact CAS precondition head.
    #[must_use]
    pub const fn expected_head(&self) -> IntegrationHead {
        self.expected_head
    }

    /// Returns the head selected by a verified decision.
    #[must_use]
    pub const fn accepted_head(&self) -> IntegrationHead {
        self.accepted_head
    }

    /// Returns this decision's terminal verdict.
    #[must_use]
    pub const fn decision(&self) -> Decision {
        self.decision
    }

    /// Returns the authority that authored this decision.
    #[must_use]
    pub const fn authority(&self) -> Authority {
        self.authority
    }

    /// Applies the decision as an exact compare-and-swap against `current`.
    ///
    /// A rejected or quarantined decision never advances a head.  A verified
    /// decision fails closed when another accepted candidate has moved the
    /// integration head first.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidTransition`] for a non-verified
    /// decision or [`ControlError::StaleRoot`] when the selected head moved.
    pub fn compare_and_swap(
        &self,
        current: IntegrationHead,
    ) -> Result<IntegrationHead, ControlError> {
        if self.decision != Decision::Verified {
            return Err(ControlError::InvalidTransition);
        }
        if current != self.expected_head {
            return Err(ControlError::StaleRoot);
        }
        Ok(self.accepted_head)
    }
}

fn canonical_refs<T: Ord>(values: impl IntoIterator<Item = T>) -> Result<Box<[T]>, ControlError> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.len() > MAX_RECEIPT_REFS {
        return Err(ControlError::Bounds);
    }
    values.sort_unstable();
    if values.windows(2).any(|window| window[0] == window[1]) {
        return Err(ControlError::EvidenceBinding);
    }
    Ok(values.into_boxed_slice())
}

fn bounded(bytes: Vec<u8>) -> Result<Vec<u8>, ControlError> {
    if bytes.len() > MAX_TYPED_RECEIPT_BYTES {
        Err(ControlError::Bounds)
    } else {
        Ok(bytes)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the candidate identity covers every independent input"
)]
fn candidate_bytes(
    cell: CellId,
    candidate_tree: CandidateTreeRoot,
    parent_head: IntegrationHead,
    contract_roots: &[ContractRoot],
    toolchain_root: ToolchainRoot,
    role_contract: RoleId,
    model: ModelId,
    controller_checks: &[EvidenceRoot],
) -> Result<Vec<u8>, ControlError> {
    let mut bytes = Vec::new();
    bytes.push(1);
    append_version::<CellSchema>(&mut bytes, cell);
    append_version::<CandidateTreeSchema>(&mut bytes, candidate_tree);
    append_version::<IntegrationHeadSchema>(&mut bytes, parent_head);
    append_versions::<ContractSchema>(&mut bytes, contract_roots)?;
    append_version::<ToolchainSchema>(&mut bytes, toolchain_root);
    append_version::<RoleSchema>(&mut bytes, role_contract);
    append_version::<ModelSchema>(&mut bytes, model);
    append_versions::<EvidenceSchema>(&mut bytes, controller_checks)?;
    bounded(bytes)
}

fn evaluation_key_bytes(
    cell: CellId,
    candidate_tree: CandidateTreeRoot,
    evaluator_root: EvaluationRoot,
    holdout: EvidenceRoot,
    environment: InputRoot,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + 5 * (1 + 2 + 1 + backend_version::ID_BYTES));
    bytes.push(1);
    append_version::<CellSchema>(&mut bytes, cell);
    append_version::<CandidateTreeSchema>(&mut bytes, candidate_tree);
    append_version::<EvaluationSchema>(&mut bytes, evaluator_root);
    append_version::<EvidenceSchema>(&mut bytes, holdout);
    append_version::<InputSchema>(&mut bytes, environment);
    bytes
}

fn evaluation_receipt_bytes(
    evaluation: EvaluationKey,
    authority: Authority,
    fence: &Identity<FenceSchema>,
    outcome: EvaluationOutcome,
    artifacts: &[EvidenceRoot],
    coverage: &[EvidenceRoot],
    result_root: &Identity<OutputSchema>,
) -> Result<Vec<u8>, ControlError> {
    let mut bytes = Vec::new();
    bytes.push(1);
    append_version::<EvaluationKeySchema>(&mut bytes, evaluation.id());
    append_version::<CandidateReceiptSchema>(&mut bytes, evaluation.candidate_receipt());
    bytes.push(authority.tag());
    append_identity::<FenceSchema>(&mut bytes, fence)?;
    bytes.push(outcome.tag());
    append_versions::<EvidenceSchema>(&mut bytes, artifacts)?;
    append_versions::<EvidenceSchema>(&mut bytes, coverage)?;
    append_identity::<OutputSchema>(&mut bytes, result_root)?;
    bounded(bytes)
}

fn decision_receipt_bytes(
    candidate_receipt: CandidateReceiptId,
    evaluations: &[EvidenceEvaluationReceiptId],
    expected_head: IntegrationHead,
    accepted_head: IntegrationHead,
    decision: Decision,
    authority: Authority,
    fence: &Identity<FenceSchema>,
) -> Result<Vec<u8>, ControlError> {
    let mut bytes = Vec::new();
    bytes.push(1);
    append_version::<CandidateReceiptSchema>(&mut bytes, candidate_receipt);
    append_versions::<EvidenceEvaluationReceiptSchema>(&mut bytes, evaluations)?;
    append_version::<IntegrationHeadSchema>(&mut bytes, expected_head);
    append_version::<IntegrationHeadSchema>(&mut bytes, accepted_head);
    bytes.push(decision.tag());
    bytes.push(authority.tag());
    append_identity::<FenceSchema>(&mut bytes, fence)?;
    bounded(bytes)
}

fn append_versions<T: backend_version::Schema>(
    bytes: &mut Vec<u8>,
    values: &[backend_version::ObjectVersion<T>],
) -> Result<(), ControlError> {
    let count = u32::try_from(values.len()).map_err(|_| ControlError::Bounds)?;
    bytes.extend_from_slice(&count.to_be_bytes());
    for value in values {
        append_version::<T>(bytes, *value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::panic,
        reason = "test helpers turn invariant failures into focused diagnostics"
    )]

    use super::*;
    use backend_version::{ObjectVersion, Schema};

    use crate::ids::IdentityBytes;

    fn must<T>(result: Result<T, ControlError>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("evidence test failed: {error}"),
        }
    }

    fn id<T: Schema<Value = IdentityBytes>>(label: &str) -> ObjectVersion<T> {
        must(Identity::<T>::from_label(label)).id()
    }

    fn ids<T: Schema<Value = IdentityBytes>>(prefix: &str, count: usize) -> Vec<ObjectVersion<T>> {
        (0..count)
            .map(|index| id::<T>(&format!("{prefix}-{index}")))
            .collect()
    }

    fn candidate(
        contracts: Vec<ContractRoot>,
        checks: Vec<EvidenceRoot>,
    ) -> Result<CandidateReceipt, ControlError> {
        CandidateReceipt::new(
            id::<CellSchema>("cell"),
            id::<CandidateTreeSchema>("tree"),
            id::<IntegrationHeadSchema>("head"),
            contracts,
            id::<ToolchainSchema>("toolchain"),
            id::<RoleSchema>("implement"),
            id::<ModelSchema>("model"),
            checks,
        )
    }

    #[test]
    fn candidate_receipt_identity_is_canonical_and_round_trips() {
        let contracts = ids::<ContractSchema>("contract", 3);
        let checks = ids::<EvidenceSchema>("check", 3);
        let receipt = must(candidate(contracts.clone(), checks.clone()));

        let mut reversed_contracts = contracts;
        reversed_contracts.reverse();
        let mut reversed_checks = checks;
        reversed_checks.reverse();
        let reordered = must(candidate(reversed_contracts, reversed_checks));
        assert_eq!(
            receipt, reordered,
            "reference order must not change identity"
        );

        let rebuilt = must(CandidateReceipt::new(
            receipt.cell(),
            receipt.candidate_tree(),
            receipt.parent_head(),
            receipt.contract_roots(),
            receipt.toolchain_root(),
            receipt.role_contract(),
            receipt.model(),
            receipt.controller_checks(),
        ));
        assert_eq!(rebuilt.id(), receipt.id());
        assert!(
            receipt.contract_roots().collect::<Vec<_>>().is_sorted(),
            "contract roots must be exposed in canonical order"
        );

        let other = must(candidate(
            ids::<ContractSchema>("contract", 3),
            ids::<EvidenceSchema>("check", 2),
        ));
        assert_ne!(
            other.id(),
            receipt.id(),
            "every check is part of the identity"
        );
    }

    #[test]
    fn candidate_receipt_rejects_more_than_max_receipt_refs() {
        let admitted = candidate(
            ids::<ContractSchema>("contract", 1),
            ids::<EvidenceSchema>("check", MAX_RECEIPT_REFS),
        );
        assert!(
            admitted.is_ok(),
            "exactly MAX_RECEIPT_REFS must be admitted"
        );

        let rejected = candidate(
            ids::<ContractSchema>("contract", 1),
            ids::<EvidenceSchema>("check", MAX_RECEIPT_REFS + 1),
        );
        assert_eq!(rejected, Err(ControlError::Bounds));
    }

    #[test]
    fn candidate_receipt_rejects_canonical_bytes_above_the_typed_bound() {
        let contracts = ids::<ContractSchema>("contract", MAX_RECEIPT_REFS);
        let checks = ids::<EvidenceSchema>("check", MAX_RECEIPT_REFS);
        let preimage = candidate_bytes(
            id::<CellSchema>("cell"),
            id::<CandidateTreeSchema>("tree"),
            id::<IntegrationHeadSchema>("head"),
            &contracts,
            id::<ToolchainSchema>("toolchain"),
            id::<RoleSchema>("implement"),
            id::<ModelSchema>("model"),
            &checks,
        );
        assert_eq!(preimage, Err(ControlError::Bounds));
        assert_eq!(candidate(contracts, checks), Err(ControlError::Bounds));
    }
}
