//! Defines types toolchain behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types toolchain invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;
use std::path::Path;

use backend_semantic::vocabulary::NativeTool;
use backend_version::{ContentId, ToolchainDomain};
use thiserror::Error;

use crate::application::TypeScriptProjectInvocationLease;

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
    module_closure_digest: [u8; 32],
}

/// Resolved launch state whose witness cannot be detached from its script paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResolvedInvocation<'path> {
    NativeExecutable {
        executable: &'path Path,
    },
    InterpretedScript {
        interpreter: &'path Path,
        script: &'path Path,
        module_root: &'path Path,
        witness: InterpretedScriptWitness,
        validation: InvocationValidation<'path>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationValidation<'path> {
    PerLaunch,
    ProjectPackage(&'path TypeScriptProjectInvocationLease),
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
    invocation: ResolvedInvocation<'path>,
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
    /// The script is not the TypeScript package entry under the selected module root.
    #[error("selected TypeScript script does not match its admitted module root")]
    CompilerModuleMismatch,
}

/// Exact selected executable that could not be verified before native launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInvocationFileRole {
    /// The script executed by the admitted interpreter.
    Script,
    /// The interpreter executable itself.
    Interpreter,
    /// Selected TypeScript package source and declaration closure.
    CompilerModule,
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
    /// The exact selected file or package no longer matches its admitted witness.
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
            invocation: ResolvedInvocation::NativeExecutable { executable },
        })
    }

    /// Binds a TypeScript script to its exact versioned compiler and interpreter inputs.
    pub(crate) fn from_interpreted_script(
        tool: NativeTool,
        interpreter: &'path Path,
        script: &'path Path,
        module_root: &'path Path,
        script_version_bytes: &[u8],
        interpreter_version_bytes: &[u8],
        script_file_digest: [u8; 32],
        interpreter_file_digest: [u8; 32],
        module_closure_digest: [u8; 32],
    ) -> Result<Self, ToolchainResolutionError> {
        Self::from_interpreted_identities(
            tool,
            interpreter,
            script,
            module_root,
            ContentId::<ToolchainDomain>::from_canonical_bytes(script_version_bytes),
            ContentId::<ToolchainDomain>::from_canonical_bytes(interpreter_version_bytes),
            script_file_digest,
            interpreter_file_digest,
            module_closure_digest,
        )
    }

    /// Binds exact already-admitted identities and file snapshots to a script invocation.
    pub(crate) fn from_interpreted_identities(
        tool: NativeTool,
        interpreter: &'path Path,
        script: &'path Path,
        module_root: &'path Path,
        script_identity: ContentId<ToolchainDomain>,
        interpreter_identity: ContentId<ToolchainDomain>,
        script_file_digest: [u8; 32],
        interpreter_file_digest: [u8; 32],
        module_closure_digest: [u8; 32],
    ) -> Result<Self, ToolchainResolutionError> {
        if !interpreter.is_absolute() || !script.is_absolute() || !module_root.is_absolute() {
            return Err(ToolchainResolutionError::RelativeInvocationPath);
        }
        if tool != NativeTool::TypeScriptCompiler
            || !crate::application::is_module_tsc_script(script, module_root)
        {
            return Err(ToolchainResolutionError::CompilerModuleMismatch);
        }
        Ok(Self {
            view: ResolvedToolchainView {
                tool,
                identity: script_identity,
            },
            invocation: ResolvedInvocation::InterpretedScript {
                interpreter,
                script,
                module_root,
                witness: InterpretedScriptWitness {
                    interpreter_identity,
                    interpreter_file_digest,
                    script_file_digest,
                    module_closure_digest,
                },
                validation: InvocationValidation::PerLaunch,
            },
        })
    }

    /// Binds a compiler launch to the private witness already admitted for one project package.
    /// The host rechecks its complete file-content closure at package boundaries; each spawn does
    /// only same-object checks, avoiding a second full Node/package hash for every source file.
    pub(crate) fn from_project_invocation(
        tool: NativeTool,
        interpreter: &'path Path,
        script: &'path Path,
        module_root: &'path Path,
        script_version_bytes: &[u8],
        interpreter_version_bytes: &[u8],
        lease: &'path TypeScriptProjectInvocationLease,
    ) -> Result<Self, ToolchainResolutionError> {
        if !lease.matches_invocation(script, interpreter, module_root) {
            return Err(ToolchainResolutionError::CompilerModuleMismatch);
        }
        Self::from_project_identities(
            tool,
            interpreter,
            script,
            module_root,
            ContentId::<ToolchainDomain>::from_canonical_bytes(script_version_bytes),
            ContentId::<ToolchainDomain>::from_canonical_bytes(interpreter_version_bytes),
            lease,
        )
    }

    fn from_project_identities(
        tool: NativeTool,
        interpreter: &'path Path,
        script: &'path Path,
        module_root: &'path Path,
        script_identity: ContentId<ToolchainDomain>,
        interpreter_identity: ContentId<ToolchainDomain>,
        lease: &'path TypeScriptProjectInvocationLease,
    ) -> Result<Self, ToolchainResolutionError> {
        if !interpreter.is_absolute() || !script.is_absolute() || !module_root.is_absolute() {
            return Err(ToolchainResolutionError::RelativeInvocationPath);
        }
        if tool != NativeTool::TypeScriptCompiler
            || !crate::application::is_module_tsc_script(script, module_root)
            || !lease.matches_invocation(script, interpreter, module_root)
        {
            return Err(ToolchainResolutionError::CompilerModuleMismatch);
        }
        Ok(Self {
            view: ResolvedToolchainView {
                tool,
                identity: script_identity,
            },
            invocation: ResolvedInvocation::InterpretedScript {
                interpreter,
                script,
                module_root,
                witness: InterpretedScriptWitness {
                    interpreter_identity,
                    interpreter_file_digest: lease.node_digest(),
                    script_file_digest: lease.compiler_digest(),
                    module_closure_digest: lease.module_closure_digest(),
                },
                validation: InvocationValidation::ProjectPackage(lease),
            },
        })
    }

    /// Identity bound to the exact executable(s) that will perform this compile.
    pub(crate) fn invocation_identity(self) -> ContentId<ToolchainDomain> {
        let ResolvedInvocation::InterpretedScript { witness, .. } = self.invocation else {
            return self.identity;
        };
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.native.interpreted-script-invocation.v1\0");
        identity.update(self.identity.as_ref());
        identity.update(witness.interpreter_identity.as_ref());
        identity.update(&witness.script_file_digest);
        identity.update(&witness.interpreter_file_digest);
        identity.update(&witness.module_closure_digest);
        ContentId::<ToolchainDomain>::from_canonical_bytes(identity.finalize().as_bytes())
    }

    /// Host-local identity of the exact paths selected for an interpreted invocation.
    pub(crate) fn invocation_location_identity(self) -> Option<[u8; 32]> {
        let ResolvedInvocation::InterpretedScript {
            interpreter,
            script,
            module_root,
            ..
        } = self.invocation
        else {
            return None;
        };
        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.native.interpreted-script-location.v1\0");
        for path in [script, interpreter, module_root] {
            let bytes = path.as_os_str().as_encoded_bytes();
            identity.update(&(bytes.len() as u64).to_be_bytes());
            identity.update(bytes);
        }
        Some(*identity.finalize().as_bytes())
    }

    pub(crate) const fn invocation(self) -> NativeInvocation<'path> {
        match self.invocation {
            ResolvedInvocation::NativeExecutable { executable } => {
                NativeInvocation::NativeExecutable { executable }
            }
            ResolvedInvocation::InterpretedScript {
                interpreter,
                script,
                ..
            } => NativeInvocation::InterpretedScript {
                interpreter,
                script,
            },
        }
    }

    pub(crate) const fn interpreter_identity(self) -> Option<ContentId<ToolchainDomain>> {
        match self.invocation {
            ResolvedInvocation::NativeExecutable { .. } => None,
            ResolvedInvocation::InterpretedScript { witness, .. } => {
                Some(witness.interpreter_identity)
            }
        }
    }

    /// Rechecks an interpreted launch before spawning.
    ///
    /// Standalone invocations rehash each selected input. A private project lease checks the
    /// same file/package objects here and relies on the host witness for full content revalidation
    /// at package boundaries. A concurrent in-place mutation restored before that boundary is
    /// outside this guarantee; this does not attest which bytes the operating system executed.
    pub(crate) fn validate_invocation(self) -> Result<(), NativeInvocationError> {
        let ResolvedInvocation::InterpretedScript {
            interpreter,
            script,
            module_root,
            witness,
            validation,
        } = self.invocation
        else {
            return Ok(());
        };
        if let InvocationValidation::ProjectPackage(lease) = validation {
            return lease.validate_launch_objects();
        }
        if !crate::application::is_module_tsc_script(script, module_root) {
            return Err(NativeInvocationError::Changed {
                role: NativeInvocationFileRole::CompilerModule,
                path: module_root.join("typescript").into_boxed_path(),
            });
        }
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
        let observed = crate::application::typescript_module_closure_digest(module_root).map_err(
            |source| NativeInvocationError::Inspect {
                role: NativeInvocationFileRole::CompilerModule,
                path: module_root.join("typescript").into_boxed_path(),
                source,
            },
        )?;
        if observed != witness.module_closure_digest {
            return Err(NativeInvocationError::Changed {
                role: NativeInvocationFileRole::CompilerModule,
                path: module_root.join("typescript").into_boxed_path(),
            });
        }
        Ok(())
    }

    pub(crate) fn executable(self) -> &'path Path {
        match self.invocation {
            ResolvedInvocation::NativeExecutable { executable } => executable,
            ResolvedInvocation::InterpretedScript { script, .. } => script,
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
