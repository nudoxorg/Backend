//! Exhaustive source-enum projection; never classify stderr, paths or error text.
use super::package_authority::PackageAuthorityError;
use super::typescript_host::TypeScriptProjectHostError;
use backend_frontend_typescript::legacy::CheckerError;
use backend_frontend_typescript::{
    TszAuthorityError, TszProjectExecutionStop, TszProjectModuleResolutionError,
    TszProjectProgramCheckError, TszProjectQuerySessionError, TszSourceError,
};
use backend_library::interface::TypeScriptAuthorityFailureKind as F;
use backend_library::interface::TypeScriptFailureCause;
use backend_library::interface::{
    TypeScriptCompilerAdmissionCause as C, TypeScriptCompilerAdmissionDetail,
    TypeScriptCompilerApiExport, TypeScriptCompilerPackageRole as R, TypeScriptCompilerVersion,
};
use std::path::Path;

pub(super) fn admission_detail(
    error: &PackageAuthorityError,
) -> Option<TypeScriptCompilerAdmissionDetail> {
    let PackageAuthorityError::TypeScriptProjectHost(error) = error else {
        return None;
    };
    let error = peel(error);
    let (error, role, version) = match error {
        TypeScriptProjectHostError::CompilerPackageAdmission {
            source,
            role,
            package_version,
        } => (
            source.as_ref(),
            *role,
            TypeScriptCompilerVersion::new(package_version),
        ),
        TypeScriptProjectHostError::CompilerApiEntryMissing {
            package_version, ..
        }
        | TypeScriptProjectHostError::CompilerApiExportMissing {
            package_version, ..
        } => (
            error,
            R::SelectedCompiler,
            Some(TypeScriptCompilerVersion::new(package_version)?),
        ),
        _ => (error, R::Unspecified, None),
    };
    let cause = match error {
        TypeScriptProjectHostError::ModuleFileLimit {
            observed, maximum, ..
        } => C::RuntimeFiles {
            observed: u64::try_from(*observed).ok()?,
            maximum: u64::try_from(*maximum).ok()?,
        },
        TypeScriptProjectHostError::ModuleEntryLimit {
            observed, maximum, ..
        } => C::FilesystemEntries {
            observed: u64::try_from(*observed).ok()?,
            maximum: u64::try_from(*maximum).ok()?,
        },
        TypeScriptProjectHostError::ModuleDirectoryDepth {
            observed, maximum, ..
        } => C::DirectoryDepth {
            observed: u64::try_from(*observed).ok()?,
            maximum: u64::try_from(*maximum).ok()?,
        },
        TypeScriptProjectHostError::FileTooLarge {
            observed, maximum, ..
        } if role != R::Unspecified => C::InputBytes {
            observed: *observed,
            maximum: *maximum,
        },
        TypeScriptProjectHostError::CompilerApiEntryMissing { .. } => C::MissingApiEntry,
        TypeScriptProjectHostError::CompilerApiExportMissing { export, .. } => {
            C::MissingApiExport {
                export: TypeScriptCompilerApiExport::from_name(export)?,
            }
        }
        _ => return None,
    };
    TypeScriptCompilerAdmissionDetail::new(role, version, cause)
        .filter(|detail| detail.failure_kind() == host_failure(error))
}

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
        TypeScriptProjectHostError::Described { source, .. } => host_failure(source),
        TypeScriptProjectHostError::CompilerPackageAdmission { source, .. } => host_failure(source),
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
        TypeScriptProjectHostError::CompilerProgramSourceMissing { .. } => F::HostCompilerApiBridge,
        TypeScriptProjectHostError::CompilerApiBridge { .. } => F::HostCompilerApiBridge,
        TypeScriptProjectHostError::CompilerApiEntryMissing { .. }
        | TypeScriptProjectHostError::CompilerApiExportMissing { .. } => F::HostCompilerApiBridge,
        TypeScriptProjectHostError::CompilerProgramCancelled => F::HostProgramCancelled,
        TypeScriptProjectHostError::CompilerProgramDeadline => F::HostProgramDeadline,
        TypeScriptProjectHostError::CompilerProgramProbe { source } => {
            program_probe_failure(source)
        }
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
        TypeScriptProjectHostError::ModuleFileLimit { .. }
        | TypeScriptProjectHostError::ModuleEntryLimit { .. }
        | TypeScriptProjectHostError::ModuleDirectoryDepth { .. } => F::HostModuleFileLimit,
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

/// Peels the public-cause wrapper so typed classification sees the original error.
fn peel(error: &TypeScriptProjectHostError) -> &TypeScriptProjectHostError {
    match error {
        TypeScriptProjectHostError::Described { source, .. } => peel(source),
        other => other,
    }
}

/// Sanitized cause retained by [`describe_host_error`], when this error carries one.
pub(super) fn failure_cause(error: &PackageAuthorityError) -> Option<TypeScriptFailureCause> {
    match error {
        PackageAuthorityError::TypeScriptProjectHost(TypeScriptProjectHostError::Described {
            cause,
            ..
        }) => Some(cause.clone()),
        PackageAuthorityError::TypeScriptTsz(TszAuthorityError::SourceAt { path, source }) => {
            // This is the rejected program member, not the first source used
            // to bind the enclosing compilation request. Keep that binding
            // intact and expose the actual member through the bounded cause.
            let scope = PathScope::new(&[]);
            TypeScriptFailureCause::sanitize(&format!(
                "Native TypeScript program member {}: {source}", scope.text(path),
            ))
        }
        _ => None,
    }
}

/// Pairs a host error with its public cause text under the given project roots.
///
/// The text is rebuilt from fields the host controls (config selection, config
/// parse, closure and bridge messages), never from process output. Every path
/// below a root becomes root-relative and every other absolute path is masked,
/// so the result names project files without exposing the machine layout.
pub(crate) fn describe_host_error(
    error: TypeScriptProjectHostError,
    roots: &[&Path],
) -> TypeScriptProjectHostError {
    if matches!(error, TypeScriptProjectHostError::Described { .. }) {
        return error;
    }
    let scope = PathScope::new(roots);
    match host_cause_text(&error, &scope).and_then(|text| TypeScriptFailureCause::sanitize(&text)) {
        Some(cause) => TypeScriptProjectHostError::Described {
            cause,
            source: Box::new(error),
        },
        None => error,
    }
}

/// Canonical and as-given spellings of the roots a public cause may name.
struct PathScope {
    roots: Vec<String>,
}

impl PathScope {
    fn new(roots: &[&Path]) -> Self {
        let mut spellings = Vec::new();
        for root in roots {
            if let Ok(canonical) = std::fs::canonicalize(root) {
                spellings.push(canonical.to_string_lossy().into_owned());
            }
            spellings.push(root.to_string_lossy().into_owned());
        }
        spellings.retain(|root| root.len() > 1);
        spellings
            .sort_unstable_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
        spellings.dedup();
        Self { roots: spellings }
    }

    /// Project-relative spelling of one path; outside paths keep only the file name.
    fn path(&self, path: &Path) -> String {
        let text = path.to_string_lossy();
        for root in &self.roots {
            if let Some(rest) = text.strip_prefix(root.as_str()) {
                if rest.is_empty() {
                    return ".".to_owned();
                }
                if let Some(relative) = rest.strip_prefix(['/', '\\']) {
                    return relative.to_owned();
                }
            }
        }
        if path.is_absolute() {
            let name = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            return format!("<outside project>/{name}");
        }
        text.into_owned()
    }

    /// Makes host-written free text project-relative and masks foreign paths.
    fn text(&self, message: &str) -> String {
        let mut text = message.to_owned();
        for root in &self.roots {
            text = text
                .replace(&format!("{root}/"), "")
                .replace(&format!("{root}\\"), "");
            text = text.replace(root.as_str(), ".");
        }
        mask_absolute_paths(&text)
    }
}

/// Replaces each remaining absolute path token with `<outside project>/<name>`.
fn mask_absolute_paths(text: &str) -> String {
    fn token_break(character: char) -> bool {
        character.is_whitespace() || matches!(character, '"' | '\'' | '`' | ')' | ']' | ',' | ';')
    }
    fn token_start_context(previous: Option<char>) -> bool {
        previous.is_none_or(|character| {
            character.is_whitespace()
                || matches!(character, '"' | '\'' | '`' | '(' | '[' | '=' | ':' | ',')
        })
    }
    let characters = text.char_indices().collect::<Vec<_>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < characters.len() {
        let (offset, character) = characters[index];
        let previous = index.checked_sub(1).map(|before| characters[before].1);
        let drive = character.is_ascii_alphabetic()
            && token_start_context(previous)
            && matches!(characters.get(index + 1), Some((_, ':')))
            && matches!(characters.get(index + 2), Some((_, '/' | '\\')));
        if (character == '/' && token_start_context(previous)) || drive {
            let mut end = index;
            while end < characters.len() && !token_break(characters[end].1) {
                end += 1;
            }
            let token_end = characters
                .get(end)
                .map_or(text.len(), |(offset, _)| *offset);
            let token = &text[offset..token_end];
            let name = token
                .rsplit(['/', '\\'])
                .find(|part| !part.is_empty())
                .unwrap_or_default();
            if token.len() > 1 && token.chars().any(char::is_alphanumeric) && !name.is_empty() {
                output.push_str("<outside project>/");
                output.push_str(name);
            } else {
                output.push_str(token);
            }
            index = end;
            continue;
        }
        output.push(character);
        index += 1;
    }
    output
}

fn host_cause_text(error: &TypeScriptProjectHostError, scope: &PathScope) -> Option<String> {
    use TypeScriptProjectHostError as E;
    Some(match error {
        E::Described { source, .. } => return host_cause_text(source, scope),
        E::CompilerApiBridge { message } => scope.text(message),
        E::CompilerProgramSourceMissing {
            package_relative_path,
        } => format!(
            "{} was absent from the compiler program",
            scope.text(package_relative_path)
        ),
        E::ConfigInvalid { config, message } => {
            format!(
                "{} is malformed: {}",
                scope.path(config),
                scope.text(message)
            )
        }
        E::ConfigMissing { config } => format!("{} does not exist", scope.path(config)),
        E::WorkspaceConfigInvalid { path, message } => {
            format!("{} is malformed: {}", scope.path(path), scope.text(message))
        }
        E::ConfigCycle { config } => {
            format!(
                "the configuration graph has a cycle at {}",
                scope.path(config)
            )
        }
        E::ConfigEscapesBoundary { config, boundary } => format!(
            "{} escapes the workspace boundary {}",
            scope.path(config),
            scope.path(boundary)
        ),
        E::ConfigFileLimit { observed, maximum } => {
            format!("{observed} tsconfig files were found; the maximum is {maximum}")
        }
        E::ConfigDirectoryDepth { directory } => format!(
            "the tsconfig scan exceeded its directory depth at {}",
            scope.path(directory)
        ),
        E::CompilerIoClosureLimit { phase, detail } => {
            format!("{} budget: {}", scope.text(phase), scope.text(detail))
        }
        E::CompilerIoClosureMismatch { detail } => scope.text(detail),
        E::PackagePath { path, source } => {
            format!("{} could not be read: {}", scope.path(path), source.kind())
        }
        E::FileTooLarge {
            path,
            observed,
            maximum,
        } => format!(
            "{} has {observed} bytes; the limit is {maximum}",
            scope.path(path)
        ),
        E::RegularFileRequired { path } => {
            format!("{} is not a regular file", scope.path(path))
        }
        E::RegularDirectoryRequired { path } => {
            format!("{} is not a regular directory", scope.path(path))
        }
        E::ModuleEntrySymlink { path } => {
            format!("{} is a disallowed symlink", scope.path(path))
        }
        E::WitnessChanged { path } => {
            format!("{} changed while it was being analyzed", scope.path(path))
        }
        E::SourceOutsideCapability { path } => format!(
            "{} is outside the project and compiler roots",
            scope.path(path)
        ),
        E::ResolvedSourceLimit { observed, maximum } => {
            format!("{observed} resolved sources exceed the {maximum}-file limit")
        }
        E::ResolvedSourceBytes { observed, maximum } => {
            format!("{observed} resolved source bytes exceed the {maximum}-byte limit")
        }
        E::ResolverObservationLimit { observed, maximum } => {
            format!("{observed} resolver observations exceed the limit of {maximum}")
        }
        E::NonPortablePath { path } => {
            format!("{} is not a portable UTF-8 path", scope.path(path))
        }
        E::CheckerConfiguration { message, .. } => scope.text(message),
        E::CompilerProgramProbe { source } => probe_cause_text(source),
        E::NodeProbe { source, .. } => format!("Node {}", probe_cause_text(source)),
        E::CompilerProbe { source, .. } => format!("TypeScript {}", probe_cause_text(source)),
        E::ManifestInvalid { manifest, message } => {
            format!(
                "{} is malformed: {}",
                scope.path(manifest),
                scope.text(message)
            )
        }
        E::ManifestTooLarge { manifest, maximum } => {
            format!("{} exceeds the {maximum}-byte limit", scope.path(manifest))
        }
        E::InvalidPackageName { manifest, name } => format!(
            "{} names package {}, not typescript",
            scope.path(manifest),
            scope.text(name)
        ),
        E::InvalidPackageVersion { manifest, version } => format!(
            "{} has the invalid version {}",
            scope.path(manifest),
            scope.text(version)
        ),
        E::PackageEscapesNodeModules {
            package,
            node_modules,
        } => format!(
            "{} escapes {}",
            scope.path(package),
            scope.path(node_modules)
        ),
        E::CompilerEscapesPackage { compiler, package } => format!(
            "compiler entry {} is outside {}",
            scope.path(compiler),
            scope.path(package)
        ),
        E::YarnPnpUnsupported { marker } => format!(
            "{} marks Yarn Plug'n'Play, which needs a node_modules layout",
            scope.path(marker)
        ),
        E::VersionMismatch {
            expected, observed, ..
        } => format!(
            "the compiler reports {} but its package is {}",
            scope.text(observed),
            scope.text(expected)
        ),
        E::InvalidCompilerVersion { output, .. } => format!(
            "the compiler printed an invalid version: {}",
            scope.text(output)
        ),
        _ => return None,
    })
}

/// Closed account of a Compiler API process failure; stream bytes never appear.
fn probe_cause_text(error: &super::toolchain_probe::ToolchainProbeError) -> String {
    use super::toolchain_probe::{ToolchainProbeError as E, ToolchainProbePrimary as P};
    match error {
        E::Bounded {
            primary: P::Deadline { .. },
            ..
        } => "the Compiler API process exceeded its time limit".to_owned(),
        E::Bounded {
            primary: P::OutputLimit { .. },
            ..
        } => "the Compiler API process exceeded its output limit".to_owned(),
        E::Bounded {
            primary: P::Cancelled,
            ..
        } => "the Compiler API process was cancelled".to_owned(),
        E::Exit { status, .. } => format!("the Compiler API process exited with {status}"),
        E::Spawn { .. } | E::ProbeWorkerSpawn { .. } | E::ReaderSpawn { .. } => {
            "the Compiler API process could not be started".to_owned()
        }
        E::Cleanup { preceding, .. }
        | E::Stream {
            preceding: Some(preceding),
            ..
        }
        | E::Streams {
            preceding: Some(preceding),
            ..
        } => probe_cause_text(preceding),
        _ => "the Compiler API process failed before producing a report".to_owned(),
    }
}

// Keep process control typed through cleanup and stream wrappers. The original
// process/cleanup error remains retained in CompilerProgramProbe for local diagnostics.
fn program_probe_failure(error: &super::toolchain_probe::ToolchainProbeError) -> F {
    use super::toolchain_probe::{ToolchainProbeError as E, ToolchainProbePrimary as P};
    match error {
        E::Bounded {
            primary: P::Cancelled,
            ..
        } => F::HostProgramCancelled,
        E::Bounded {
            primary: P::Deadline { .. },
            ..
        } => F::HostProgramDeadline,
        E::Cleanup { preceding, .. }
        | E::Stream {
            preceding: Some(preceding),
            ..
        }
        | E::Streams {
            preceding: Some(preceding),
            ..
        } => program_probe_failure(preceding),
        E::TypeScriptInterpreterUnavailable { .. }
        | E::TypeScriptModuleRootUnavailable { .. }
        | E::TypeScriptModuleEntryMismatch { .. }
        | E::RelativeExecutable { .. }
        | E::ExecutableWitness { .. }
        | E::ExecutableChanged { .. }
        | E::Spawn { .. }
        | E::ProbeWorkerSpawn { .. }
        | E::ReaderSpawn { .. }
        | E::MissingStream { .. }
        | E::Wait { .. }
        | E::Stream {
            preceding: None, ..
        }
        | E::Streams {
            preceding: None, ..
        }
        | E::Bounded {
            primary: P::OutputLimit { .. },
            ..
        }
        | E::MissingStatus { .. }
        | E::Exit { .. }
        | E::Empty { .. }
        | E::Resolution { .. } => F::HostCompilerApiBridge,
    }
}

fn native_failure(error: &TszAuthorityError) -> F {
    match error {
        TszAuthorityError::Source(error) | TszAuthorityError::SourceAt { source: error, .. } => match error {
            TszSourceError::InvalidPath => F::NativeSourceInvalidPath,
            TszSourceError::UnsupportedExtension => F::NativeSourceUnsupportedExtension,
            TszSourceError::DuplicatePath => F::NativeSourceDuplicatePath,
            TszSourceError::EmptyProject => F::NativeSourceEmptyProject,
            TszSourceError::InvalidUtf8 => F::NativeSourceInvalidUtf8,
        },
        TszAuthorityError::CheckerWorkerPoolUnavailable(_) => F::NativeCheckerWorkerPoolUnavailable,
        TszAuthorityError::MissingFile(_) => F::NativeMissingFile,
        TszAuthorityError::MissingSource(_) => F::NativeMissingSource,
        TszAuthorityError::SourceMismatch { .. } => F::NativeSourceMismatch,
        TszAuthorityError::ProjectSessionMismatch => F::NativeProjectSessionMismatch,
        TszAuthorityError::ModuleResolution(error) => module_failure(error, ModuleSite::Admission),
        TszAuthorityError::ProjectCheckerSession(error) => match error {
            TszProjectQuerySessionError::MissingProjectModuleResolutions => {
                F::NativeSessionMissingProjectModuleResolutions
            }
            TszProjectQuerySessionError::ModuleResolution(_) => F::NativeSessionModuleResolution,
            TszProjectQuerySessionError::ExecutionCheckpointMismatch => {
                F::NativeSessionExecutionCheckpointMismatch
            }
            TszProjectQuerySessionError::FileIndexOutOfRange { .. } => {
                F::NativeSessionFileIndexOutOfRange
            }
            TszProjectQuerySessionError::ExecutionStopped(reason) => {
                stop_failure(*reason, StopSite::Session)
            }
            // A file query that spent its own allowance with no restart left: the
            // allowance is work budget, so the closed cause is the same one.
            TszProjectQuerySessionError::FileWorkExhausted { .. } => {
                F::NativeSessionWorkBudgetExhausted
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
        TszAuthorityError::FileWorkExhausted(_) => F::NativeProjectCheckWorkBudgetExhausted,
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
        (ModuleSite::Admission, E::InvalidReference { .. }) => F::NativeModuleInvalidReference,
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
        (ModuleSite::ProjectCheck, E::InvalidReference { .. }) => {
            F::NativeProjectCheckModuleInvalidReference
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
    fn native_source_refusal_retains_the_actual_member_with_the_existing_failure_tag() {
        let error = PackageAuthorityError::TypeScriptTsz(TszAuthorityError::SourceAt {
            path: "src/frontend/assets/view.vue".into(),
            source: TszSourceError::UnsupportedExtension,
        });
        assert_eq!(failure_kind(&error), Some(F::NativeSourceUnsupportedExtension));
        let cause = failure_cause(&error).expect("bounded actual program member context");
        assert!(cause.as_str().contains("src/frontend/assets/view.vue"));
        assert!(!cause.as_str().contains(".prettierrc.js"));
    }

    #[test]
    fn native_module_refusals_retain_the_resolution_site_without_inventing_a_path_failure() {
        use TszProjectModuleResolutionError as E;
        let invalid_reference = E::InvalidReference {
            importer_path: "/private/authority.secret.ts".into(),
            offset: 17,
        };
        for (error, expected, tag) in [
            (
                TszAuthorityError::ModuleResolution(invalid_reference.clone()),
                F::NativeModuleInvalidReference,
                "typescript_native_module_invalid_reference",
            ),
            (
                TszAuthorityError::ProjectCheck(TszProjectProgramCheckError::ModuleResolution(
                    invalid_reference.clone(),
                )),
                F::NativeProjectCheckModuleInvalidReference,
                "typescript_native_project_check_module_invalid_reference",
            ),
            (
                TszAuthorityError::ProjectCheckerSession(
                    TszProjectQuerySessionError::ModuleResolution(invalid_reference),
                ),
                F::NativeSessionModuleResolution,
                "typescript_native_session_module_resolution",
            ),
        ] {
            let kind = failure_kind(&PackageAuthorityError::TypeScriptTsz(error))
                .expect("exact native refusal");
            assert_eq!(kind, expected);
            assert_eq!(kind.kind_tag(), tag);
            assert!(!kind.is_cancelled());
            assert!(!kind.is_program_execution_failure());
            assert!(!kind.detail().contains("private"));
            assert_ne!(kind, F::NativeModuleInvalidVirtualPath);
            assert_ne!(kind, F::NativeSessionMissingProjectModuleResolutions);
        }

        // The session variant is a rejection of present resolution inputs,
        // regardless of which nested validation failed. It is never absence.
        for error in [
            E::InvalidVirtualPath {
                path: "/private/path".into(),
            },
            E::DuplicateProgramPath {
                path: "/private/path".into(),
            },
            E::ImporterNotInProgram {
                path: "/private/path".into(),
            },
            E::TargetNotInProgram {
                path: "/private/path".into(),
            },
            E::EmptySpecifier,
            E::EmptyExternalIdentity,
            E::UnsupportedRequestKind {
                syntax_kind: "private syntax".into(),
            },
            E::DuplicateRequest {
                importer_path: "/private/path".into(),
                specifier: "private specifier".into(),
            },
        ] {
            assert_eq!(
                native_failure(&TszAuthorityError::ProjectCheckerSession(
                    TszProjectQuerySessionError::ModuleResolution(error),
                )),
                F::NativeSessionModuleResolution
            );
        }
    }

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
    fn described_host_errors_keep_their_kind_and_expose_only_bounded_project_relative_cause() {
        let root = Path::new("/work/app");
        let bridge = describe_host_error(
            TypeScriptProjectHostError::CompilerApiBridge {
                message: "selected config has Compiler API parse diagnostics: TS5083: Cannot read file '/work/app/tsconfig.app.json'. (/home/dev/.nvm/typescript/lib/typescript.js:1)".into(),
            },
            &[root],
        );
        let error = PackageAuthorityError::TypeScriptProjectHost(bridge);
        assert_eq!(failure_kind(&error), Some(F::HostCompilerApiBridge));
        assert_eq!(admission_detail(&error), None);
        let cause = failure_cause(&error).expect("bridge cause is retained");
        assert_eq!(
            cause.as_str(),
            "selected config has Compiler API parse diagnostics: TS5083: Cannot read file 'tsconfig.app.json'. (<outside project>/typescript.js:1)"
        );
        assert!(!cause.as_str().contains("/work/app"));
        assert!(!cause.as_str().contains("/home/dev"));
        assert!(cause.detail_for(F::HostCompilerApiBridge).len() <= 384);

        for (error, expected) in [
            (
                TypeScriptProjectHostError::ConfigInvalid {
                    config: Path::new("/work/app/packages/a/tsconfig.json").into(),
                    message: "expected value at line 3 column 5".into(),
                },
                "packages/a/tsconfig.json is malformed: expected value at line 3 column 5",
            ),
            (
                TypeScriptProjectHostError::ConfigMissing {
                    config: Path::new("/work/app/tsconfig.node.json").into(),
                },
                "tsconfig.node.json does not exist",
            ),
            (
                TypeScriptProjectHostError::ConfigCycle {
                    config: Path::new("/elsewhere/tsconfig.json").into(),
                },
                "the configuration graph has a cycle at <outside project>/tsconfig.json",
            ),
            (
                TypeScriptProjectHostError::ConfigFileLimit {
                    observed: 600,
                    maximum: 512,
                },
                "600 tsconfig files were found; the maximum is 512",
            ),
        ] {
            let kind = host_failure(&error);
            let described = describe_host_error(error, &[root]);
            assert_eq!(host_failure(&described), kind, "typed kind is preserved");
            let error = PackageAuthorityError::TypeScriptProjectHost(described);
            assert_eq!(failure_cause(&error).unwrap().as_str(), expected);
        }

        // Typed admission facts stay authoritative and cannot be shadowed by text.
        let admission = describe_host_error(
            TypeScriptProjectHostError::CompilerPackageAdmission {
                package_version: "5.9.3".into(),
                role: R::SelectedCompiler,
                source: Box::new(TypeScriptProjectHostError::ModuleFileLimit {
                    package_root: Path::new("/work/app/node_modules/typescript").into(),
                    observed: 300,
                    maximum: 256,
                }),
            },
            &[root],
        );
        assert!(matches!(
            admission,
            TypeScriptProjectHostError::CompilerPackageAdmission { .. }
        ));

        // Process output never becomes public text, only a closed account of the failure.
        let probe = describe_host_error(
            TypeScriptProjectHostError::CompilerProgramProbe {
                source: super::super::toolchain_probe::ToolchainProbeError::Empty {
                    tool: backend_semantic::vocabulary::NativeTool::TypeScriptCompiler,
                },
            },
            &[root],
        );
        let error = PackageAuthorityError::TypeScriptProjectHost(probe);
        assert_eq!(
            failure_cause(&error).unwrap().as_str(),
            "the Compiler API process failed before producing a report"
        );
    }

    #[test]
    fn path_masking_handles_roots_quotes_and_foreign_absolute_paths() {
        let scope = PathScope::new(&[Path::new("/work/app"), Path::new("/work")]);
        assert_eq!(scope.path(Path::new("/work/app")), ".");
        assert_eq!(scope.path(Path::new("/work/app/src/a.ts")), "src/a.ts");
        assert_eq!(scope.path(Path::new("/work/other/b.ts")), "other/b.ts");
        assert_eq!(
            scope.path(Path::new("/usr/lib/c.ts")),
            "<outside project>/c.ts"
        );
        assert_eq!(
            scope.text(
                "a/b is fine; \"/usr/lib/x.js\" and (/opt/y/z.ts:3) and C:\\Users\\me\\q.ts too"
            ),
            "a/b is fine; \"<outside project>/x.js\" and (<outside project>/z.ts:3) and <outside project>/q.ts too"
        );
        assert_eq!(scope.text("see /work/app/src"), "see src");
        assert_eq!(mask_absolute_paths("a / b"), "a / b");
    }

    #[test]
    fn native_checker_worker_start_refusal_keeps_native_provenance() {
        let error =
            PackageAuthorityError::TypeScriptTsz(TszAuthorityError::CheckerWorkerPoolUnavailable(
                "private OS thread failure /secret/path".into(),
            ));
        let kind = failure_kind(&error).expect("closed native failure");
        assert_eq!(kind, F::NativeCheckerWorkerPoolUnavailable);
        assert_ne!(kind, F::CheckerSpawn);
        assert_eq!(
            kind.kind_tag(),
            "typescript_native_checker_worker_pool_unavailable"
        );
        assert!(!kind.detail().contains("/secret"));
        assert!(!kind.is_cancelled());
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
                    TypeScriptProjectHostError::CompilerProgramSourceMissing {
                        package_relative_path: "modules/private-source.js".into(),
                    },
                ),
                F::HostCompilerApiBridge,
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
        let source_missing = TypeScriptProjectHostError::CompilerProgramSourceMissing {
            package_relative_path: "modules/private-source.js".into(),
        };
        let local_detail = source_missing.to_string();
        assert!(local_detail.contains("modules/private-source.js"));
        assert!(!local_detail.contains("/private/"));
        assert_eq!(host_failure(&source_missing), F::HostCompilerApiBridge);
    }
}
