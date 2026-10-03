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
        let text: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let wire = serde_json::to_string(&text).map_err(|error| error.to_string())?;
        let key = serde_json::from_str(&wire).map_err(|error| error.to_string())?;
        Self::for_project(key, project)
    }

    /// Bind an already generated key to the selected native project.
    pub fn for_project(key: IndexOperationKey, project: &LocalProjectId) -> Result<Self, String> {
        let coordinate = project.service_coordinate().map_err(|error| error.to_string())?;
        let package = PackageReference::parse(coordinate.to_owned()).map_err(|error| error.to_string())?;
        Ok(Self { key, package, execution_intent: CompileExecutionIntent::Interactive })
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
