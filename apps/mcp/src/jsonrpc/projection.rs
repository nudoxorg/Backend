//! Typed backend values projected into stable MCP JSON fields.

use backend_library::{
    CapabilityAuthority, CapabilityFamily, CapabilityLifecycle, CapabilityStatus, CapabilityTarget,
    CapabilityUnavailable, CommandFailure, Coverage, Fragment, Lane, LanguageOracleTask, Reason,
    RowState, SourceAvailability, SourceExcerpt, SourceExcerptExtent, ViewRoot, ViewStateRoot,
    encode_id,
};
use serde_json::{Value, json};

pub(super) fn source_availability_value(source: &SourceAvailability) -> Value {
    match source {
        SourceAvailability::Captured(source) => json!({
            "available": true,
            "state": "captured",
            "path": source.path(),
            "startLine": source.start_line()
        }),
        SourceAvailability::NotCaptured => json!({ "available": false, "state": "not_captured" }),
        SourceAvailability::NotHydrated => json!({ "available": false, "state": "not_hydrated" }),
        SourceAvailability::Unconfigured => json!({ "available": false, "state": "unconfigured" }),
    }
}

pub(super) fn source_excerpt_value(source: &SourceExcerpt) -> Value {
    match source {
        SourceExcerpt::Captured { text, extent } => json!({
            "available": true,
            "state": "captured",
            "text": text,
            "extent": match extent {
                SourceExcerptExtent::Complete => "complete",
                SourceExcerptExtent::Truncated => "truncated",
            }
        }),
        SourceExcerpt::NotCaptured => json!({ "available": false, "state": "not_captured" }),
        SourceExcerpt::NotHydrated => json!({ "available": false, "state": "not_hydrated" }),
        SourceExcerpt::Unconfigured => json!({ "available": false, "state": "unconfigured" }),
    }
}

pub(super) fn capability_value(status: &CapabilityStatus) -> Value {
    let family = match status.family() {
        CapabilityFamily::StructuralFrontend { profile } => json!({
            "kind": "structural_frontend",
            "profile": format!("{profile:?}")
        }),
        CapabilityFamily::LanguageOracle { profile, task } => json!({
            "kind": "language_oracle",
            "profile": format!("{profile:?}"),
            "task": language_task_name(task)
        }),
        CapabilityFamily::Embedding { recipe } => json!({
            "kind": "embedding",
            "recipe": recipe.map(|recipe| encode_id(&recipe.as_bytes()))
        }),
    };
    let target = status.target().map(|target| match target {
        CapabilityTarget::Native { os, architecture } => {
            json!({ "kind": "native", "os": os, "architecture": architecture })
        }
        CapabilityTarget::Managed(runtime) => json!({ "kind": "managed", "runtime": runtime }),
    });
    let (lifecycle, unavailable) = match status.lifecycle() {
        CapabilityLifecycle::Unavailable(reason) => {
            ("unavailable", Some(capability_unavailable_name(reason)))
        }
        CapabilityLifecycle::Probing => ("probing", None),
        CapabilityLifecycle::Installed => ("installed", None),
        CapabilityLifecycle::Resident => ("resident", None),
        CapabilityLifecycle::Active => ("active", None),
        CapabilityLifecycle::Ready => ("ready", None),
        CapabilityLifecycle::Revoked => ("revoked", None),
    };
    let authority = status.authority().map(|authority| match authority {
        CapabilityAuthority::Structural { producer } => json!({
            "kind": "structural",
            "producer": encode_id(&producer)
        }),
        CapabilityAuthority::Compiler {
            recipe,
            toolchain,
            toolchain_identity,
            package_authority,
        } => json!({
            "kind": "compiler",
            "recipe": encode_id(&recipe),
            "toolchain": u8::from(toolchain),
            "toolchainIdentity": encode_id(&toolchain_identity),
            "packageAuthority": {
                "scope": if package_authority.is_portable() {
                    "immutable_closure"
                } else {
                    "local_configuration"
                },
                "identity": encode_id(&package_authority.as_bytes())
            }
        }),
        CapabilityAuthority::Embedding(recipe) => json!({
            "kind": "embedding",
            "recipe": encode_id(&recipe.identity().as_bytes()),
            "model": encode_id(&recipe.model),
            "tokenizer": encode_id(&recipe.tokenizer),
            "source": format!("{:?}", recipe.source),
            "maximumInputBytes": recipe.maximum_input_bytes,
            "maximumTokens": recipe.maximum_tokens,
            "overlapTokens": recipe.overlap_tokens,
            "dimensions": recipe.dimensions,
            "metric": format!("{:?}", recipe.metric),
            "pooling": format!("{:?}", recipe.pooling),
            "normalization": format!("{:?}", recipe.normalization),
            "encoding": format!("{:?}", recipe.encoding),
            "queryTreatment": encode_id(&recipe.query_treatment),
            "documentTreatment": encode_id(&recipe.document_treatment)
        }),
    });
    json!({
        "id": encode_id(&status.id().as_bytes()),
        "family": family,
        "manifest": status.manifest().map(|manifest| encode_id(&manifest)),
        "target": target,
        "protocolAbi": status.protocol_abi(),
        "authority": authority,
        "lifecycle": lifecycle,
        "unavailableReason": unavailable
    })
}

const fn language_task_name(task: LanguageOracleTask) -> &'static str {
    match task {
        LanguageOracleTask::Parse => "parse",
        LanguageOracleTask::TypeCheck => "type_check",
        LanguageOracleTask::SemanticIndex => "semantic_index",
    }
}

const fn capability_unavailable_name(reason: CapabilityUnavailable) -> &'static str {
    match reason {
        CapabilityUnavailable::NoManifest => "no_manifest",
        CapabilityUnavailable::NotInstalled => "not_installed",
        CapabilityUnavailable::UnsupportedTarget => "unsupported_target",
        CapabilityUnavailable::UnsupportedAbi => "unsupported_abi",
        CapabilityUnavailable::MissingDependency => "missing_dependency",
        CapabilityUnavailable::ProbeFailed => "probe_failed",
    }
}

pub(super) fn error_value(kind: &str, message: impl Into<String>) -> Value {
    json!({ "error": { "kind": kind, "message": message.into() } })
}

pub(super) fn readiness(view: &ViewRoot) -> &'static str {
    coverage_readiness(
        view.coverage(),
        view.rows().iter().any(|row| row.state == RowState::Failed),
        view.rows().iter().any(|row| row.state == RowState::Loading),
    )
}

pub(super) fn coverage_readiness(
    coverage: &[Coverage],
    failed: bool,
    loading: bool,
) -> &'static str {
    if coverage
        .iter()
        .any(|coverage| matches!(coverage, Coverage::Unavailable { .. }))
        || failed
    {
        "unavailable"
    } else if coverage
        .iter()
        .any(|coverage| matches!(coverage, Coverage::Partial { .. }))
        || loading
    {
        "indexing"
    } else if !coverage.is_empty() && coverage.iter().all(|coverage| coverage.is_complete()) {
        "ready"
    } else {
        "unknown"
    }
}

pub(super) fn coverage_value(coverage: Coverage) -> Value {
    match coverage {
        Coverage::Complete => json!({ "state": "complete" }),
        Coverage::Partial {
            lane,
            completed,
            total,
        } => {
            json!({ "state": "partial", "lane": lane_name(lane), "completed": completed, "total": total })
        }
        Coverage::Unavailable { lane, reason } => json!({
            "state": "unavailable",
            "lane": lane_name(lane),
            "reason": reason_name(reason)
        }),
    }
}

pub(super) const fn row_state_name(state: RowState) -> &'static str {
    match state {
        RowState::Ready => "ready",
        RowState::Loading => "loading",
        RowState::Failed => "failed",
    }
}

pub(super) const fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Exact => "exact",
        Lane::Names => "names",
        Lane::Graph => "graph",
        Lane::Semantic => "semantic",
    }
}

pub(super) const fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "no_index",
        Reason::Unconfigured => "unconfigured",
        Reason::Offline => "offline",
        Reason::Cancelled => "cancelled",
        Reason::Incomplete => "incomplete",
    }
}

pub(super) const fn command_failure_kind(failure: &CommandFailure) -> &'static str {
    match failure {
        CommandFailure::NotFound => "not_found",
        CommandFailure::WrongBasis { .. } => "wrong_basis",
        CommandFailure::InvalidQuery(_) => "invalid_query",
        CommandFailure::CursorMismatch => "cursor_mismatch",
        CommandFailure::IncoherentView(_) => "incoherent_view",
        CommandFailure::SequenceOverflow => "sequence_overflow",
        CommandFailure::MutationRequiresOwner => "mutation_requires_owner",
    }
}

pub(super) fn fragments_text(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(text) | Fragment::Code(text) => output.push_str(text),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

pub(super) fn short_root(view: &ViewStateRoot) -> String {
    encode_id(view.as_bytes())[..12].to_owned()
}

pub(super) fn display_name(coordinate: &str) -> &str {
    coordinate.rsplit("::").next().unwrap_or(coordinate)
}
