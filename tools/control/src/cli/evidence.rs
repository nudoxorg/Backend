//! Executable adapters for typed candidate, evaluation, and decision receipts.

use backend_control::evidence::{
    CandidateReceipt, Decision, DecisionReceipt, EvaluationKey, EvaluationOutcome,
    EvaluationReceipt,
};
use backend_control::ids::{
    CandidateTreeSchema, CellSchema, ContractSchema, EvaluationSchema, EvidenceSchema, FenceSchema,
    Identity, IdentityBytes, InputSchema, IntegrationHeadSchema, ModelSchema, OutputSchema,
    RoleSchema, ToolchainSchema, hex_encode,
};
use backend_control::{Authority, ControlError};
use backend_version::{ObjectVersion, Schema};
use serde::Deserialize;
use serde_json::json;

use super::{input, output};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateRequest {
    cell: String,
    candidate_tree: String,
    parent_head: String,
    #[serde(default)]
    contract_roots: Vec<String>,
    toolchain_root: String,
    role_contract: String,
    #[serde(alias = "model_class")]
    model: String,
    #[serde(default)]
    controller_checks: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationRequest {
    candidate: CandidateRequest,
    evaluator_root: String,
    holdout: String,
    environment: String,
    #[serde(default = "default_evaluation_authority")]
    authority: String,
    fence: String,
    outcome: String,
    #[serde(default)]
    artifact_digests: Vec<String>,
    #[serde(default)]
    coverage: Vec<String>,
    result_root: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest {
    candidate: CandidateRequest,
    evaluations: Vec<EvaluationInput>,
    expected_head: String,
    accepted_head: String,
    current_head: String,
    decision: String,
    #[serde(default = "default_decision_authority")]
    authority: String,
    fence: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationInput {
    evaluator_root: String,
    holdout: String,
    environment: String,
    #[serde(default = "default_evaluation_authority")]
    authority: String,
    fence: String,
    outcome: String,
    #[serde(default)]
    artifact_digests: Vec<String>,
    #[serde(default)]
    coverage: Vec<String>,
    result_root: String,
}

fn default_evaluation_authority() -> String {
    "evaluator".to_owned()
}

fn default_decision_authority() -> String {
    "sol".to_owned()
}

pub(crate) fn candidate(arguments: &[String]) -> Result<(), ControlError> {
    let request = serde_json::from_str::<CandidateRequest>(&input::request_json(arguments)?)
        .map_err(|error| ControlError::Wire(error.to_string()))?;
    let candidate = admit_candidate(&request)?;
    output::emit(&json!({
        "candidate_receipt": hex_encode(candidate.id().as_bytes()),
        "candidate_tree": hex_encode(candidate.candidate_tree().as_bytes()),
        "parent_head": hex_encode(candidate.parent_head().as_bytes()),
        "cell": hex_encode(candidate.cell().as_bytes()),
    }))
}

pub(crate) fn evaluation(arguments: &[String]) -> Result<(), ControlError> {
    let request = serde_json::from_str::<EvaluationRequest>(&input::request_json(arguments)?)
        .map_err(|error| ControlError::Wire(error.to_string()))?;
    let candidate = admit_candidate(&request.candidate)?;
    let evaluation = request.into_input();
    let receipt = admit_evaluation(&candidate, &evaluation)?;
    output::emit(&json!({
        "evaluation_key": hex_encode(receipt.evaluation_key().as_bytes()),
        "candidate_receipt": hex_encode(receipt.candidate_receipt().as_bytes()),
        "evaluation_receipt": hex_encode(receipt.id().as_bytes()),
        "authority": format!("{:?}", receipt.authority()).to_ascii_lowercase(),
        "outcome": format!("{:?}", receipt.outcome()).to_ascii_lowercase(),
    }))
}

pub(crate) fn decision(arguments: &[String]) -> Result<(), ControlError> {
    let request = serde_json::from_str::<DecisionRequest>(&input::request_json(arguments)?)
        .map_err(|error| ControlError::Wire(error.to_string()))?;
    let candidate = admit_candidate(&request.candidate)?;
    let evaluations = request
        .evaluations
        .into_iter()
        .map(|evaluation| admit_evaluation(&candidate, &evaluation))
        .collect::<Result<Vec<_>, _>>()?;
    let expected_head = label_id::<IntegrationHeadSchema>(&request.expected_head)?;
    let accepted_head = label_id::<IntegrationHeadSchema>(&request.accepted_head)?;
    let current_head = label_id::<IntegrationHeadSchema>(&request.current_head)?;
    let decision = parse_decision(&request.decision)?;
    let authority = parse_authority(&request.authority)?;
    let fence = Identity::<FenceSchema>::from_label(&request.fence)?;
    let receipt = DecisionReceipt::new(
        &candidate,
        evaluations,
        expected_head,
        accepted_head,
        decision,
        authority,
        fence,
    )?;
    let selected = receipt.compare_and_swap(current_head)?;
    output::emit(&json!({
        "decision_receipt": hex_encode(receipt.id().as_bytes()),
        "candidate_receipt": hex_encode(receipt.candidate_receipt().as_bytes()),
        "decision": format!("{:?}", receipt.decision()).to_ascii_lowercase(),
        "expected_head": hex_encode(receipt.expected_head().as_bytes()),
        "accepted_head": hex_encode(selected.as_bytes()),
    }))
}

impl EvaluationRequest {
    fn into_input(self) -> EvaluationInput {
        EvaluationInput {
            evaluator_root: self.evaluator_root,
            holdout: self.holdout,
            environment: self.environment,
            authority: self.authority,
            fence: self.fence,
            outcome: self.outcome,
            artifact_digests: self.artifact_digests,
            coverage: self.coverage,
            result_root: self.result_root,
        }
    }
}

fn admit_candidate(request: &CandidateRequest) -> Result<CandidateReceipt, ControlError> {
    let contract_roots = labels::<ContractSchema>(&request.contract_roots)?;
    let controller_checks = labels::<EvidenceSchema>(&request.controller_checks)?;
    CandidateReceipt::new(
        label_id::<CellSchema>(&request.cell)?,
        label_id::<CandidateTreeSchema>(&request.candidate_tree)?,
        label_id::<IntegrationHeadSchema>(&request.parent_head)?,
        contract_roots,
        label_id::<ToolchainSchema>(&request.toolchain_root)?,
        label_id::<RoleSchema>(&request.role_contract)?,
        label_id::<ModelSchema>(&request.model)?,
        controller_checks,
    )
}

fn admit_evaluation(
    candidate: &CandidateReceipt,
    request: &EvaluationInput,
) -> Result<EvaluationReceipt, ControlError> {
    let evaluation = EvaluationKey::new(
        candidate,
        label_id::<EvaluationSchema>(&request.evaluator_root)?,
        label_id::<EvidenceSchema>(&request.holdout)?,
        label_id::<InputSchema>(&request.environment)?,
    )?;
    EvaluationReceipt::new(
        evaluation,
        parse_authority(&request.authority)?,
        Identity::<FenceSchema>::from_label(&request.fence)?,
        parse_outcome(&request.outcome)?,
        labels::<EvidenceSchema>(&request.artifact_digests)?,
        labels::<EvidenceSchema>(&request.coverage)?,
        Identity::<OutputSchema>::from_label(&request.result_root)?,
    )
}

fn labels<T>(values: &[String]) -> Result<Vec<ObjectVersion<T>>, ControlError>
where
    T: Schema<Value = IdentityBytes>,
{
    values.iter().map(|value| label_id::<T>(value)).collect()
}

fn label_id<T>(value: &str) -> Result<ObjectVersion<T>, ControlError>
where
    T: Schema<Value = IdentityBytes>,
{
    Identity::<T>::from_label(value).map(|identity| identity.id())
}

fn parse_authority(value: &str) -> Result<Authority, ControlError> {
    match value {
        "evaluator" => Ok(Authority::Evaluator),
        "reviewer" => Ok(Authority::Reviewer),
        "sol" => Ok(Authority::Sol),
        "controller" => Ok(Authority::Controller),
        _ => Err(ControlError::Wire("invalid evidence authority".to_owned())),
    }
}

fn parse_outcome(value: &str) -> Result<EvaluationOutcome, ControlError> {
    match value {
        "verified" => Ok(EvaluationOutcome::Verified),
        "failed" => Ok(EvaluationOutcome::Failed),
        "pending" => Ok(EvaluationOutcome::Pending),
        _ => Err(ControlError::Wire("invalid evaluation outcome".to_owned())),
    }
}

fn parse_decision(value: &str) -> Result<Decision, ControlError> {
    match value {
        "verified" => Ok(Decision::Verified),
        "rejected" => Ok(Decision::Rejected),
        "quarantined" => Ok(Decision::Quarantined),
        _ => Err(ControlError::Wire(
            "invalid integration decision".to_owned(),
        )),
    }
}
