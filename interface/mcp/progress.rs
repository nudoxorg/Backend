//! Defines progress behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the progress invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! `notifications/progress` for the one command that is slow enough to need it.
//!
//! Only `add` reports progress, because only `add` runs a compiler. A phase is never a claim of
//! success: the journey has eight ordered steps and reaching the seventh still ends in a failure
//! row if publication fails. The notification says which step, out of how many, and nothing more.

use interface_library::{AddProgress, CompilePhaseProgress};
use serde_json::Value;

/// Total ordered steps in one compile journey, as the shelf counts them.
pub const TOTAL_PHASES: u64 = 8;

/// One progress notification's position and label, or `None` when the signal carries no phase.
#[must_use]
pub fn step(progress: AddProgress) -> (u64, &'static str) {
    match progress {
        // Admission is the zeroth step: the lock is held and the row exists, but no phase has run.
        AddProgress::Admitted { .. } => (0, "admitted"),
        AddProgress::Phase(phase) => (u64::from(phase.ordinal).saturating_add(1), phase.label()),
        // Indexing follows the reopen that proves the publication, so it sits at the discovery step.
        AddProgress::Indexing => (
            u64::from(CompilePhaseProgress::of(compiler_phase_discover()).ordinal)
                .saturating_add(1),
            "index",
        ),
    }
}

const fn compiler_phase_discover() -> interface_core::PackageCompilePhase {
    interface_core::PackageCompilePhase::Discover
}

/// Builds the notification for one progress signal against a client's token.
#[must_use]
pub fn notification(token: &Value, progress: AddProgress) -> Value {
    let (value, message) = step(progress);
    interface_protocol::mcp::progress_notification(token, value, TOTAL_PHASES, message)
}

#[cfg(test)]
mod tests {
    use interface_core::{CorrelationId, PackageCompilePhase};

    use super::*;

    #[test]
    fn phases_walk_one_to_eight_and_admission_precedes_them() {
        assert_eq!(
            step(AddProgress::Admitted {
                correlation: CorrelationId(1)
            }),
            (0, "admitted")
        );
        assert_eq!(
            step(AddProgress::Phase(CompilePhaseProgress::of(
                PackageCompilePhase::Locate
            ))),
            (1, "locate")
        );
        assert_eq!(
            step(AddProgress::Phase(CompilePhaseProgress::of(
                PackageCompilePhase::Render
            ))),
            (TOTAL_PHASES, "render")
        );
        assert_eq!(step(AddProgress::Indexing), (7, "index"));
    }
}
