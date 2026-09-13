//! Individual bounded lifecycle command adapters.

use backend_control::ids::{
    AgentWorkKey, EvidenceSchema, Identity, OutputSchema, OwnerSchema, hex_encode,
};
use backend_control::{
    ControlError, CustodyVerdict, DurableControlPlane, MAX_STATUS_PAGE, PlanResult, StageResult,
    WireWorkSpec,
};
use serde_json::json;

use super::{input, output};

pub(crate) fn help() {
    println!(
        "backend-control <init|status|key|plan|apply|admit|renew|candidate|evaluation|review|decision|fail|cancel|recover|invalidate> [options]"
    );
}

pub(crate) fn init(arguments: &[String]) -> Result<(), ControlError> {
    let plane = input::open(arguments)?;
    output::emit(&json!({
        "root": hex_encode(plane.root().as_bytes()),
        "initialized": true
    }))
}

pub(crate) fn status(arguments: &[String]) -> Result<(), ControlError> {
    let plane = input::open(arguments)?;
    let limit = input::optional(arguments, "--limit")
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| ControlError::Wire("invalid status page size".to_owned()))?
        .unwrap_or(MAX_STATUS_PAGE);
    if limit == 0 || limit > MAX_STATUS_PAGE {
        return Err(ControlError::Bounds);
    }
    let after = input::optional(arguments, "--after")
        .map(|value| input::admit_key(&plane, value))
        .transpose()?;
    let page = plane.status_page(after, limit)?;
    let records = page
        .records()
        .iter()
        .map(|record| {
            json!({
                "work_key": hex_encode(record.key().as_bytes()),
                "status": format!("{:?}", record.status()).to_ascii_lowercase(),
                "attempts": record.attempts(),
                "waiters": record.waiter_count(),
                "candidate": record.receipt().map(|receipt| hex_encode(receipt.id().as_bytes())),
                "evaluation": record.evaluation().map(|receipt| hex_encode(receipt.id().as_bytes())),
                "review": record.review().map(|receipt| hex_encode(receipt.id().as_bytes())),
                "decision": record.decision().map(|receipt| hex_encode(receipt.id().as_bytes())),
            })
        })
        .collect::<Vec<_>>();
    output::emit(&json!({
        "root": hex_encode(plane.root().as_bytes()),
        "records": records,
        "next": page.next().map(|key| hex_encode(key.as_bytes()))
    }))
}

pub(crate) fn key(arguments: &[String]) -> Result<(), ControlError> {
    let spec = WireWorkSpec::from_json(&input::request_json(arguments)?)?;
    output::emit(&json!({"work_key": hex_encode(spec.key().as_bytes())}))
}

pub(crate) fn plan_or_apply(command: &str, arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let spec = WireWorkSpec::from_json(&input::request_json(arguments)?)?;
    let key = spec.key();
    let result = plane.plan(spec)?;
    let result = match result {
        PlanResult::Admitted(commit) if command == "apply" => {
            json!({"kind": "applied", "commit": output::commit_json(&commit)})
        }
        PlanResult::Admitted(commit) => output::plan_result(PlanResult::Admitted(commit)),
        other => output::plan_result(other),
    };
    output::emit(&json!({
        "work_key": hex_encode(key.as_bytes()),
        "result": result
    }))
}

pub(crate) fn admit(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let owner = input::owner(input::optional(arguments, "--owner").unwrap_or("cli"))?;
    let (admission, commit) = plane.admit(key, owner, input::now()?)?;
    output::emit(&json!({
        "admission": output::admission(admission),
        "commit": commit.as_ref().map(output::commit_json)
    }))
}

pub(crate) fn renew(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let fence = input::fence(arguments)?;
    let lease = plane.lease_for(key, &fence)?;
    let (renewed, commit) = plane.renew(&lease, input::now()?)?;
    output::emit(&json!({
        "lease": output::lease(renewed.fence()),
        "commit": output::commit_json(&commit)
    }))
}

pub(crate) fn freeze(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let fence = input::fence(arguments)?;
    let lease = plane.lease_for(key, &fence)?;
    let output_root = Identity::<OutputSchema>::from_hex(input::required(arguments, "--output")?)?;
    let evidence = Identity::<EvidenceSchema>::from_hex(input::required(arguments, "--evidence")?)?;
    let (frozen, commit) = plane.freeze(&lease, output_root, evidence, input::now()?)?;
    let candidate = plane
        .get(key)?
        .and_then(|record| {
            record
                .receipt()
                .map(|receipt| hex_encode(receipt.id().as_bytes()))
        })
        .ok_or(ControlError::Corrupt)?;
    output::emit(&json!({
        "status": "frozen",
        "candidate": candidate,
        "lease": output::lease(frozen.fence()),
        "commit": output::commit_json(&commit)
    }))
}

pub(crate) fn evaluate(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let fence = input::fence(arguments)?;
    let lease = plane.frozen_for(key, &fence)?;
    let evaluator =
        Identity::<OwnerSchema>::from_label(input::required(arguments, "--evaluator")?)?;
    let verdict = parse_verdict(input::required(arguments, "--verdict")?, "evaluation")?;
    let evidence = Identity::<EvidenceSchema>::from_hex(input::required(arguments, "--evidence")?)?;
    let result = plane.evaluate(&lease, evaluator, verdict, evidence, input::now()?)?;
    emit_stage(result, "evaluation")
}

pub(crate) fn review(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let receipt = selected_receipt(
        &plane,
        key,
        input::optional(arguments, "--evaluation"),
        |record| record.evaluation().map(|receipt| receipt.id().to_bytes()),
    )?;
    let candidate = plane.evaluated_for(key, &receipt)?;
    let reviewer = Identity::<OwnerSchema>::from_label(input::required(arguments, "--reviewer")?)?;
    let verdict = parse_verdict(input::required(arguments, "--verdict")?, "review")?;
    let evidence = Identity::<EvidenceSchema>::from_hex(input::required(arguments, "--evidence")?)?;
    let result = plane.review(&candidate, reviewer, verdict, evidence)?;
    emit_stage(result, "review")
}

pub(crate) fn decide(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let receipt = selected_receipt(
        &plane,
        key,
        input::optional(arguments, "--review"),
        |record| record.review().map(|receipt| receipt.id().to_bytes()),
    )?;
    let candidate = plane.reviewed_for(key, &receipt)?;
    let sol = Identity::<OwnerSchema>::from_label(input::required(arguments, "--sol")?)?;
    let verdict = parse_verdict(input::required(arguments, "--verdict")?, "decision")?;
    let evidence = Identity::<EvidenceSchema>::from_hex(input::required(arguments, "--evidence")?)?;
    let result = plane.decide(&candidate, sol, verdict, evidence)?;
    output::emit(&json!({
        "status": format!("{:?}", result.status).to_ascii_lowercase(),
        "decision": hex_encode(result.receipt.id().as_bytes()),
        "commit": output::commit_json(&result.commit)
    }))
}

pub(crate) fn fail_or_cancel(command: &str, arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let key = key_from(&plane, arguments)?;
    let fence = input::fence(arguments)?;
    let lease = plane.lease_for(key, &fence)?;
    let commit = if command == "fail" {
        plane.fail(&lease)?
    } else {
        plane.cancel(&lease)?
    };
    output::emit(&json!({"commit": output::commit_json(&commit)}))
}

pub(crate) fn recover(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let (keys, commit) = plane.recover(input::now()?)?;
    output::emit(&json!({
        "expired": keys
            .iter()
            .map(|key| hex_encode(key.as_bytes()))
            .collect::<Vec<_>>(),
        "commit": commit.as_ref().map(output::commit_json)
    }))
}

pub(crate) fn invalidate(arguments: &[String]) -> Result<(), ControlError> {
    let mut plane = input::open(arguments)?;
    let dependency = key_from(&plane, arguments)?;
    let commit = plane.invalidate_dependency(dependency)?;
    output::emit(&json!({
        "dependency": hex_encode(dependency.as_bytes()),
        "commit": commit.as_ref().map(output::commit_json)
    }))
}

fn key_from(
    plane: &DurableControlPlane,
    arguments: &[String],
) -> Result<AgentWorkKey, ControlError> {
    input::admit_key(plane, input::required(arguments, "--key")?)
}

fn selected_receipt(
    plane: &DurableControlPlane,
    key: AgentWorkKey,
    explicit: Option<&str>,
    select: impl FnOnce(&backend_control::WorkRecord) -> Option<[u8; backend_version::ID_BYTES]>,
) -> Result<Vec<u8>, ControlError> {
    if let Some(explicit) = explicit {
        return backend_control::ids::decode_hex(explicit);
    }
    plane
        .get(key)?
        .as_ref()
        .and_then(select)
        .map(Vec::from)
        .ok_or(ControlError::InvalidTransition)
}

fn parse_verdict(value: &str, stage: &str) -> Result<CustodyVerdict, ControlError> {
    match value {
        "accepted" | "passed" | "promoted" | "verified" => Ok(CustodyVerdict::Accepted),
        "rejected" | "failed" => Ok(CustodyVerdict::Rejected),
        "quarantined" => Ok(CustodyVerdict::Quarantined),
        _ => Err(ControlError::Wire(format!("invalid {stage} verdict"))),
    }
}

fn emit_stage<S: backend_control::CandidateStage>(
    result: StageResult<S>,
    field: &str,
) -> Result<(), ControlError> {
    match result {
        StageResult::Advanced { candidate, commit } => output::emit(&json!({
            "status": format!("{:?}", S::STATUS).to_ascii_lowercase(),
            field: hex_encode(candidate.receipt().as_bytes()),
            "commit": output::commit_json(&commit)
        })),
        StageResult::Terminal { status, commit } => output::emit(&json!({
            "status": format!("{status:?}").to_ascii_lowercase(),
            "commit": output::commit_json(&commit)
        })),
    }
}
