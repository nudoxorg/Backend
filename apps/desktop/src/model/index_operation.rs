//! Caller-owned durable identity for one exact local index mutation.
//!
//! A service revision or an indexed package is not an operation receipt. This
//! claim survives interruption so reconciliation can address exactly the work
//! prepared before the first transport send.

use crate::core::LocalProjectId;
use backend_library::{CompileExecutionIntent, IndexOperationKey, PackageReference};
use serde::{Deserialize, Serialize};

/// Immutable payload bound to a caller key, persisted before submission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexOperationClaim {
    /// Opaque owner-independent identity. It is never an ephemeral RequestId.
    pub key: IndexOperationKey,
    /// Exact package admitted at the native path and product boundary.
    pub package: PackageReference,
    /// Execution policy sent with the first request and every safe replay.
    pub execution_intent: CompileExecutionIntent,
    /// Canonical owner evidence; missing means no answer was admitted yet.
    #[serde(default)]
    pub observation: Option<backend_library::IndexOperationObservation>,
}

impl IndexOperationClaim {
    /// Allocate an owner-independent key from the operating system. Allocation
    /// does not submit anything; the caller must save the claim before send.
    pub fn fresh(project: &LocalProjectId) -> Result<Self, String> {
        let mut bytes = [0_u8; 32];
        #[cfg(unix)]
        {
            use std::io::Read as _;
            std::fs::File::open("/dev/urandom").and_then(|mut source| source.read_exact(&mut bytes))
                .map_err(|error| format!("The index operation key could not be generated: {error}"))?;
        }
        #[cfg(windows)]
        backend_platform::win32::random::fill(&mut bytes)
            .map_err(|error| format!("The index operation key could not be generated: {error}"))?;
        #[cfg(not(any(unix, windows)))]
        return Err("This platform cannot generate a durable index operation key.".to_owned());
        // The canonical key owns nonzero and lower-hex admission. An entropy
        // failure is never replaced by a timestamp, PID, root or local counter.
        let key = IndexOperationKey::from_bytes(bytes).map_err(|error| error.to_string())?;
        Self::for_project(key, project)
    }

    /// Bind an already generated key to the selected native project.
    pub fn for_project(key: IndexOperationKey, project: &LocalProjectId) -> Result<Self, String> {
        let coordinate = project.service_coordinate().map_err(|error| error.to_string())?;
        let package = PackageReference::parse(coordinate.to_owned()).map_err(|error| error.to_string())?;
        Ok(Self { key, package, execution_intent: CompileExecutionIntent::Interactive, observation: None })
    }

    /// Match an observation to the caller payload and re-admit the canonical
    /// receipt shape, including digest and fixed-width publication cursor.
    #[must_use]
    pub fn admits_observation(&self, observation: &backend_library::IndexOperationObservation) -> bool {
        use backend_library::{IndexOperationObservation, SurfaceCommand, SurfaceReply};
        let command = SurfaceCommand::IndexOperationStatus { operation_key: self.key };
        if SurfaceReply::IndexOperationStatus(observation.clone()).admit(command.id()).is_err() { return false; }
        match observation {
            IndexOperationObservation::Known(status) => status.operation_key == self.key
                && status.package == self.package && status.execution_intent == self.execution_intent
                && status.request_digest == backend_library::index_operation_request_digest(&self.package, self.execution_intent),
            IndexOperationObservation::Unknown { operation_key } => *operation_key == self.key,
        }
    }

    /// Durable request identity excludes the latest changing observation.
    #[must_use]
    pub fn same_request(&self, other: &Self) -> bool {
        self.key == other.key && self.package == other.package && self.execution_intent == other.execution_intent
    }

    /// Whether the owner still owes a terminal observation. This is a work
    /// lifecycle, never a guess from package coverage or elapsed time.
    #[must_use]
    pub fn needs_observation(&self) -> bool {
        match self.observation.as_ref() {
            None => true,
            Some(backend_library::IndexOperationObservation::Known(status)) => matches!(status.state,
                backend_library::IndexOperationState::Accepted | backend_library::IndexOperationState::Active { .. }),
            Some(backend_library::IndexOperationObservation::Unknown { .. }) => false,
        }
    }

    /// Plain product wording for the exact owner observation. No key, digest
    /// or protocol representation is exposed as ordinary progress.
    #[must_use]
    pub fn status_text(&self) -> &'static str {
        use backend_library::{IndexJobStage, IndexOperationObservation, IndexOperationState};
        match self.observation.as_ref() {
            None => "Checking the saved index operation",
            Some(IndexOperationObservation::Unknown { .. }) => "No operation receipt is available",
            Some(IndexOperationObservation::Known(status)) => match &status.state {
                IndexOperationState::Accepted => "Accepted by the index owner",
                IndexOperationState::Active { stage, .. } => match stage {
                    IndexJobStage::Acquiring => "Acquiring the package",
                    IndexJobStage::Staging => "Preparing the package sources",
                    IndexJobStage::Scanning => "Reading the package sources",
                    IndexJobStage::Compiling => "Compiling the package",
                    IndexJobStage::Publishing => "Publishing the index",
                },
                IndexOperationState::Published(_) => "Index published",
                IndexOperationState::Failed { .. } => "Index stopped before publication",
                IndexOperationState::Unresolved { .. } => "Index outcome is unresolved",
            },
        }
    }

    /// Admit a restored payload against the durable native project identity.
    #[must_use]
    pub fn belongs_to(&self, project: &LocalProjectId) -> bool {
        project.service_coordinate().ok()
            .and_then(|coordinate| PackageReference::parse(coordinate.to_owned()).ok())
            .is_some_and(|package| package == self.package)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn claim(project: &LocalProjectId, byte: u8) -> IndexOperationClaim {
        // Exercise the canonical key's wire admission without depending on an
        // owner-local ticket or generation. Production uses OS entropy.
        let wire = format!("\"{}\"", format!("{byte:02x}").repeat(32));
        let key = serde_json::from_str(&wire).expect("nonzero canonical caller key");
        IndexOperationClaim::for_project(key, project).expect("exact project payload")
    }

    pub(crate) fn observation(operation: &IndexOperationClaim, state: backend_library::IndexOperationState) -> backend_library::IndexOperationObservation {
        backend_library::IndexOperationObservation::Known(backend_library::IndexOperationStatus {
            operation_key: operation.key,
            request_digest: backend_library::index_operation_request_digest(&operation.package, operation.execution_intent),
            package: operation.package.clone(), execution_intent: operation.execution_intent, state,
        })
    }

    /// A checked transport fixture receipt, not a claim of live compilation.
    pub(crate) fn published(operation: &IndexOperationClaim) -> backend_library::IndexOperationObservation {
        let root = backend_library::view_state_root(&[]);
        let basis = backend_library::Basis::new(root, backend_library::object_version(b"desktop-operation-fixture"));
        let frontier = backend_library::Frontier::new(backend_library::branch_key("main"),
            backend_library::log_key("library"), backend_library::CURSOR_SCHEMA, root, 0);
        let view = backend_library::ViewRoot::new_incomplete(backend_library::view_key(b"desktop-operation-fixture"),
            basis, frontier, Vec::new(), Vec::new()).expect("checked fixture view");
        let receipt = backend_library::IndexOperationPublicationReceipt::from_published_view(
            Some([1; 32]), [2; 32], [3; 32], 1, &view, backend_library::Cursor::for_view_root(&view)).expect("checked fixture receipt");
        observation(operation, backend_library::IndexOperationState::Published(receipt))
    }

    #[test]
    fn observation_admission_requires_exact_key_payload_and_checked_receipt() {
        let project = LocalProjectId::new("/fixture/admitted-operation").expect("project");
        let operation = claim(&project, 0x71);
        assert!(operation.admits_observation(&published(&operation)));
        assert!(operation.admits_observation(&backend_library::IndexOperationObservation::Unknown { operation_key: operation.key }));
        let another = claim(&project, 0x72);
        assert!(!operation.admits_observation(&published(&another)));
        assert!(!operation.admits_observation(&backend_library::IndexOperationObservation::Unknown { operation_key: another.key }));
        let other_project = LocalProjectId::new("/fixture/another-package").expect("project");
        let wrong_payload = IndexOperationClaim::for_project(operation.key, &other_project).expect("payload");
        assert!(!operation.admits_observation(&published(&wrong_payload)));
        let mut wrong_digest = published(&operation);
        if let backend_library::IndexOperationObservation::Known(status) = &mut wrong_digest { status.request_digest = [0; 32]; }
        assert!(!operation.admits_observation(&wrong_digest));
    }

    #[test]
    fn fresh_attempts_for_one_project_have_distinct_admitted_keys() {
        let project = LocalProjectId::new("/fixture/exact-operation").expect("project");
        let first = IndexOperationClaim::fresh(&project).expect("OS key");
        let second = IndexOperationClaim::fresh(&project).expect("OS key");
        assert_ne!(first.key, second.key);
        assert_eq!(first.package, second.package);
        assert_eq!(first.execution_intent, second.execution_intent);
    }

    #[test]
    fn durable_claim_retains_key_and_payload_and_refuses_another_project() {
        let project = LocalProjectId::new("/fixture/exact-operation").expect("project");
        let other = LocalProjectId::new("/fixture/other-operation").expect("project");
        let operation = claim(&project, 0x51);
        let bytes = serde_json::to_vec(&operation).expect("durable claim");
        let restored: IndexOperationClaim = serde_json::from_slice(&bytes).expect("durable claim");
        assert_eq!(restored, operation);
        assert!(restored.belongs_to(&project));
        assert!(!restored.belongs_to(&other));
        assert_ne!(restored, claim(&project, 0x52), "a deliberate new attempt has its own key");
    }
}
