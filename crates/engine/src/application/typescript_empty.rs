//! Bounded compiler-owned proof for a single empty configured native TS program.
//!
//! This private evidence is not an inventory-completeness claim. The owner
//! must separately bind the exact prior project and selected publication, and
//! revalidate this witness at the eventual durable grant.
use super::typescript_host::{
    AdmittedTypeScriptProject, TypeScriptProjectHost, TypeScriptProjectHostError,
};
use super::typescript_program::{TypeScriptForestClosureWitness, build_native_inputs};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// A freshly admitted native TS program with no authored or external source
/// members, under one exact project configuration. It grants no mutation.
#[derive(Debug)]
pub struct EmptyNativeTypeScriptProgram {
    root: PathBuf,
    host: Arc<super::typescript_host::TypeScriptProjectWitness>,
    closure: TypeScriptForestClosureWitness,
    cancellation: Arc<AtomicBool>,
    retained_bytes: usize,
}

impl EmptyNativeTypeScriptProgram {
    pub(crate) fn capture(
        host: &TypeScriptProjectHost,
        root: &Path,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut control = || {
            if cancelled.load(Ordering::Acquire) {
                return Err(TypeScriptProjectHostError::CompilerProgramCancelled);
            }
            if Instant::now() >= deadline {
                return Err(TypeScriptProjectHostError::CompilerIoClosureMismatch {
                    detail: "empty native program deadline exceeded".into(),
                });
            }
            Ok(())
        };
        let admitted = host
            .admit_controlled(root, &mut control)
            .map_err(|e| e.to_string())?
            .ok_or("no native TypeScript host admitted")?;
        let AdmittedTypeScriptProject::Native { witness, .. } = &admitted else {
            return Err("empty TypeScript retirement requires native TSZ authority".into());
        };
        let inputs = admitted.inputs();
        if inputs.package_root != root
            || !root.is_absolute()
            || root.canonicalize().map_err(|e| e.to_string())? != root
        {
            return Err("empty native program root identity differs".into());
        }
        let selected: Vec<_> = inputs
            .config_candidates
            .iter()
            .filter(|config| config.project_candidate)
            .collect();
        if selected.len() != 1
            || selected[0].path.as_ref() != root.join("tsconfig.json")
            || !selected[0].references.is_empty()
            || inputs.selected_build_config_paths.len() > 1
        {
            return Err(
                "empty native program requires exactly one root tsconfig without references".into(),
            );
        }
        let mut resolver = inputs.resolver();
        let program =
            build_native_inputs(&inputs, &mut resolver, &[], deadline, cancelled.as_ref())
                .map_err(|e| e.to_string())?;
        if !program.sources.is_empty()
            || program.solution
            || program
                .options
                .program_diagnostics
                .iter()
                .any(|diagnostic| !matches!(diagnostic.code, 18002 | 18003))
        {
            return Err("native program is not a successful empty configured scope".into());
        }
        let closure = TypeScriptForestClosureWitness(vec![program.closure_witness]);
        let owned_root = root.to_owned();
        let bytes = witness
            .retained_native_bytes()
            .and_then(|bytes| bytes.checked_add(closure.retained_bytes()))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()))
            .and_then(|bytes| bytes.checked_add(owned_root.capacity()))
            .and_then(|bytes| bytes.checked_add(6 * std::mem::size_of::<usize>()))
            .ok_or("empty native program proof size overflow")?;
        if bytes > backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES {
            return Err("empty native program proof exceeds bound".into());
        }
        closure
            .validate_current_controlled(witness, &mut control)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            root: owned_root,
            host: Arc::clone(witness),
            closure,
            cancellation: cancelled,
            retained_bytes: bytes,
        })
    }

    /// Revalidate the admitted configuration and compiler directory closure.
    pub fn validate_current(&self) -> Result<(), String> {
        let cancelled = self.cancellation.as_ref();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut control = || {
            if cancelled.load(Ordering::Acquire) {
                return Err(TypeScriptProjectHostError::CompilerProgramCancelled);
            }
            if Instant::now() >= deadline {
                return Err(TypeScriptProjectHostError::CompilerIoClosureMismatch {
                    detail: "empty native program revalidation deadline exceeded".into(),
                });
            }
            Ok(())
        };
        if !self.root.is_absolute()
            || self.root.canonicalize().map_err(|e| e.to_string())? != self.root
        {
            return Err("empty native program root identity changed".into());
        }
        self.closure
            .validate_current_controlled(&self.host, &mut control)
            .map_err(|e| e.to_string())
    }

    /// Conservative memory charge for a queued owner transition.
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
