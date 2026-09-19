//! Stable JSON output for the command adapter.

use backend_control::ids::hex_encode;
use backend_control::{Admission, ControlCommit, ControlError, LeaseFence, PlanResult};
use serde_json::{Value, json};

pub(crate) fn plan_result(result: PlanResult) -> Value {
    match result {
        PlanResult::Admitted(commit) => {
            json!({"kind": "admitted", "commit": commit_json(&commit)})
        }
        PlanResult::Reused(receipt) => {
            json!({"kind": "reused", "receipt": hex_encode(receipt.id().as_bytes())})
        }
        PlanResult::Existing { status, waiters } => json!({
            "kind": "existing",
            "status": format!("{status:?}").to_ascii_lowercase(),
            "waiters": waiters
        }),
    }
}

pub(crate) fn admission(admission: Admission) -> Value {
    match admission {
        Admission::Owned(held) => json!({"kind": "owned", "lease": lease(held.fence())}),
        Admission::Coalesced { status, waiters } => json!({
            "kind": "coalesced",
            "status": format!("{status:?}").to_ascii_lowercase(),
            "waiters": waiters
        }),
        Admission::Waiting => json!({"kind": "waiting"}),
        Admission::Queued => json!({"kind": "queued"}),
        Admission::Reused(receipt) => {
            json!({"kind": "reused", "receipt": hex_encode(receipt.id().as_bytes())})
        }
    }
}

pub(crate) fn lease(lease: LeaseFence) -> Value {
    json!({
        "work_key": hex_encode(lease.key().as_bytes()),
        "owner": hex_encode(lease.owner().as_bytes()),
        "epoch": lease.epoch(),
        "fence": hex_encode(lease.fence().as_bytes()),
        "expires_at": lease.expires_at(),
        "attempt": lease.attempt(),
    })
}

pub(crate) fn commit_json(commit: &ControlCommit) -> Value {
    json!({
        "base": hex_encode(commit.base.as_bytes()),
        "target": hex_encode(commit.target.as_bytes()),
        "delta": hex_encode(commit.delta.id().as_bytes()),
    })
}

pub(crate) fn emit(value: &Value) -> Result<(), ControlError> {
    let bytes = serde_json::to_vec(value).map_err(|error| ControlError::Wire(error.to_string()))?;
    println!(
        "{}",
        String::from_utf8(bytes).map_err(|_| ControlError::Wire("json encoding".to_owned()))?
    );
    Ok(())
}
