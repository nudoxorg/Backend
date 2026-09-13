//! Deterministic human projection of the shared application reply vocabulary.
//!
//! JSON remains the lossless default.  This module only renders facts already carried by the
//! reply and never infers provider success, snapshot residence, or an untyped error status.

use core::fmt;

use interface_core::{
    ApplicationOutcome, ApplicationReply, Capability, CapabilityHealth, DiagnosticCode,
    ExecutionReply, ReplyBody,
};

/// Writes one human-readable projection without allocating a second reply representation.
///
/// # Errors
///
/// Returns only the caller writer's exact formatting failure.
pub fn write_human<Output>(reply: &ApplicationReply, output: &mut Output) -> fmt::Result
where
    Output: fmt::Write,
{
    match &reply.outcome {
        ApplicationOutcome::Resolved(body) => write_body(body, output),
        ApplicationOutcome::Failed { diagnostic } => {
            writeln!(output, "error: {}", diagnostic_name(diagnostic.code))?;
            writeln!(output, "  inspect the typed diagnostic in JSON output")
        }
    }
}

fn write_body<Output>(body: &ReplyBody, output: &mut Output) -> fmt::Result
where
    Output: fmt::Write,
{
    match body {
        ReplyBody::Generated(_) => writeln!(output, "generated: typed compiler result"),
        ReplyBody::DependencyUnavailable { capability } => writeln!(
            output,
            "capability: {} unavailable",
            capability_name(*capability)
        ),
        ReplyBody::Health(facts) => {
            writeln!(output, "health:")?;
            for fact in facts {
                match fact {
                    CapabilityHealth::LocalReady(capability) => {
                        writeln!(output, "  {}: ready", capability_name(*capability))?
                    }
                    CapabilityHealth::Unavailable(capability) => {
                        writeln!(output, "  {}: unavailable", capability_name(*capability))?
                    }
                }
            }
            Ok(())
        }
        ReplyBody::Adaptive(_) => writeln!(output, "adaptive: typed decision"),
        ReplyBody::ExecutionStarted { operation, .. } => {
            writeln!(output, "operation: started {}", operation.0)
        }
        ReplyBody::Execution(state) => match state {
            ExecutionReply::Pending { operation, .. } => {
                writeln!(output, "operation: pending {}", operation.0)
            }
            ExecutionReply::Completed { operation, .. } => {
                writeln!(output, "operation: completed {}", operation.0)
            }
            ExecutionReply::Cancelled { operation, .. } => {
                writeln!(output, "operation: cancelled {}", operation.0)
            }
        },
        ReplyBody::Snapshot(facts) => writeln!(
            output,
            "snapshot: {} resident={}",
            &*facts.snapshot, facts.resident
        ),
        ReplyBody::Retrieval(rows) => {
            writeln!(output, "retrieval: {} row(s)", rows.len())?;
            for row in rows {
                let mode = match row.mode {
                    interface_core::RetrievalMode::Exact => "exact",
                    interface_core::RetrievalMode::Lexical => "lexical",
                };
                writeln!(
                    output,
                    "  {} {} score={} document={}",
                    mode, &*row.term, row.score, &*row.document
                )?;
            }
            Ok(())
        }
        ReplyBody::IndexRemoved(receipt) => writeln!(
            output,
            "snapshot-unload: {} removed={}",
            &*receipt.snapshot, receipt.removed
        ),
    }
}

const fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::CompilerRegistry => "compiler_registry",
        Capability::CompilerOutput => "compiler_output",
        Capability::Index => "index",
        Capability::Graph => "graph",
        Capability::Vector => "vector",
        Capability::LocalAnalyzer => "local_analyzer",
        Capability::Remote => "remote",
    }
}

const fn diagnostic_name(code: DiagnosticCode) -> &'static str {
    match code {
        DiagnosticCode::SemanticTextTooLong => "semantic_text_too_long",
        DiagnosticCode::ResultLimitExceeded => "result_limit_exceeded",
        DiagnosticCode::DependencyUnavailable => "dependency_unavailable",
        DiagnosticCode::UnsupportedCompilerStage => "unsupported_compiler_stage",
        DiagnosticCode::OperationUnavailable => "operation_unavailable",
        DiagnosticCode::AdaptivePolicyRejected => "adaptive_policy_rejected",
        DiagnosticCode::CompilerTerminal => "compiler_terminal",
        DiagnosticCode::ExecutionFailed => "execution_failed",
        DiagnosticCode::RetrievalFailed => "retrieval_failed",
    }
}
