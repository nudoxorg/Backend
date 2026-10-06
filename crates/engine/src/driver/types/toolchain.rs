//! Defines types toolchain behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types toolchain invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;
use std::path::Path;

use backend_semantic::vocabulary::NativeTool;
use backend_version::{ContentId, ToolchainDomain};
use thiserror::Error;

/// Exact native launch form admitted for one toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInvocation<'path> {
    /// Invoke one executable directly.
    NativeExecutable {
        /// Exact absolute executable selected by the caller.
        executable: &'path Path,
    },
    /// Invoke one selected script through its separately admitted interpreter.
    InterpretedScript {
        /// Exact absolute interpreter selected by the caller.
        interpreter: &'path Path,
        /// Exact absolute script selected by the caller.
        script: &'path Path,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InterpretedScriptWitness {
    interpreter_identity: ContentId<ToolchainDomain>,
    interpreter_file_digest: [u8; 32],
    script_file_digest: [u8; 32],
}

/// Immutable, caller-resolved native executable facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedToolchainView {
    /// Concrete tool family whose command line the caller resolved.
    pub tool: NativeTool,
    /// Central typed identity derived from caller-supplied exact version bytes.
    pub identity: ContentId<ToolchainDomain>,
}

/// Caller-resolved absolute executable capability with private execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedToolchain<'path> {
    view: ResolvedToolchainView,
    invocation: NativeInvocation<'path>,
    interpreted_script_witness: Option<InterpretedScriptWitness>,
}

impl<'path> Deref for ResolvedToolchain<'path> {
    type Target = ResolvedToolchainView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Exposes the already-validated executable path without granting a second
/// mutable or ambient tool-resolution route.
impl AsRef<Path> for ResolvedToolchain<'_> {
    fn as_ref(&self) -> &Path {
        self.executable()
    }
}

/// Rejection while binding an explicit executable to its version provenance.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolchainResolutionError {
    /// Relative executable lookup would consult ambient process search state.
    #[error("resolved executable path is not absolute")]
    RelativeExecutable,
    /// An interpreted launch is missing one of its exact absolute paths.
    #[error("resolved interpreter or script path is not absolute")]
    RelativeInvocationPath,
}

/// Exact selected executable that could not be verified before native launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInvocationFileRole {
    /// The script executed by the admitted interpreter.
    Script,
    /// The interpreter executable itself.
    Interpreter,
}

/// Failure while confirming that an interpreted native launch still matches admission.
#[derive(Debug, Error)]
pub enum NativeInvocationError {
    /// The exact selected file could not be inspected before launch.
    #[error("could not inspect selected TypeScript {role:?} at {path:?}")]
    Inspect {
        /// Which selected invocation file failed inspection.
        role: NativeInvocationFileRole,
        /// Exact absolute path selected at admission.
        path: Box<std::path::Path>,
        /// Original filesystem cause.
        #[source]
        source: std::io::Error,
    },
    /// The exact selected file's content changed after admission.
    #[error("selected TypeScript {role:?} changed after toolchain admission at {path:?}")]
    Changed {
        /// Which selected invocation file changed.
        role: NativeInvocationFileRole,
        /// Exact absolute path selected at admission.
        path: Box<std::path::Path>,
    },
}

impl<'path> ResolvedToolchain<'path> {
    /// Binds one absolute executable path to exact caller-probed tool version bytes.
    pub fn from_version(
        tool: NativeTool,
        executable: &'path Path,
        version_bytes: &[u8],
    ) -> Result<Self, ToolchainResolutionError> {
        Self::from_identity(
            tool,
            executable,
            ContentId::<ToolchainDomain>::from_canonical_bytes(version_bytes),
        )
    }

    /// Rebinds a caller-owned executable path to its already-proven exact toolchain identity.
    ///
    /// This preserves version provenance when an outer asynchronous adapter takes ownership of
    /// configuration without running a second ambient tool probe.
    pub fn from_identity(
        tool: NativeTool,
        executable: &'path Path,
        identity: ContentId<ToolchainDomain>,
    ) -> Result<Self, ToolchainResolutionError> {
        if !executable.is_absolute() {
            return Err(ToolchainResolutionError::RelativeExecutable);
        }
        Ok(Self {
            view: ResolvedToolchainView { tool, identity },
            invocation: NativeInvocation::NativeExecutable { executable },
            interpreted_script_witness: None,
        })
    }

    /// Binds a TypeScript script to its exact versioned compiler and interpreter inputs.
    pub(crate) fn from_interpreted_script(
        tool: NativeTool,
        interpreter: &'path Path,
        script: &'path Path,
        script_version_bytes: &[u8],
        interpreter_version_bytes: &[u8],
        script_file_digest: [u8; 32],
        interpreter_file_digest: [u8; 32],
    ) -> Result<Self, ToolchainResolutionError> {
        Self::from_interpreted_identities(
            tool,
            interpreter,
            script,
            ContentId::<ToolchainDomain>::from_canonical_bytes(script_version_bytes),
            ContentId::<ToolchainDomain>::from_canonical_bytes(interpreter_version_bytes),
            script_file_digest,
            interpreter_file_digest,
        )
    }

    /// Binds exact already-admitted identities and file snapshots to a script invocation.
    pub(crate) fn from_interpreted_identities(
        tool: NativeTool,
        interpreter: &'path Path,
        script: &'path Path,
        script_identity: ContentId<ToolchainDomain>,
        interpreter_identity: ContentId<ToolchainDomain>,
        script_file_digest: [u8; 32],
        interpreter_file_digest: [u8; 32],
    ) -> Result<Self, ToolchainResolutionError> {
        if !interpreter.is_absolute() || !script.is_absolute() {
            return Err(ToolchainResolutionError::RelativeInvocationPath);
        }
        Ok(Self {
            view: ResolvedToolchainView {
                tool,
                identity: script_identity,
            },
            invocation: NativeInvocation::InterpretedScript {
                interpreter,
                script,
            },
            interpreted_script_witness: Some(InterpretedScriptWitness {
                interpreter_identity,
                interpreter_file_digest,
                script_file_digest,
            }),
        })
    }

    /// Identity bound to the exact executable(s) that will perform this compile.
    pub(crate) fn invocation_identity(self) -> ContentId<ToolchainDomain> {
        let Some(witness) = self.interpreted_script_witness else {
            return self.identity;
        };
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.native.interpreted-script-invocation.v1\0");
        identity.update(self.identity.as_ref());
        identity.update(witness.interpreter_identity.as_ref());
        identity.update(&witness.script_file_digest);
        identity.update(&witness.interpreter_file_digest);
        ContentId::<ToolchainDomain>::from_canonical_bytes(identity.finalize().as_bytes())
    }

    /// Host-local identity of the exact paths selected for an interpreted invocation.
    pub(crate) fn invocation_location_identity(self) -> Option<[u8; 32]> {
        let NativeInvocation::InterpretedScript {
            interpreter,
            script,
        } = self.invocation
        else {
            return None;
        };
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.native.interpreted-script-location.v1\0");
        for path in [script, interpreter] {
            let bytes = path.as_os_str().as_encoded_bytes();
            identity.update(&(bytes.len() as u64).to_be_bytes());
            identity.update(bytes);
        }
        Some(*identity.finalize().as_bytes())
    }

    pub(crate) const fn invocation(self) -> NativeInvocation<'path> {
        self.invocation
    }

    pub(crate) const fn interpreter_identity(self) -> Option<ContentId<ToolchainDomain>> {
        match self.interpreted_script_witness {
            Some(witness) => Some(witness.interpreter_identity),
            None => None,
        }
    }

    /// Rechecks every file in an interpreted launch against its admission digest.
    pub(crate) fn validate_invocation(self) -> Result<(), NativeInvocationError> {
        let NativeInvocation::InterpretedScript {
            interpreter,
            script,
        } = self.invocation
        else {
            return Ok(());
        };
        let witness = self
            .interpreted_script_witness
            .expect("interpreted launch carries its admitted file digests");
        for (role, path, expected) in [
            (
                NativeInvocationFileRole::Script,
                script,
                witness.script_file_digest,
            ),
            (
                NativeInvocationFileRole::Interpreter,
                interpreter,
                witness.interpreter_file_digest,
            ),
        ] {
            let observed =
                crate::application::executable_content_digest(path).map_err(|source| {
                    NativeInvocationError::Inspect {
                        role,
                        path: path.to_path_buf().into_boxed_path(),
                        source,
                    }
                })?;
            if observed != expected {
                return Err(NativeInvocationError::Changed {
                    role,
                    path: path.to_path_buf().into_boxed_path(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn executable(self) -> &'path Path {
        match self.invocation {
            NativeInvocation::NativeExecutable { executable } => executable,
            NativeInvocation::InterpretedScript { script, .. } => script,
        }
    }
}

/// Closed toolchain state supplied to a compilation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainSelection<'path> {
    /// A validated absolute executable may service one locally reproducible native adapter.
    ResolvedNative(ResolvedToolchain<'path>),
    /// This language's selected native tool is intentionally unavailable without a forged path.
    ExplicitlyUnavailable {
        /// Exact native tool family the caller cannot provide.
        tool: NativeTool,
    },
}

/// Exact shape of a supplied toolchain selection retained by mismatch diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainSelectionFact {
    /// Request supplied a resolved executable for this concrete tool family.
    ResolvedNative {
        /// Native tool family identified by the supplied executable selection.
        tool: NativeTool,
    },
    /// Request supplied an explicit unavailable terminal for this concrete tool family.
    ExplicitlyUnavailable {
        /// Native tool family named by the explicit unavailable terminal.
        tool: NativeTool,
    },
}

impl<'path> ToolchainSelection<'path> {
    pub(super) fn fact(self) -> ToolchainSelectionFact {
        match self {
            Self::ResolvedNative(resolved) => ToolchainSelectionFact::ResolvedNative {
                tool: resolved.tool,
            },
            Self::ExplicitlyUnavailable { tool } => {
                ToolchainSelectionFact::ExplicitlyUnavailable { tool }
            }
        }
    }
}
