//! Bounded composition measurements; never a readiness or owner capability.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FILE_NAME: &str = "owner-startup-timing-v1.json";
const CHECKPOINT_FILE_NAME: &str = "owner-startup-active-v1.json";
const MAX_CHECKPOINT_BYTES: usize = 4096;

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

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CheckpointState {
    Opening,
    Completed,
    Refused,
}

/// A last observation of one composition, never a live-process or owner proof.
/// PID and start time must both be correlated with independent process evidence;
/// even matching or forged fields cannot grant authority or readiness.
#[derive(Serialize)]
struct Checkpoint<'a> {
    version: u8,
    kind: &'static str,
    process_id: u32,
    composition_started_unix_millis: Option<u64>,
    workspace_root: Option<[u8; 32]>,
    state: CheckpointState,
    phase: Phase,
    phase_elapsed_micros: u64,
    elapsed_micros: u64,
    completed_phases: &'a [Measurement],
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
        // Trace only the closed phase vocabulary, never paths or credentials.
        // A blocked phase has no completion receipt yet.
        if std::env::var_os("NUDOX_TRACE").is_some() {
            eprintln!("backend-local-service: owner startup phase {phase:?}");
        }
        self.begin_at(phase, Instant::now());
    }

    fn begin_at(&mut self, phase: Phase, now: Instant) {
        self.completed.push(Measurement {
            phase: self.active.0,
            elapsed_micros: Some(micros(now.duration_since(self.active.1))),
            outcome: Outcome::Completed,
        });
        self.active = (phase, now);
        self.publish_checkpoint_at(CheckpointState::Opening, now);
    }

    /// Only the owner whose workspace namespace has passed admission can
    /// publish the diagnostic. The path is never serialized into its receipt.
    pub(super) fn admit_destination(&mut self, workspace: &Path) {
        self.destination = Some(workspace.join(FILE_NAME));
        self.publish_checkpoint_at(CheckpointState::Opening, Instant::now());
    }

    pub(super) fn workspace_root(&mut self, root: [u8; 32]) {
        self.workspace_root = Some(root);
        self.publish_checkpoint_at(CheckpointState::Opening, Instant::now());
    }

    fn checkpoint(&self, state: CheckpointState, now: Instant) -> Checkpoint<'_> {
        Checkpoint {
            version: 1,
            kind: "owner-composition-checkpoint",
            process_id: std::process::id(),
            composition_started_unix_millis: self.started_unix_millis,
            workspace_root: self.workspace_root,
            state,
            phase: self.active.0,
            phase_elapsed_micros: micros(now.duration_since(self.active.1)),
            elapsed_micros: micros(now.duration_since(self.started)),
            completed_phases: &self.completed,
        }
    }

    fn publish_checkpoint_at(&self, state: CheckpointState, now: Instant) {
        // Unix's existing diagnostic writer omits both durability barriers.
        // Other platforms retain only their existing final receipt: that API
        // performs durable writes there, unsuitable for every live phase.
        if !cfg!(unix) {
            return;
        }
        let Some(destination) = self.destination.as_ref() else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(&self.checkpoint(state, now))
            && bytes.len() <= MAX_CHECKPOINT_BYTES
        {
            let _ = backend_platform::durable::write_private_atomic_unflushed(
                &destination.with_file_name(CHECKPOINT_FILE_NAME),
                &bytes,
            );
        }
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
    pub(super) fn publish(self, composed: bool) {
        self.publish_at(composed, Instant::now());
    }

    fn publish_at(mut self, composed: bool, now: Instant) {
        self.publish_checkpoint_at(
            if composed {
                CheckpointState::Completed
            } else {
                CheckpointState::Refused
            },
            now,
        );
        let Some(destination) = self.destination.take() else {
            return;
        };
        let receipt = self.receipt(composed, now);
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

    #[cfg(unix)]
    #[test]
    fn active_checkpoint_keeps_the_new_instance_separate_from_a_stale_final_receipt() {
        let directory = tempfile::tempdir().expect("private diagnostic directory");
        let final_path = directory.path().join(FILE_NAME);
        let checkpoint_path = directory.path().join(CHECKPOINT_FILE_NAME);
        let mut stale = Timing::new();
        let stale_started = stale.started;
        stale.workspace_root([3; 32]);
        let mut old =
            serde_json::to_value(stale.receipt(true, stale_started + Duration::from_millis(1)))
                .expect("old final receipt");
        old["process_id"] = serde_json::json!(std::process::id().wrapping_add(1));
        backend_platform::durable::write_private_atomic_unflushed(
            &final_path,
            &serde_json::to_vec(&old).expect("old receipt bytes"),
        )
        .expect("retained previous completed receipt");

        let mut unadmitted = Timing::new();
        unadmitted.begin_at(
            Phase::WorkspaceOpen,
            unadmitted.started + Duration::from_millis(1),
        );
        assert!(!checkpoint_path.exists(), "no diagnostic before admission");
        let mut timing = Timing::new();
        let started = timing.started;
        let instance_start = timing.started_unix_millis;
        timing.workspace_root([7; 32]);
        // Transitions alone cannot publish before namespace admission.
        assert!(!checkpoint_path.exists());
        timing.admit_destination(directory.path());
        let read = |path: &Path| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(path).expect("diagnostic bytes"))
                .expect("diagnostic JSON")
        };
        let initial = read(&checkpoint_path);
        assert_ne!(initial["process_id"], old["process_id"]);
        assert_eq!(initial["kind"], "owner-composition-checkpoint");
        assert_eq!(initial["state"], "opening");
        assert_eq!(
            read(&final_path),
            old,
            "the prior final receipt is retained"
        );

        // A local file with this PID is still no process/owner capability.
        // No reader imports even a forged same-PID identity into Timing.
        let mut forged = initial.clone();
        forged["composition_started_unix_millis"] = serde_json::json!(u64::MAX);
        forged["state"] = serde_json::json!("completed");
        std::fs::write(
            &checkpoint_path,
            serde_json::to_vec(&forged).expect("forged bytes"),
        )
        .expect("replace test observation");
        timing.begin_at(Phase::WorkspaceOpen, started + Duration::from_millis(2));
        let opening = read(&checkpoint_path);
        assert_eq!(opening["process_id"], std::process::id());
        assert_eq!(
            opening["composition_started_unix_millis"],
            serde_json::to_value(instance_start).expect("new instance observation")
        );
        assert_ne!(
            opening["composition_started_unix_millis"],
            forged["composition_started_unix_millis"]
        );
        assert_eq!(opening["state"], "opening");
        assert_eq!(opening["phase"], "workspace-open");
        assert_eq!(opening["phase_elapsed_micros"], 0);
        assert_eq!(opening["elapsed_micros"], 2000);
        assert_eq!(opening["completed_phases"][0]["elapsed_micros"], 2000);
        assert_eq!(read(&final_path), old);

        timing.publish_at(false, started + Duration::from_millis(7));
        let closed = read(&checkpoint_path);
        let final_receipt = read(&final_path);
        assert_eq!(closed["state"], "refused");
        assert_eq!(closed["phase_elapsed_micros"], 5000);
        assert_eq!(closed["elapsed_micros"], 7000);
        assert_eq!(final_receipt["composition_completed"], false);
        assert_eq!(closed["process_id"], final_receipt["process_id"]);
        assert_eq!(
            closed["composition_started_unix_millis"],
            final_receipt["composition_started_unix_millis"]
        );
        assert_eq!(closed["workspace_root"], final_receipt["workspace_root"]);
        assert_eq!(closed["elapsed_micros"], final_receipt["elapsed_micros"]);
    }

    #[cfg(unix)]
    #[test]
    fn active_checkpoint_is_bounded_and_closes_when_composition_completes() {
        let directory = tempfile::tempdir().expect("private diagnostic directory");
        let mut timing = Timing::new();
        timing.admit_destination(directory.path());
        let started = timing.started;
        for (index, phase) in PHASES.into_iter().enumerate().skip(1) {
            timing.begin_at(
                phase,
                started + Duration::from_secs(u64::try_from(index).expect("bounded phase index")),
            );
        }
        timing.publish_at(true, started + Duration::from_secs(16));
        let bytes =
            std::fs::read(directory.path().join(CHECKPOINT_FILE_NAME)).expect("closed checkpoint");
        assert!(bytes.len() <= MAX_CHECKPOINT_BYTES);
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("checkpoint JSON");
        assert_eq!(value["state"], "completed");
        assert_eq!(value["phase"], "query-warmup");
        assert_eq!(value["phase_elapsed_micros"], 1_000_000);
        assert_eq!(value["elapsed_micros"], 16_000_000);
        assert_eq!(
            value["completed_phases"]
                .as_array()
                .expect("completed prefix")
                .len(),
            PHASES.len() - 1
        );
        for name in ["ready", "endpoint", "workspace_path", "owner", "alive"] {
            assert!(
                value.get(name).is_none(),
                "diagnostic does not claim {name}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_write_failure_does_not_change_composition_outcome() {
        let directory = tempfile::tempdir().expect("test directory");
        let not_directory = directory.path().join("not-a-directory");
        std::fs::write(&not_directory, b"untouched").expect("blocked diagnostic parent");
        let mut timing = Timing::new();
        let started = timing.started;
        timing.admit_destination(&not_directory);
        timing.begin_at(Phase::WorkspaceOpen, started + Duration::from_millis(2));
        timing.publish_at(true, started + Duration::from_millis(7));
        assert_eq!(
            std::fs::read(&not_directory).expect("original bytes"),
            b"untouched"
        );
        assert!(!not_directory.join(CHECKPOINT_FILE_NAME).exists());
        assert!(!not_directory.join(FILE_NAME).exists());
    }
}
