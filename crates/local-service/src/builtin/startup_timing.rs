//! Bounded composition measurements; never a readiness or owner capability.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FILE_NAME: &str = "owner-startup-timing-v1.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Phase {
    Configuration,
    WorkspaceOpen,
    DirectoryAdmission,
    EmbeddingProvision,
    CompilerSelection,
    CompilerOwner,
    SemanticAuthorityOpen,
    SemanticReconcile,
    ViewRecovery,
    ViewRestore,
    ViewPublication,
    SqlProjection,
    ProjectionWatermarks,
    ClusterOwner,
    CommandOwner,
    QueryWarmup,
}

const PHASES: [Phase; 16] = [
    Phase::Configuration,
    Phase::WorkspaceOpen,
    Phase::DirectoryAdmission,
    Phase::EmbeddingProvision,
    Phase::CompilerSelection,
    Phase::CompilerOwner,
    Phase::SemanticAuthorityOpen,
    Phase::SemanticReconcile,
    Phase::ViewRecovery,
    Phase::ViewRestore,
    Phase::ViewPublication,
    Phase::SqlProjection,
    Phase::ProjectionWatermarks,
    Phase::ClusterOwner,
    Phase::CommandOwner,
    Phase::QueryWarmup,
];

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    Completed,
    Refused,
    Unmeasured,
}

#[derive(Serialize)]
struct Measurement {
    phase: Phase,
    elapsed_micros: Option<u64>,
    outcome: Outcome,
}

#[derive(Serialize)]
struct Receipt {
    version: u8,
    kind: &'static str,
    process_id: u32,
    composition_started_unix_millis: Option<u64>,
    workspace_root: Option<[u8; 32]>,
    composition_completed: bool,
    elapsed_micros: u64,
    phases: Vec<Measurement>,
}

pub(super) struct Timing {
    started: Instant,
    started_unix_millis: Option<u64>,
    active: (Phase, Instant),
    completed: Vec<Measurement>,
    workspace_root: Option<[u8; 32]>,
    destination: Option<PathBuf>,
}

impl Timing {
    pub(super) fn new() -> Self {
        let started = Instant::now();
        Self {
            started,
            started_unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
            active: (Phase::Configuration, started),
            completed: Vec::with_capacity(PHASES.len()),
            workspace_root: None,
            destination: None,
        }
    }

    pub(super) fn begin(&mut self, phase: Phase) {
        self.begin_at(phase, Instant::now());
    }

    fn begin_at(&mut self, phase: Phase, now: Instant) {
        self.completed.push(Measurement {
            phase: self.active.0,
            elapsed_micros: Some(micros(now.duration_since(self.active.1))),
            outcome: Outcome::Completed,
        });
        self.active = (phase, now);
    }

    /// Only the owner whose workspace namespace has passed admission can
    /// publish the diagnostic. The path is never serialized into its receipt.
    pub(super) fn admit_destination(&mut self, workspace: &Path) {
        self.destination = Some(workspace.join(FILE_NAME));
    }

    pub(super) fn workspace_root(&mut self, root: [u8; 32]) {
        self.workspace_root = Some(root);
    }

    fn receipt(mut self, composed: bool, now: Instant) -> Receipt {
        self.completed.push(Measurement {
            phase: self.active.0,
            elapsed_micros: Some(micros(now.duration_since(self.active.1))),
            outcome: if composed {
                Outcome::Completed
            } else {
                Outcome::Refused
            },
        });
        let phases = PHASES
            .into_iter()
            .map(|phase| {
                self.completed
                    .iter()
                    .position(|item| item.phase == phase)
                    .map(|index| self.completed.remove(index))
                    .unwrap_or(Measurement {
                        phase,
                        elapsed_micros: None,
                        outcome: Outcome::Unmeasured,
                    })
            })
            .collect();
        Receipt {
            version: 1,
            kind: "owner-composition-timing",
            process_id: std::process::id(),
            composition_started_unix_millis: self.started_unix_millis,
            workspace_root: self.workspace_root,
            composition_completed: composed,
            elapsed_micros: micros(now.duration_since(self.started)),
            phases,
        }
    }

    /// Measurement publication is best effort and cannot admit or reject an
    /// owner. Startup refusal and endpoint readiness keep their existing paths.
    pub(super) fn publish(mut self, composed: bool) {
        let Some(destination) = self.destination.take() else {
            return;
        };
        let receipt = self.receipt(composed, Instant::now());
        if let Ok(bytes) = serde_json::to_vec(&receipt) {
            let _ = backend_platform::durable::write_private_atomic_unflushed(&destination, &bytes);
        }
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_phase_retains_duration_and_unmeasured_phases_never_claim_readiness() {
        let mut timing = Timing::new();
        let started = timing.started;
        timing.begin_at(Phase::WorkspaceOpen, started + Duration::from_millis(2));
        let value = serde_json::to_value(timing.receipt(false, started + Duration::from_millis(7)))
            .expect("serializable timing receipt");
        assert_eq!(value["composition_completed"], false);
        assert_eq!(value["elapsed_micros"], 7000);
        assert_eq!(value["phases"][0]["elapsed_micros"], 2000);
        assert_eq!(value["phases"][1]["elapsed_micros"], 5000);
        assert_eq!(value["phases"][1]["outcome"], "refused");
        assert_eq!(value["phases"][2]["outcome"], "unmeasured");
        assert!(value["phases"][2]["elapsed_micros"].is_null());
        assert!(value["workspace_root"].is_null());
        assert!(value.get("ready").is_none());
        assert!(value.get("endpoint").is_none());
        assert!(value.get("workspace_path").is_none());
    }

    #[test]
    fn composed_receipt_is_bounded_provenance_without_endpoint_readiness() {
        let mut timing = Timing::new();
        let started = timing.started;
        let started_unix_millis = timing.started_unix_millis;
        timing.workspace_root([7; 32]);
        // The actual composition call sites traverse this closed phase set.
        // This exercises receipt provenance and bounds, not startup latency.
        for (index, phase) in PHASES.into_iter().enumerate().skip(1) {
            timing.begin_at(
                phase,
                started + Duration::from_secs(u64::try_from(index).expect("bounded phase index")),
            );
        }
        let receipt = timing.receipt(true, started + Duration::from_secs(16));
        let bytes = serde_json::to_vec(&receipt).expect("timing bytes");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("timing JSON");
        assert!(bytes.len() < 4096, "closed, path-free diagnostic payload");
        assert_eq!(value["process_id"], std::process::id());
        assert_eq!(
            value["composition_started_unix_millis"],
            serde_json::to_value(started_unix_millis).expect("start observation")
        );
        assert_eq!(
            value["workspace_root"],
            serde_json::to_value([7_u8; 32]).expect("workspace root")
        );
        assert_eq!(value["composition_completed"], true);
        assert_eq!(value["elapsed_micros"], 16_000_000);
        let phases = value["phases"].as_array().expect("closed phases");
        assert_eq!(phases.len(), PHASES.len());
        assert!(phases.iter().all(|phase| phase["outcome"] == "completed"));
        assert!(value.get("ready").is_none());
        assert!(value.get("endpoint").is_none());
        assert!(value.get("workspace_path").is_none());
    }
}
