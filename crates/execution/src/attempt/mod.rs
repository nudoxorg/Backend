//! Internal attempt modules: lease state, proof/admission, and fenced history.

mod admission;
mod history;
mod lease;
mod proof;

pub use admission::{OutputAdmission, OutputAdmissionError, ResultReceipt, UntrustedResultReceipt};
pub use history::{AttemptError, AttemptManager};
pub use lease::{AttemptFence, AttemptLease, AttemptState, ResultCoverage};
pub use proof::{
    AuthorityValidationError, AuthorityVerifier, BoundOutputValidator, ExecutionAuthorityEvidence,
    OutputValidationError, OutputValidator, UntrustedAuthorityClaim, UntrustedOutputClaim,
};

use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

pub(super) static MANAGER_NONCE: AtomicU64 = AtomicU64::new(1);
pub(super) static PROCESS_FENCE_SECRET: OnceLock<[u8; 32]> = OnceLock::new();
