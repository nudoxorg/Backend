//! Bounded compiler-owned proof for a single empty native TS program.
//!
//! This private evidence is not an inventory-completeness claim. The owner
//! must separately bind the exact prior project and selected publication, and
//! revalidate this witness at the eventual durable grant.
use super::typescript_host::{TypeScriptProjectHost, TypeScriptProjectHostError};
use crate::workspace::retirement_budget::{Metered, RetirementBudget};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

// The specialized host path performs no file reads, directory iteration, JSON
// parsing, or source scan. Its only allocations are canonical/root path copies
// and the two root marker paths (`tsconfig.json`, `angular.json`), each derived
// from a path capped at 4096 bytes. The 64 KiB transient reservation covers
// the overlapping canonical copies, marker paths, witness construction, and
// error bookkeeping. It is acquired before host capture and held through all
// temporary drops. The proof has a separate 1 MiB retained reservation.
const EMPTY_NATIVE_SCAN_RESERVATION_BYTES: usize = 64 * 1024;
const EMPTY_NATIVE_PROOF_RESERVATION_BYTES: usize = 1024 * 1024;
// The specialized host does not yet prove equivalence with normal ancestor
// workspace and nested-config selection. Do not issue a compiler capability
// until a complete bound, compiler execution, and process controls establish it.
const EMPTY_NATIVE_COMPILER_AUTHORITY_VERIFIED: bool = false;

/// A freshly admitted native TS program with no authored or external source
/// members, under a no-config project with an empty inferred-root set.
/// Inferred roots are supplied by the caller; this witness does not prove
/// source discovery was complete and grants no mutation.
#[derive(Debug)]
pub struct EmptyNativeTypeScriptProgram {
    root: PathBuf,
    host: Arc<super::typescript_host::TypeScriptProjectWitness>,
    program_identity: [u8; 32],
    cancellation: Arc<AtomicBool>,
    retained_bytes: usize,
}

impl EmptyNativeTypeScriptProgram {
    pub(crate) fn capture(
        host: &TypeScriptProjectHost,
        root: &Path,
        cancelled: Arc<AtomicBool>,
        budget: &RetirementBudget,
    ) -> Result<Metered<Self>, String> {
        if !EMPTY_NATIVE_COMPILER_AUTHORITY_VERIFIED {
            return Err("empty native TypeScript compiler authority is not yet proven".into());
        }
        let transient = budget
            .reserve(EMPTY_NATIVE_SCAN_RESERVATION_BYTES)
            .map_err(|e| e.to_string())?;
        let retained = budget
            .reserve(EMPTY_NATIVE_PROOF_RESERVATION_BYTES)
            .map_err(|e| e.to_string())?;
        let program = Self::capture_reserved(host, root, cancelled)?;
        drop(transient);
        Ok(retained.meter(program))
    }

    fn capture_reserved(
        host: &TypeScriptProjectHost,
        root: &Path,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let mut control = cancellation_control(&cancelled, "empty native program");
        let witness = host
            .capture_empty_native_controlled(root, &mut control)
            .map_err(|e| e.to_string())?;
        drop(control);
        if !witness.is_native_for_root(root) || !root.is_absolute() {
            return Err("empty native program root identity differs".into());
        }

        let owned_root = root.to_owned();
        let program_identity = empty_native_program_identity(
            witness
                .native_project_fingerprint()
                .ok_or("empty native host lacks the linked TSZ identity")?,
        );
        let retained_bytes = witness
            .retained_native_bytes()
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()))
            .and_then(|bytes| bytes.checked_add(owned_root.capacity()))
            // Arc control block, two boxed-slice allocation headers, and
            // conservative fixed allocation bookkeeping.
            .and_then(|bytes| bytes.checked_add(8 * std::mem::size_of::<usize>() + 256))
            .ok_or("empty native program proof size overflow")?;
        if retained_bytes > EMPTY_NATIVE_PROOF_RESERVATION_BYTES {
            return Err("empty native program proof exceeds its retained reservation".into());
        }

        // The builder's absent-config branch uses precisely the inferred roots
        // supplied by the caller. This witness supplies none; type-root
        // discovery returns immediately without a config directory, so the
        // project source set is empty. Embedded libraries are not project
        // source members. Revalidation is deferred to the grant boundary.
        Ok(Self {
            root: owned_root,
            host: witness,
            program_identity,
            cancellation: cancelled,
            retained_bytes,
        })
    }

    /// Revalidate using a temporary reservation from the caller's shared
    /// ledger. The retained proof remains metered for the entire operation.
    pub fn validate_current_budgeted(&self, budget: &RetirementBudget) -> Result<(), String> {
        let reservation = budget
            .reserve(EMPTY_NATIVE_SCAN_RESERVATION_BYTES)
            .map_err(|e| e.to_string())?;
        let result = self.validate_current_reserved();
        drop(reservation);
        result
    }

    fn validate_current_reserved(&self) -> Result<(), String> {
        let mut control =
            cancellation_control(&self.cancellation, "empty native program revalidation");
        if !self.root.is_absolute()
            || self.root.canonicalize().map_err(|e| e.to_string())? != self.root
        {
            return Err("empty native program root identity changed".into());
        }
        self.host
            .validate_empty_native_current_controlled(&mut control)
            .map_err(|e| e.to_string())?;
        if self
            .host
            .native_project_fingerprint()
            .is_none_or(|fingerprint| {
                empty_native_program_identity(fingerprint) != self.program_identity
            })
        {
            return Err("empty native program identity changed".into());
        }
        Ok(())
    }

    /// Conservative requested-allocation charge retained with the proof.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Whether the supplied operation retains the original cancellation token.
    pub fn matches_cancellation(&self, cancellation: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(&self.cancellation, cancellation)
    }

    /// Exact canonical root to which the proof is confined.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn cancellation_control<'a>(
    cancelled: &'a AtomicBool,
    operation: &'static str,
) -> impl FnMut() -> Result<(), TypeScriptProjectHostError> + 'a {
    let deadline = Instant::now() + Duration::from_secs(30);
    move || {
        if cancelled.load(Ordering::Acquire) {
            return Err(TypeScriptProjectHostError::CompilerProgramCancelled);
        }
        if Instant::now() >= deadline {
            return Err(TypeScriptProjectHostError::CompilerIoClosureMismatch {
                detail: format!("{operation} deadline exceeded").into_boxed_str(),
            });
        }
        Ok(())
    }
}

fn empty_native_program_identity(host: [u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"typescript.native-empty-inferred-program.v1\0");
    hash.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
    hash.update(&host);
    hash.update(b"no-config\0empty-inferred-roots\0");
    *hash.finalize().as_bytes()
}
