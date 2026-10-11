//! Closed host-control terminals must survive process wrappers without entering a compiler.
use super::{package_authority_terminal, request_source};
use crate::application::toolchain_probe::{
    ToolchainProbeCleanupAction, ToolchainProbeError, ToolchainProbePrimary,
    ToolchainProbeStreamError,
};
use crate::application::{PackageAuthorityError, TypeScriptProjectHostError};
use crate::driver::ToolchainSelection;
use backend_library::interface::{
    CompilerRequest, CompilerTerminal, PackageCompilePhase, TypeScriptAuthorityFailureKind,
};
use backend_semantic::vocabulary::{
    LanguageProfile, NativeTool, NativeWorker, Stage, TypeScriptSource,
};
use backend_version::{CompilationTargetDomain, ContentId};
use std::{io, time::Duration};

fn bounded(cancelled: bool) -> ToolchainProbeError {
    ToolchainProbeError::Bounded {
        tool: NativeTool::TypeScriptCompiler,
        primary: if cancelled {
            ToolchainProbePrimary::Cancelled
        } else {
            ToolchainProbePrimary::Deadline {
                timeout: Duration::from_secs(3),
            }
        },
    }
}
fn cleanup(cancelled: bool) -> ToolchainProbeError {
    ToolchainProbeError::Cleanup {
        tool: NativeTool::TypeScriptCompiler,
        action: ToolchainProbeCleanupAction::Reap,
        preceding: Box::new(bounded(cancelled)),
        source: io::Error::other("retained cleanup cause"),
    }
}
fn stream(worker: NativeWorker) -> ToolchainProbeStreamError {
    ToolchainProbeStreamError::Read {
        worker,
        source: io::Error::other("retained stream cause"),
    }
}

#[test]
fn host_program_control_preserves_exact_cancelled_and_deadline_package_terminals() {
    let request = CompilerRequest {
        profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        stage: Stage::LowerIr,
        source: "export const x = 1;",
    };
    let source = request_source(request).unwrap();
    let target =
        ContentId::<CompilationTargetDomain>::from_canonical_bytes(b"host-program-control");
    let toolchain = ToolchainSelection::ExplicitlyUnavailable {
        tool: NativeTool::TypeScriptCompiler,
    };
    for cancelled in [true, false] {
        let direct = if cancelled {
            TypeScriptProjectHostError::CompilerProgramCancelled
        } else {
            TypeScriptProjectHostError::CompilerProgramDeadline
        };
        let errors = [
            direct,
            TypeScriptProjectHostError::CompilerProgramProbe {
                source: bounded(cancelled),
            },
            TypeScriptProjectHostError::CompilerProgramProbe {
                source: cleanup(cancelled),
            },
            TypeScriptProjectHostError::CompilerProgramProbe {
                source: ToolchainProbeError::Stream {
                    tool: NativeTool::TypeScriptCompiler,
                    preceding: Some(Box::new(cleanup(cancelled))),
                    source: stream(NativeWorker::StandardOutputReader),
                },
            },
            TypeScriptProjectHostError::CompilerProgramProbe {
                source: ToolchainProbeError::Streams {
                    tool: NativeTool::TypeScriptCompiler,
                    preceding: Some(Box::new(cleanup(cancelled))),
                    stdout: stream(NativeWorker::StandardOutputReader),
                    stderr: stream(NativeWorker::StandardErrorReader),
                },
            },
        ];
        for error in errors {
            let terminal = package_authority_terminal(
                target,
                request,
                source,
                toolchain,
                PackageAuthorityError::TypeScriptProjectHost(error),
            );
            let projected = backend_library::PackageCompilerFailure::from_package_terminal(
                "entry.ts", &terminal,
            )
            .unwrap();
            if cancelled {
                assert!(
                    matches!(terminal, CompilerTerminal::PackageCancelled {target: actual, phase: PackageCompilePhase::Authority} if actual==target)
                );
                assert!(
                    projected.is_none(),
                    "host cooperative cancellation cannot become CompilerRefused"
                );
            } else {
                let failure = projected.expect("a deadline remains an exact typed refusal");
                assert_eq!(
                    failure.kind_tag(),
                    TypeScriptAuthorityFailureKind::HostProgramDeadline.kind_tag()
                );
                assert!(
                    failure
                        .detail()
                        .contains("program admission exceeded its admitted deadline")
                );
                assert!(!failure.cause().requires_tool_configuration());
            }
        }
    }
}

#[test]
fn compiler_api_probe_retains_cleanup_source_chain_and_path_free_control_codec() {
    use std::error::Error;
    let error = TypeScriptProjectHostError::CompilerProgramProbe {
        source: cleanup(false),
    };
    let retained = error.source().expect("original probe error");
    assert!(retained.to_string().contains("Reap"));
    assert_eq!(
        retained.source().unwrap().to_string(),
        "retained cleanup cause"
    );
    for failure in [
        TypeScriptAuthorityFailureKind::HostProgramCancelled,
        TypeScriptAuthorityFailureKind::HostProgramDeadline,
    ] {
        let bytes = serde_json::to_vec(&failure).unwrap();
        assert_eq!(
            serde_json::from_slice::<TypeScriptAuthorityFailureKind>(&bytes).unwrap(),
            failure
        );
        assert_eq!(
            failure.is_cancelled(),
            failure == TypeScriptAuthorityFailureKind::HostProgramCancelled
        );
        assert!(!String::from_utf8(bytes).unwrap().contains("retained"));
    }
}
