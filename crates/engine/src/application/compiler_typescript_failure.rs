//! Exhaustive source-enum projection; never classify stderr, paths or error text.
use super::package_authority::PackageAuthorityError;
use super::typescript_host::TypeScriptProjectHostError;
use backend_frontend_typescript::legacy::CheckerError;
use backend_frontend_typescript::{
    TszAuthorityError, TszProjectExecutionStop, TszProjectModuleResolutionError,
    TszProjectProgramCheckError, TszProjectQuerySessionError, TszSourceError,
};
use backend_library::interface::TypeScriptAuthorityFailureKind as F;

pub(super) fn failure_kind(error: &PackageAuthorityError) -> Option<F> {
    match error {
        PackageAuthorityError::TypeScriptSourcePath { .. } => Some(F::SourcePath),
        PackageAuthorityError::TypeScript(error) => Some(checker_failure(error)),
        PackageAuthorityError::TypeScriptProjectHost(error) => Some(host_failure(error)),
        PackageAuthorityError::TypeScriptTsz(error) => Some(native_failure(error)),
        _ => None,
    }
}

fn checker_failure(error: &CheckerError) -> F {
    match error {
        CheckerError::Spawn { .. } => F::CheckerSpawn,
        CheckerError::WorkerPanic { .. } => F::CheckerWorkerPanic,
        CheckerError::ToolingUnavailable { .. } => F::CheckerToolingUnavailable,
        CheckerError::ModuleUnavailable { .. } => F::CheckerModuleUnavailable,
        CheckerError::Exit { .. } => F::CheckerExit,
        CheckerError::Decode { .. } => F::CheckerDecode,
        CheckerError::OutputLimit { .. } => F::CheckerOutputLimit,
        CheckerError::Timeout { .. } => F::CheckerTimeout,
        CheckerError::Staleness { .. } => F::CheckerStaleness,
        CheckerError::SourceBinding { .. } => F::CheckerSourceBinding,
        CheckerError::Pipe { .. } => F::CheckerPipe,
        CheckerError::Work { .. } => F::CheckerWork,
        CheckerError::PackageFileLimit { .. } => F::CheckerPackageFileLimit,
        CheckerError::PackageByteLimit { .. } => F::CheckerPackageByteLimit,
        CheckerError::PackageTimeout { .. } => F::CheckerPackageTimeout,
        CheckerError::PackageSourcePath { .. } => F::CheckerPackageSourcePath,
        CheckerError::PackageSourceMissing { .. } => F::CheckerPackageSourceMissing,
        CheckerError::PackageSourceChanged { .. } => F::CheckerPackageSourceChanged,
        CheckerError::PackageSourceProfile { .. } => F::CheckerPackageSourceProfile,
        CheckerError::SpanBinding { .. } => F::CheckerSpanBinding,
    }
}

fn host_failure(error: &TypeScriptProjectHostError) -> F {
    match error {
        TypeScriptProjectHostError::BundledSdkAdmission { .. } => F::HostToolchainResolution,
        TypeScriptProjectHostError::ToolchainResolution { .. } => F::HostToolchainResolution,
        TypeScriptProjectHostError::RelativePackageRoot { .. } => F::HostRelativePackageRoot,
        TypeScriptProjectHostError::HomePath { .. } => F::HostHomePath,
        TypeScriptProjectHostError::PackageRoot { .. } => F::HostPackageRoot,
        TypeScriptProjectHostError::YarnPnpUnsupported { .. } => F::HostYarnPnpUnsupported,
        TypeScriptProjectHostError::NodeUnavailable { .. } => F::HostNodeUnavailable,
        TypeScriptProjectHostError::NodeProbe { .. } => F::HostNodeProbe,
        TypeScriptProjectHostError::NodePath { .. } => F::HostNodePath,
        TypeScriptProjectHostError::ExplicitModuleRootRequired { .. } => {
            F::HostExplicitModuleRootRequired
        }
        TypeScriptProjectHostError::PackagePath { .. } => F::HostPackagePath,
        TypeScriptProjectHostError::PackageEscapesNodeModules { .. } => {
            F::HostPackageEscapesNodeModules
        }
        TypeScriptProjectHostError::InvalidPackageName { .. } => F::HostInvalidPackageName,
        TypeScriptProjectHostError::InvalidPackageVersion { .. } => F::HostInvalidPackageVersion,
        TypeScriptProjectHostError::ManifestTooLarge { .. } => F::HostManifestTooLarge,
        TypeScriptProjectHostError::ManifestInvalid { .. } => F::HostManifestInvalid,
        TypeScriptProjectHostError::WorkspaceConfigInvalid { .. } => F::HostWorkspaceConfigInvalid,
        TypeScriptProjectHostError::ConfigInvalid { .. } => F::HostConfigInvalid,
        TypeScriptProjectHostError::ConfigMissing { .. } => F::HostConfigMissing,
        TypeScriptProjectHostError::CompilerApiBridge { .. } => F::HostCompilerApiBridge,
        TypeScriptProjectHostError::CompilerIoClosureLimit { .. } => F::HostCompilerIoClosureLimit,
        TypeScriptProjectHostError::CompilerIoClosureMismatch { .. } => {
            F::HostCompilerIoClosureMismatch
        }
        TypeScriptProjectHostError::ConfigCycle { .. } => F::HostConfigCycle,
        TypeScriptProjectHostError::ConfigEscapesBoundary { .. } => F::HostConfigEscapesBoundary,
        TypeScriptProjectHostError::ConfigFileLimit { .. } => F::HostConfigFileLimit,
        TypeScriptProjectHostError::ConfigDirectoryDepth { .. } => F::HostConfigDirectoryDepth,
        TypeScriptProjectHostError::FileTooLarge { .. } => F::HostFileTooLarge,
        TypeScriptProjectHostError::FileAllocation { .. } => F::HostFileAllocation,
        TypeScriptProjectHostError::RegularFileRequired { .. } => F::HostRegularFileRequired,
        TypeScriptProjectHostError::RegularDirectoryRequired { .. } => {
            F::HostRegularDirectoryRequired
        }
        TypeScriptProjectHostError::ModuleEntrySymlink { .. } => F::HostModuleEntrySymlink,
        TypeScriptProjectHostError::ModuleFileLimit { .. } => F::HostModuleFileLimit,
        TypeScriptProjectHostError::WitnessChanged { .. } => F::HostWitnessChanged,
        TypeScriptProjectHostError::SourceOutsideCapability { .. } => {
            F::HostSourceOutsideCapability
        }
        TypeScriptProjectHostError::ResolvedSourceLimit { .. } => F::HostResolvedSourceLimit,
        TypeScriptProjectHostError::ResolvedSourceBytes { .. } => F::HostResolvedSourceBytes,
        TypeScriptProjectHostError::ResolverObservationLimit { .. } => {
            F::HostResolverObservationLimit
        }
        TypeScriptProjectHostError::ResolverDirectoryLimit { .. } => F::HostResolverDirectoryLimit,
        TypeScriptProjectHostError::ResolverMetadataLimit { .. } => F::HostResolverMetadataLimit,
        TypeScriptProjectHostError::NonPortablePath { .. } => F::HostNonPortablePath,
        TypeScriptProjectHostError::WitnessLockPoisoned { .. } => F::HostWitnessLockPoisoned,
        TypeScriptProjectHostError::CompilerEscapesPackage { .. } => F::HostCompilerEscapesPackage,
        TypeScriptProjectHostError::CompilerShimRejected { .. } => F::HostCompilerShimRejected,
        TypeScriptProjectHostError::CompilerLinkMismatch { .. } => F::HostCompilerLinkMismatch,
        TypeScriptProjectHostError::CompilerProbe { .. } => F::HostCompilerProbe,
        TypeScriptProjectHostError::InvalidCompilerVersion { .. } => F::HostInvalidCompilerVersion,
        TypeScriptProjectHostError::VersionMismatch { .. } => F::HostVersionMismatch,
        TypeScriptProjectHostError::CheckerConfiguration { .. } => F::HostCheckerConfiguration,
    }
}

fn native_failure(error: &TszAuthorityError) -> F {
    match error {
        TszAuthorityError::Source(error) => match error {
            TszSourceError::InvalidPath => F::NativeSourceInvalidPath,
            TszSourceError::UnsupportedExtension => F::NativeSourceUnsupportedExtension,
            TszSourceError::DuplicatePath => F::NativeSourceDuplicatePath,
            TszSourceError::EmptyProject => F::NativeSourceEmptyProject,
            TszSourceError::InvalidUtf8 => F::NativeSourceInvalidUtf8,
        },
        TszAuthorityError::MissingFile(_) => F::NativeMissingFile,
        TszAuthorityError::MissingSource(_) => F::NativeMissingSource,
        TszAuthorityError::SourceMismatch { .. } => F::NativeSourceMismatch,
        TszAuthorityError::ProjectSessionMismatch => F::NativeProjectSessionMismatch,
        TszAuthorityError::ModuleResolution(error) => module_failure(error, ModuleSite::Admission),
        TszAuthorityError::ProjectCheckerSession(error) => match error {
            TszProjectQuerySessionError::MissingProjectModuleResolutions => {
                F::NativeSessionMissingProjectModuleResolutions
            }
            TszProjectQuerySessionError::FileIndexOutOfRange { .. } => {
                F::NativeSessionFileIndexOutOfRange
            }
            TszProjectQuerySessionError::ExecutionStopped(reason) => {
                stop_failure(*reason, StopSite::Session)
            }
        },
        TszAuthorityError::ProjectCheck(error) => match error {
            TszProjectProgramCheckError::ModuleResolution(error) => {
                module_failure(error, ModuleSite::ProjectCheck)
            }
            TszProjectProgramCheckError::ExecutionStopped(reason) => {
                stop_failure(*reason, StopSite::ProjectCheck)
            }
        },
        TszAuthorityError::ExecutionStopped(reason) => stop_failure(*reason, StopSite::Update),
    }
}

#[derive(Clone, Copy)]
enum ModuleSite {
    Admission,
    ProjectCheck,
}
fn module_failure(error: &TszProjectModuleResolutionError, site: ModuleSite) -> F {
    use TszProjectModuleResolutionError as E;
    match (site, error) {
        (ModuleSite::Admission, E::InvalidVirtualPath { .. }) => F::NativeModuleInvalidVirtualPath,
        (ModuleSite::Admission, E::DuplicateProgramPath { .. }) => {
            F::NativeModuleDuplicateProgramPath
        }
        (ModuleSite::Admission, E::ImporterNotInProgram { .. }) => {
            F::NativeModuleImporterNotInProgram
        }
        (ModuleSite::Admission, E::TargetNotInProgram { .. }) => F::NativeModuleTargetNotInProgram,
        (ModuleSite::Admission, E::EmptySpecifier) => F::NativeModuleEmptySpecifier,
        (ModuleSite::Admission, E::EmptyExternalIdentity) => F::NativeModuleEmptyExternalIdentity,
        (ModuleSite::Admission, E::UnsupportedRequestKind { .. }) => {
            F::NativeModuleUnsupportedRequestKind
        }
        (ModuleSite::Admission, E::DuplicateRequest { .. }) => F::NativeModuleDuplicateRequest,
        (ModuleSite::ProjectCheck, E::InvalidVirtualPath { .. }) => {
            F::NativeProjectCheckModuleInvalidVirtualPath
        }
        (ModuleSite::ProjectCheck, E::DuplicateProgramPath { .. }) => {
            F::NativeProjectCheckModuleDuplicateProgramPath
        }
        (ModuleSite::ProjectCheck, E::ImporterNotInProgram { .. }) => {
            F::NativeProjectCheckModuleImporterNotInProgram
        }
        (ModuleSite::ProjectCheck, E::TargetNotInProgram { .. }) => {
            F::NativeProjectCheckModuleTargetNotInProgram
        }
        (ModuleSite::ProjectCheck, E::EmptySpecifier) => F::NativeProjectCheckModuleEmptySpecifier,
        (ModuleSite::ProjectCheck, E::EmptyExternalIdentity) => {
            F::NativeProjectCheckModuleEmptyExternalIdentity
        }
        (ModuleSite::ProjectCheck, E::UnsupportedRequestKind { .. }) => {
            F::NativeProjectCheckModuleUnsupportedRequestKind
        }
        (ModuleSite::ProjectCheck, E::DuplicateRequest { .. }) => {
            F::NativeProjectCheckModuleDuplicateRequest
        }
    }
}

#[derive(Clone, Copy)]
enum StopSite {
    Session,
    ProjectCheck,
    Update,
}
fn stop_failure(reason: TszProjectExecutionStop, site: StopSite) -> F {
    match (site, reason) {
        (StopSite::Session, TszProjectExecutionStop::Cancelled) => F::NativeSessionCancelled,
        (StopSite::Session, TszProjectExecutionStop::Deadline) => F::NativeSessionDeadline,
        (StopSite::Session, TszProjectExecutionStop::WorkBudgetExhausted) => {
            F::NativeSessionWorkBudgetExhausted
        }
        (StopSite::ProjectCheck, TszProjectExecutionStop::Cancelled) => {
            F::NativeProjectCheckCancelled
        }
        (StopSite::ProjectCheck, TszProjectExecutionStop::Deadline) => {
            F::NativeProjectCheckDeadline
        }
        (StopSite::ProjectCheck, TszProjectExecutionStop::WorkBudgetExhausted) => {
            F::NativeProjectCheckWorkBudgetExhausted
        }
        (StopSite::Update, TszProjectExecutionStop::Cancelled) => F::NativeUpdateCancelled,
        (StopSite::Update, TszProjectExecutionStop::Deadline) => F::NativeUpdateDeadline,
        (StopSite::Update, TszProjectExecutionStop::WorkBudgetExhausted) => {
            F::NativeUpdateWorkBudgetExhausted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typescript_failure_retains_native_stop_site_and_reason() {
        for (site, expected) in [
            (
                StopSite::Session,
                [
                    F::NativeSessionCancelled,
                    F::NativeSessionDeadline,
                    F::NativeSessionWorkBudgetExhausted,
                ],
            ),
            (
                StopSite::ProjectCheck,
                [
                    F::NativeProjectCheckCancelled,
                    F::NativeProjectCheckDeadline,
                    F::NativeProjectCheckWorkBudgetExhausted,
                ],
            ),
            (
                StopSite::Update,
                [
                    F::NativeUpdateCancelled,
                    F::NativeUpdateDeadline,
                    F::NativeUpdateWorkBudgetExhausted,
                ],
            ),
        ] {
            for (reason, expected) in [
                TszProjectExecutionStop::Cancelled,
                TszProjectExecutionStop::Deadline,
                TszProjectExecutionStop::WorkBudgetExhausted,
            ]
            .into_iter()
            .zip(expected)
            {
                assert_eq!(stop_failure(reason, site), expected);
                assert_eq!(
                    expected.is_cancelled(),
                    reason == TszProjectExecutionStop::Cancelled
                );
            }
        }
        let error = PackageAuthorityError::TypeScriptTsz(TszAuthorityError::ProjectCheck(
            TszProjectProgramCheckError::ExecutionStopped(TszProjectExecutionStop::Deadline),
        ));
        assert_eq!(failure_kind(&error), Some(F::NativeProjectCheckDeadline));
        let error = PackageAuthorityError::TypeScriptTsz(TszAuthorityError::ProjectCheck(
            TszProjectProgramCheckError::ModuleResolution(
                TszProjectModuleResolutionError::TargetNotInProgram {
                    path: "/private/secret.ts".into(),
                },
            ),
        ));
        assert_eq!(
            failure_kind(&error),
            Some(F::NativeProjectCheckModuleTargetNotInProgram)
        );
    }

    #[test]
    fn typescript_failure_distinguishes_host_bridge_staging_and_checker_exit() {
        let cases = [
            (
                PackageAuthorityError::TypeScriptProjectHost(
                    TypeScriptProjectHostError::BundledSdkAdmission {
                        path: std::path::Path::new("/private/sdk/packaging-manifest.json").into(),
                        message: "private receipt mismatch".into(),
                    },
                ),
                F::HostToolchainResolution,
            ),
            (
                PackageAuthorityError::TypeScriptProjectHost(
                    TypeScriptProjectHostError::CompilerApiBridge {
                        message: "private transcript".into(),
                    },
                ),
                F::HostCompilerApiBridge,
            ),
            (
                PackageAuthorityError::TypeScriptProjectHost(
                    TypeScriptProjectHostError::CompilerIoClosureMismatch {
                        detail: "private path".into(),
                    },
                ),
                F::HostCompilerIoClosureMismatch,
            ),
            (
                PackageAuthorityError::TypeScript(CheckerError::PackageSourceMissing {
                    path: "/private/secret.ts".into(),
                }),
                F::CheckerPackageSourceMissing,
            ),
            (
                PackageAuthorityError::TypeScript(CheckerError::Exit {
                    status: "exit status: 1".into(),
                    stderr: "private transcript".into(),
                }),
                F::CheckerExit,
            ),
            (
                PackageAuthorityError::TypeScript(CheckerError::Decode {
                    message: "private message".into(),
                    transcript: "private transcript".into(),
                }),
                F::CheckerDecode,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(failure_kind(&error), Some(expected));
            assert!(!expected.detail().contains("private"));
            assert!(!expected.detail().contains("not configured"));
        }
    }
}
