//! Exact path-free TypeScript authority failures. Native diagnostic bytes remain local.
use serde::{Deserialize, Serialize};

/// Exhaustive TypeScript authority cause, retaining the native call site for nested failures.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeScriptAuthorityFailureKind {
    /// TypeScript selected source path was unsafe or incompatible with its grammar profile.
    SourcePath,
    /// TypeScript checker could not be started.
    CheckerSpawn,
    /// TypeScript checker stream worker panicked.
    CheckerWorkerPanic,
    /// TypeScript checker executable is unavailable.
    CheckerToolingUnavailable,
    /// TypeScript checker started but its compiler module was unavailable.
    CheckerModuleUnavailable,
    /// TypeScript checker exited unsuccessfully.
    CheckerExit,
    /// TypeScript checker output did not satisfy its JSON protocol.
    CheckerDecode,
    /// TypeScript checker exceeded its admitted output bound.
    CheckerOutputLimit,
    /// TypeScript checker exceeded its execution timeout.
    CheckerTimeout,
    /// TypeScript checker report schema did not match the admitted version.
    CheckerStaleness,
    /// TypeScript checker report was bound to different source bytes.
    CheckerSourceBinding,
    /// TypeScript checker stream I/O failed.
    CheckerPipe,
    /// TypeScript checker private workspace I/O failed.
    CheckerWork,
    /// TypeScript package staging exceeded its file count bound.
    CheckerPackageFileLimit,
    /// TypeScript package staging exceeded its byte bound.
    CheckerPackageByteLimit,
    /// TypeScript package staging exceeded its timeout.
    CheckerPackageTimeout,
    /// TypeScript package source path failed relative path admission.
    CheckerPackageSourcePath,
    /// TypeScript selected source was absent from the staged package.
    CheckerPackageSourceMissing,
    /// TypeScript selected source changed after admission.
    CheckerPackageSourceChanged,
    /// TypeScript selected source did not match its grammar profile.
    CheckerPackageSourceProfile,
    /// TypeScript checker span did not bind to the selected source bytes.
    CheckerSpanBinding,
    /// TypeScript executable could not bind to its version identity.
    HostToolchainResolution,
    /// TypeScript package root was not absolute.
    HostRelativePackageRoot,
    /// TypeScript home boundary could not be canonicalized.
    HostHomePath,
    /// TypeScript package root could not be resolved.
    HostPackageRoot,
    /// TypeScript project uses an unsupported Yarn Plug and Play layout.
    HostYarnPnpUnsupported,
    /// TypeScript project has a compiler installation but no admitted Node executable.
    HostNodeUnavailable,
    /// TypeScript admitted Node executable failed its version probe.
    HostNodeProbe,
    /// TypeScript admitted Node executable path could not be resolved.
    HostNodePath,
    /// TypeScript explicit executable requires a matching module root.
    HostExplicitModuleRootRequired,
    /// TypeScript package path could not be read.
    HostPackagePath,
    /// TypeScript package escaped its admitted node_modules boundary.
    HostPackageEscapesNodeModules,
    /// TypeScript compiler package manifest had the wrong package name.
    HostInvalidPackageName,
    /// TypeScript compiler package manifest had an invalid version.
    HostInvalidPackageVersion,
    /// TypeScript compiler package manifest exceeded its byte bound.
    HostManifestTooLarge,
    /// TypeScript compiler package manifest could not be decoded.
    HostManifestInvalid,
    /// TypeScript workspace configuration could not be decoded.
    HostWorkspaceConfigInvalid,
    /// TypeScript project configuration could not be decoded.
    HostConfigInvalid,
    /// TypeScript selected configuration file was absent.
    HostConfigMissing,
    /// TypeScript admitted compiler API rejected its program input.
    HostCompilerApiBridge,
    /// TypeScript compiler I/O closure exceeded an admitted bound.
    HostCompilerIoClosureLimit,
    /// TypeScript compiler I/O closure did not match admitted directory state.
    HostCompilerIoClosureMismatch,
    /// TypeScript configuration graph contained a cycle.
    HostConfigCycle,
    /// TypeScript configuration escaped its admitted workspace boundary.
    HostConfigEscapesBoundary,
    /// TypeScript configuration graph exceeded its file count bound.
    HostConfigFileLimit,
    /// TypeScript configuration scan exceeded its directory depth bound.
    HostConfigDirectoryDepth,
    /// TypeScript captured file exceeded its byte bound.
    HostFileTooLarge,
    /// TypeScript captured file allocation failed.
    HostFileAllocation,
    /// TypeScript capture required a regular file.
    HostRegularFileRequired,
    /// TypeScript capture required a regular directory.
    HostRegularDirectoryRequired,
    /// TypeScript module capture encountered a disallowed symlink.
    HostModuleEntrySymlink,
    /// TypeScript compiler package exceeded its file count bound.
    HostModuleFileLimit,
    /// TypeScript project witness changed after admission.
    HostWitnessChanged,
    /// TypeScript resolved source escaped its admitted capability roots.
    HostSourceOutsideCapability,
    /// TypeScript resolved source frontier exceeded its file count bound.
    HostResolvedSourceLimit,
    /// TypeScript resolved source frontier exceeded its byte bound.
    HostResolvedSourceBytes,
    /// TypeScript resolver exceeded its observation bound.
    HostResolverObservationLimit,
    /// TypeScript resolver directory query exceeded its depth or entry bound.
    HostResolverDirectoryLimit,
    /// TypeScript resolver metadata exceeded its byte bound.
    HostResolverMetadataLimit,
    /// TypeScript resolver path was not portable UTF-8.
    HostNonPortablePath,
    /// TypeScript project witness lock was poisoned.
    HostWitnessLockPoisoned,
    /// TypeScript compiler entry escaped its admitted package.
    HostCompilerEscapesPackage,
    /// TypeScript discovered compiler shim was not an admitted package manager symlink.
    HostCompilerShimRejected,
    /// TypeScript compiler shim did not resolve to the admitted package compiler.
    HostCompilerLinkMismatch,
    /// TypeScript compiler version probe failed under the admitted Node executable.
    HostCompilerProbe,
    /// TypeScript compiler returned an invalid version result.
    HostInvalidCompilerVersion,
    /// TypeScript executable version differed from its selected module version.
    HostVersionMismatch,
    /// TypeScript checker could not be configured with the admitted Node and module root.
    HostCheckerConfiguration,
    /// Native TypeScript source path had no usable filename or extension.
    NativeSourceInvalidPath,
    /// Native TypeScript source extension was unsupported.
    NativeSourceUnsupportedExtension,
    /// Native TypeScript project contained duplicate source paths.
    NativeSourceDuplicatePath,
    /// Native TypeScript project contained no source files.
    NativeSourceEmptyProject,
    /// Native TypeScript library source was not valid UTF-8.
    NativeSourceInvalidUtf8,
    /// Native TypeScript query selected a file index absent from its program.
    NativeMissingFile,
    /// Native TypeScript selected source was absent from its program.
    NativeMissingSource,
    /// Native TypeScript retained source bytes did not match the compile lease.
    NativeSourceMismatch,
    /// Native TypeScript query session belonged to a different project.
    NativeProjectSessionMismatch,
    /// Native TypeScript checker session lacked compiler-owned module resolutions.
    NativeSessionMissingProjectModuleResolutions,
    /// Native TypeScript checker session selected a file index outside its program.
    NativeSessionFileIndexOutOfRange,
    /// Native TypeScript module resolution contained an invalid normalized path.
    NativeModuleInvalidVirtualPath,
    /// Native TypeScript module resolution contained a duplicate program path.
    NativeModuleDuplicateProgramPath,
    /// Native TypeScript module importer was absent from the admitted program.
    NativeModuleImporterNotInProgram,
    /// Native TypeScript module target was absent from the admitted program.
    NativeModuleTargetNotInProgram,
    /// Native TypeScript module request had an empty specifier.
    NativeModuleEmptySpecifier,
    /// Native TypeScript external module target had an empty identity.
    NativeModuleEmptyExternalIdentity,
    /// Native TypeScript module request syntax kind was unsupported.
    NativeModuleUnsupportedRequestKind,
    /// Native TypeScript module resolution contained a duplicate request.
    NativeModuleDuplicateRequest,
    /// Native TypeScript project checking module resolution contained an invalid normalized path.
    NativeProjectCheckModuleInvalidVirtualPath,
    /// Native TypeScript project checking module resolution contained a duplicate program path.
    NativeProjectCheckModuleDuplicateProgramPath,
    /// Native TypeScript project checking module importer was absent from the admitted program.
    NativeProjectCheckModuleImporterNotInProgram,
    /// Native TypeScript project checking module target was absent from the admitted program.
    NativeProjectCheckModuleTargetNotInProgram,
    /// Native TypeScript project checking module request had an empty specifier.
    NativeProjectCheckModuleEmptySpecifier,
    /// Native TypeScript project checking external module target had an empty identity.
    NativeProjectCheckModuleEmptyExternalIdentity,
    /// Native TypeScript project checking module request syntax kind was unsupported.
    NativeProjectCheckModuleUnsupportedRequestKind,
    /// Native TypeScript project checking module resolution contained a duplicate request.
    NativeProjectCheckModuleDuplicateRequest,
    /// Native TypeScript checker session was cancelled.
    NativeSessionCancelled,
    /// Native TypeScript checker session exceeded its admitted deadline.
    NativeSessionDeadline,
    /// Native TypeScript checker session exhausted its admitted work budget.
    NativeSessionWorkBudgetExhausted,
    /// Native TypeScript project checking was cancelled.
    NativeProjectCheckCancelled,
    /// Native TypeScript project checking exceeded its admitted deadline.
    NativeProjectCheckDeadline,
    /// Native TypeScript project checking exhausted its admitted work budget.
    NativeProjectCheckWorkBudgetExhausted,
    /// Native TypeScript project update was cancelled.
    NativeUpdateCancelled,
    /// Native TypeScript project update exceeded its admitted deadline.
    NativeUpdateDeadline,
    /// Native TypeScript project update exhausted its admitted work budget.
    NativeUpdateWorkBudgetExhausted,
}

impl TypeScriptAuthorityFailureKind {
    /// Stable, closed refusal tag.
    #[must_use]
    pub const fn kind_tag(self) -> &'static str {
        match self {
            Self::SourcePath => "typescript_source_path",
            Self::CheckerSpawn => "typescript_checker_spawn",
            Self::CheckerWorkerPanic => "typescript_checker_worker_panic",
            Self::CheckerToolingUnavailable => "typescript_checker_tooling_unavailable",
            Self::CheckerModuleUnavailable => "typescript_checker_module_unavailable",
            Self::CheckerExit => "typescript_checker_exit",
            Self::CheckerDecode => "typescript_checker_decode",
            Self::CheckerOutputLimit => "typescript_checker_output_limit",
            Self::CheckerTimeout => "typescript_checker_timeout",
            Self::CheckerStaleness => "typescript_checker_staleness",
            Self::CheckerSourceBinding => "typescript_checker_source_binding",
            Self::CheckerPipe => "typescript_checker_pipe",
            Self::CheckerWork => "typescript_checker_work",
            Self::CheckerPackageFileLimit => "typescript_checker_package_file_limit",
            Self::CheckerPackageByteLimit => "typescript_checker_package_byte_limit",
            Self::CheckerPackageTimeout => "typescript_checker_package_timeout",
            Self::CheckerPackageSourcePath => "typescript_checker_package_source_path",
            Self::CheckerPackageSourceMissing => "typescript_checker_package_source_missing",
            Self::CheckerPackageSourceChanged => "typescript_checker_package_source_changed",
            Self::CheckerPackageSourceProfile => "typescript_checker_package_source_profile",
            Self::CheckerSpanBinding => "typescript_checker_span_binding",
            Self::HostToolchainResolution => "typescript_host_toolchain_resolution",
            Self::HostRelativePackageRoot => "typescript_host_relative_package_root",
            Self::HostHomePath => "typescript_host_home_path",
            Self::HostPackageRoot => "typescript_host_package_root",
            Self::HostYarnPnpUnsupported => "typescript_host_yarn_pnp_unsupported",
            Self::HostNodeUnavailable => "typescript_host_node_unavailable",
            Self::HostNodeProbe => "typescript_host_node_probe",
            Self::HostNodePath => "typescript_host_node_path",
            Self::HostExplicitModuleRootRequired => "typescript_host_explicit_module_root_required",
            Self::HostPackagePath => "typescript_host_package_path",
            Self::HostPackageEscapesNodeModules => "typescript_host_package_escapes_node_modules",
            Self::HostInvalidPackageName => "typescript_host_invalid_package_name",
            Self::HostInvalidPackageVersion => "typescript_host_invalid_package_version",
            Self::HostManifestTooLarge => "typescript_host_manifest_too_large",
            Self::HostManifestInvalid => "typescript_host_manifest_invalid",
            Self::HostWorkspaceConfigInvalid => "typescript_host_workspace_config_invalid",
            Self::HostConfigInvalid => "typescript_host_config_invalid",
            Self::HostConfigMissing => "typescript_host_config_missing",
            Self::HostCompilerApiBridge => "typescript_host_compiler_api_bridge",
            Self::HostCompilerIoClosureLimit => "typescript_host_compiler_io_closure_limit",
            Self::HostCompilerIoClosureMismatch => "typescript_host_compiler_io_closure_mismatch",
            Self::HostConfigCycle => "typescript_host_config_cycle",
            Self::HostConfigEscapesBoundary => "typescript_host_config_escapes_boundary",
            Self::HostConfigFileLimit => "typescript_host_config_file_limit",
            Self::HostConfigDirectoryDepth => "typescript_host_config_directory_depth",
            Self::HostFileTooLarge => "typescript_host_file_too_large",
            Self::HostFileAllocation => "typescript_host_file_allocation",
            Self::HostRegularFileRequired => "typescript_host_regular_file_required",
            Self::HostRegularDirectoryRequired => "typescript_host_regular_directory_required",
            Self::HostModuleEntrySymlink => "typescript_host_module_entry_symlink",
            Self::HostModuleFileLimit => "typescript_host_module_file_limit",
            Self::HostWitnessChanged => "typescript_host_witness_changed",
            Self::HostSourceOutsideCapability => "typescript_host_source_outside_capability",
            Self::HostResolvedSourceLimit => "typescript_host_resolved_source_limit",
            Self::HostResolvedSourceBytes => "typescript_host_resolved_source_bytes",
            Self::HostResolverObservationLimit => "typescript_host_resolver_observation_limit",
            Self::HostResolverDirectoryLimit => "typescript_host_resolver_directory_limit",
            Self::HostResolverMetadataLimit => "typescript_host_resolver_metadata_limit",
            Self::HostNonPortablePath => "typescript_host_non_portable_path",
            Self::HostWitnessLockPoisoned => "typescript_host_witness_lock_poisoned",
            Self::HostCompilerEscapesPackage => "typescript_host_compiler_escapes_package",
            Self::HostCompilerShimRejected => "typescript_host_compiler_shim_rejected",
            Self::HostCompilerLinkMismatch => "typescript_host_compiler_link_mismatch",
            Self::HostCompilerProbe => "typescript_host_compiler_probe",
            Self::HostInvalidCompilerVersion => "typescript_host_invalid_compiler_version",
            Self::HostVersionMismatch => "typescript_host_version_mismatch",
            Self::HostCheckerConfiguration => "typescript_host_checker_configuration",
            Self::NativeSourceInvalidPath => "typescript_native_source_invalid_path",
            Self::NativeSourceUnsupportedExtension => {
                "typescript_native_source_unsupported_extension"
            }
            Self::NativeSourceDuplicatePath => "typescript_native_source_duplicate_path",
            Self::NativeSourceEmptyProject => "typescript_native_source_empty_project",
            Self::NativeSourceInvalidUtf8 => "typescript_native_source_invalid_utf8",
            Self::NativeMissingFile => "typescript_native_missing_file",
            Self::NativeMissingSource => "typescript_native_missing_source",
            Self::NativeSourceMismatch => "typescript_native_source_mismatch",
            Self::NativeProjectSessionMismatch => "typescript_native_project_session_mismatch",
            Self::NativeSessionMissingProjectModuleResolutions => {
                "typescript_native_session_missing_project_module_resolutions"
            }
            Self::NativeSessionFileIndexOutOfRange => {
                "typescript_native_session_file_index_out_of_range"
            }
            Self::NativeModuleInvalidVirtualPath => "typescript_native_module_invalid_virtual_path",
            Self::NativeModuleDuplicateProgramPath => {
                "typescript_native_module_duplicate_program_path"
            }
            Self::NativeModuleImporterNotInProgram => {
                "typescript_native_module_importer_not_in_program"
            }
            Self::NativeModuleTargetNotInProgram => {
                "typescript_native_module_target_not_in_program"
            }
            Self::NativeModuleEmptySpecifier => "typescript_native_module_empty_specifier",
            Self::NativeModuleEmptyExternalIdentity => {
                "typescript_native_module_empty_external_identity"
            }
            Self::NativeModuleUnsupportedRequestKind => {
                "typescript_native_module_unsupported_request_kind"
            }
            Self::NativeModuleDuplicateRequest => "typescript_native_module_duplicate_request",
            Self::NativeProjectCheckModuleInvalidVirtualPath => {
                "typescript_native_project_check_module_invalid_virtual_path"
            }
            Self::NativeProjectCheckModuleDuplicateProgramPath => {
                "typescript_native_project_check_module_duplicate_program_path"
            }
            Self::NativeProjectCheckModuleImporterNotInProgram => {
                "typescript_native_project_check_module_importer_not_in_program"
            }
            Self::NativeProjectCheckModuleTargetNotInProgram => {
                "typescript_native_project_check_module_target_not_in_program"
            }
            Self::NativeProjectCheckModuleEmptySpecifier => {
                "typescript_native_project_check_module_empty_specifier"
            }
            Self::NativeProjectCheckModuleEmptyExternalIdentity => {
                "typescript_native_project_check_module_empty_external_identity"
            }
            Self::NativeProjectCheckModuleUnsupportedRequestKind => {
                "typescript_native_project_check_module_unsupported_request_kind"
            }
            Self::NativeProjectCheckModuleDuplicateRequest => {
                "typescript_native_project_check_module_duplicate_request"
            }
            Self::NativeSessionCancelled => "typescript_native_session_cancelled",
            Self::NativeSessionDeadline => "typescript_native_session_deadline",
            Self::NativeSessionWorkBudgetExhausted => {
                "typescript_native_session_work_budget_exhausted"
            }
            Self::NativeProjectCheckCancelled => "typescript_native_project_check_cancelled",
            Self::NativeProjectCheckDeadline => "typescript_native_project_check_deadline",
            Self::NativeProjectCheckWorkBudgetExhausted => {
                "typescript_native_project_check_work_budget_exhausted"
            }
            Self::NativeUpdateCancelled => "typescript_native_update_cancelled",
            Self::NativeUpdateDeadline => "typescript_native_update_deadline",
            Self::NativeUpdateWorkBudgetExhausted => {
                "typescript_native_update_work_budget_exhausted"
            }
        }
    }

    /// Path-free explanation with no native transcript or inferred SDK cause.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::SourcePath => {
                "TypeScript selected source path was unsafe or incompatible with its grammar profile"
            }
            Self::CheckerSpawn => "TypeScript checker could not be started",
            Self::CheckerWorkerPanic => "TypeScript checker stream worker panicked",
            Self::CheckerToolingUnavailable => "TypeScript checker executable is unavailable",
            Self::CheckerModuleUnavailable => {
                "TypeScript checker started but its compiler module was unavailable"
            }
            Self::CheckerExit => "TypeScript checker exited unsuccessfully",
            Self::CheckerDecode => "TypeScript checker output did not satisfy its JSON protocol",
            Self::CheckerOutputLimit => "TypeScript checker exceeded its admitted output bound",
            Self::CheckerTimeout => "TypeScript checker exceeded its execution timeout",
            Self::CheckerStaleness => {
                "TypeScript checker report schema did not match the admitted version"
            }
            Self::CheckerSourceBinding => {
                "TypeScript checker report was bound to different source bytes"
            }
            Self::CheckerPipe => "TypeScript checker stream I/O failed",
            Self::CheckerWork => "TypeScript checker private workspace I/O failed",
            Self::CheckerPackageFileLimit => {
                "TypeScript package staging exceeded its file count bound"
            }
            Self::CheckerPackageByteLimit => "TypeScript package staging exceeded its byte bound",
            Self::CheckerPackageTimeout => "TypeScript package staging exceeded its timeout",
            Self::CheckerPackageSourcePath => {
                "TypeScript package source path failed relative path admission"
            }
            Self::CheckerPackageSourceMissing => {
                "TypeScript selected source was absent from the staged package"
            }
            Self::CheckerPackageSourceChanged => {
                "TypeScript selected source changed after admission"
            }
            Self::CheckerPackageSourceProfile => {
                "TypeScript selected source did not match its grammar profile"
            }
            Self::CheckerSpanBinding => {
                "TypeScript checker span did not bind to the selected source bytes"
            }
            Self::HostToolchainResolution => {
                "TypeScript executable could not bind to its version identity"
            }
            Self::HostRelativePackageRoot => "TypeScript package root was not absolute",
            Self::HostHomePath => "TypeScript home boundary could not be canonicalized",
            Self::HostPackageRoot => "TypeScript package root could not be resolved",
            Self::HostYarnPnpUnsupported => {
                "TypeScript project uses an unsupported Yarn Plug and Play layout"
            }
            Self::HostNodeUnavailable => {
                "TypeScript project has a compiler installation but no admitted Node executable"
            }
            Self::HostNodeProbe => "TypeScript admitted Node executable failed its version probe",
            Self::HostNodePath => "TypeScript admitted Node executable path could not be resolved",
            Self::HostExplicitModuleRootRequired => {
                "TypeScript explicit executable requires a matching module root"
            }
            Self::HostPackagePath => "TypeScript package path could not be read",
            Self::HostPackageEscapesNodeModules => {
                "TypeScript package escaped its admitted node_modules boundary"
            }
            Self::HostInvalidPackageName => {
                "TypeScript compiler package manifest had the wrong package name"
            }
            Self::HostInvalidPackageVersion => {
                "TypeScript compiler package manifest had an invalid version"
            }
            Self::HostManifestTooLarge => {
                "TypeScript compiler package manifest exceeded its byte bound"
            }
            Self::HostManifestInvalid => {
                "TypeScript compiler package manifest could not be decoded"
            }
            Self::HostWorkspaceConfigInvalid => {
                "TypeScript workspace configuration could not be decoded"
            }
            Self::HostConfigInvalid => "TypeScript project configuration could not be decoded",
            Self::HostConfigMissing => "TypeScript selected configuration file was absent",
            Self::HostCompilerApiBridge => {
                "TypeScript admitted compiler API rejected its program input"
            }
            Self::HostCompilerIoClosureLimit => {
                "TypeScript compiler I/O closure exceeded an admitted bound"
            }
            Self::HostCompilerIoClosureMismatch => {
                "TypeScript compiler I/O closure did not match admitted directory state"
            }
            Self::HostConfigCycle => "TypeScript configuration graph contained a cycle",
            Self::HostConfigEscapesBoundary => {
                "TypeScript configuration escaped its admitted workspace boundary"
            }
            Self::HostConfigFileLimit => {
                "TypeScript configuration graph exceeded its file count bound"
            }
            Self::HostConfigDirectoryDepth => {
                "TypeScript configuration scan exceeded its directory depth bound"
            }
            Self::HostFileTooLarge => "TypeScript captured file exceeded its byte bound",
            Self::HostFileAllocation => "TypeScript captured file allocation failed",
            Self::HostRegularFileRequired => "TypeScript capture required a regular file",
            Self::HostRegularDirectoryRequired => "TypeScript capture required a regular directory",
            Self::HostModuleEntrySymlink => {
                "TypeScript module capture encountered a disallowed symlink"
            }
            Self::HostModuleFileLimit => {
                "TypeScript compiler package exceeded its file count bound"
            }
            Self::HostWitnessChanged => "TypeScript project witness changed after admission",
            Self::HostSourceOutsideCapability => {
                "TypeScript resolved source escaped its admitted capability roots"
            }
            Self::HostResolvedSourceLimit => {
                "TypeScript resolved source frontier exceeded its file count bound"
            }
            Self::HostResolvedSourceBytes => {
                "TypeScript resolved source frontier exceeded its byte bound"
            }
            Self::HostResolverObservationLimit => {
                "TypeScript resolver exceeded its observation bound"
            }
            Self::HostResolverDirectoryLimit => {
                "TypeScript resolver directory query exceeded its depth or entry bound"
            }
            Self::HostResolverMetadataLimit => {
                "TypeScript resolver metadata exceeded its byte bound"
            }
            Self::HostNonPortablePath => "TypeScript resolver path was not portable UTF-8",
            Self::HostWitnessLockPoisoned => "TypeScript project witness lock was poisoned",
            Self::HostCompilerEscapesPackage => {
                "TypeScript compiler entry escaped its admitted package"
            }
            Self::HostCompilerShimRejected => {
                "TypeScript discovered compiler shim was not an admitted package manager symlink"
            }
            Self::HostCompilerLinkMismatch => {
                "TypeScript compiler shim did not resolve to the admitted package compiler"
            }
            Self::HostCompilerProbe => {
                "TypeScript compiler version probe failed under the admitted Node executable"
            }
            Self::HostInvalidCompilerVersion => {
                "TypeScript compiler returned an invalid version result"
            }
            Self::HostVersionMismatch => {
                "TypeScript executable version differed from its selected module version"
            }
            Self::HostCheckerConfiguration => {
                "TypeScript checker could not be configured with the admitted Node and module root"
            }
            Self::NativeSourceInvalidPath => {
                "Native TypeScript source path had no usable filename or extension"
            }
            Self::NativeSourceUnsupportedExtension => {
                "Native TypeScript source extension was unsupported"
            }
            Self::NativeSourceDuplicatePath => {
                "Native TypeScript project contained duplicate source paths"
            }
            Self::NativeSourceEmptyProject => "Native TypeScript project contained no source files",
            Self::NativeSourceInvalidUtf8 => "Native TypeScript library source was not valid UTF-8",
            Self::NativeMissingFile => {
                "Native TypeScript query selected a file index absent from its program"
            }
            Self::NativeMissingSource => {
                "Native TypeScript selected source was absent from its program"
            }
            Self::NativeSourceMismatch => {
                "Native TypeScript retained source bytes did not match the compile lease"
            }
            Self::NativeProjectSessionMismatch => {
                "Native TypeScript query session belonged to a different project"
            }
            Self::NativeSessionMissingProjectModuleResolutions => {
                "Native TypeScript checker session lacked compiler-owned module resolutions"
            }
            Self::NativeSessionFileIndexOutOfRange => {
                "Native TypeScript checker session selected a file index outside its program"
            }
            Self::NativeModuleInvalidVirtualPath => {
                "Native TypeScript module resolution contained an invalid normalized path"
            }
            Self::NativeModuleDuplicateProgramPath => {
                "Native TypeScript module resolution contained a duplicate program path"
            }
            Self::NativeModuleImporterNotInProgram => {
                "Native TypeScript module importer was absent from the admitted program"
            }
            Self::NativeModuleTargetNotInProgram => {
                "Native TypeScript module target was absent from the admitted program"
            }
            Self::NativeModuleEmptySpecifier => {
                "Native TypeScript module request had an empty specifier"
            }
            Self::NativeModuleEmptyExternalIdentity => {
                "Native TypeScript external module target had an empty identity"
            }
            Self::NativeModuleUnsupportedRequestKind => {
                "Native TypeScript module request syntax kind was unsupported"
            }
            Self::NativeModuleDuplicateRequest => {
                "Native TypeScript module resolution contained a duplicate request"
            }
            Self::NativeProjectCheckModuleInvalidVirtualPath => {
                "Native TypeScript project checking module resolution contained an invalid normalized path"
            }
            Self::NativeProjectCheckModuleDuplicateProgramPath => {
                "Native TypeScript project checking module resolution contained a duplicate program path"
            }
            Self::NativeProjectCheckModuleImporterNotInProgram => {
                "Native TypeScript project checking module importer was absent from the admitted program"
            }
            Self::NativeProjectCheckModuleTargetNotInProgram => {
                "Native TypeScript project checking module target was absent from the admitted program"
            }
            Self::NativeProjectCheckModuleEmptySpecifier => {
                "Native TypeScript project checking module request had an empty specifier"
            }
            Self::NativeProjectCheckModuleEmptyExternalIdentity => {
                "Native TypeScript project checking external module target had an empty identity"
            }
            Self::NativeProjectCheckModuleUnsupportedRequestKind => {
                "Native TypeScript project checking module request syntax kind was unsupported"
            }
            Self::NativeProjectCheckModuleDuplicateRequest => {
                "Native TypeScript project checking module resolution contained a duplicate request"
            }
            Self::NativeSessionCancelled => "Native TypeScript checker session was cancelled",
            Self::NativeSessionDeadline => {
                "Native TypeScript checker session exceeded its admitted deadline"
            }
            Self::NativeSessionWorkBudgetExhausted => {
                "Native TypeScript checker session exhausted its admitted work budget"
            }
            Self::NativeProjectCheckCancelled => "Native TypeScript project checking was cancelled",
            Self::NativeProjectCheckDeadline => {
                "Native TypeScript project checking exceeded its admitted deadline"
            }
            Self::NativeProjectCheckWorkBudgetExhausted => {
                "Native TypeScript project checking exhausted its admitted work budget"
            }
            Self::NativeUpdateCancelled => "Native TypeScript project update was cancelled",
            Self::NativeUpdateDeadline => {
                "Native TypeScript project update exceeded its admitted deadline"
            }
            Self::NativeUpdateWorkBudgetExhausted => {
                "Native TypeScript project update exhausted its admitted work budget"
            }
        }
    }

    /// Whether this exact terminal observed cooperative caller cancellation.
    #[must_use]
    pub const fn is_cancelled(self) -> bool {
        matches!(
            self,
            Self::NativeSessionCancelled
                | Self::NativeProjectCheckCancelled
                | Self::NativeUpdateCancelled
        )
    }
}
