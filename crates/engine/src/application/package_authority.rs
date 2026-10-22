//! Owns package-scoped language authority until driver lowering consumes it.
//!
//! A [`crate::driver::SemanticAuthorityInput`] only borrows its reports,
//! images, or project context.  This module is the deliberately non-
//! self-referential owner that keeps those values alive across the driver's
//! fused semantic transaction.  It never manufactures a source-only fallback
//! for a profile whose authoritative package adapter was unavailable.

use std::{
    path::{Component, Path},
    sync::atomic::Ordering,
    time::Instant,
};

use crate::compiler_input_manifest_v2::CompilationUnitKeyV2;
use crate::driver::{
    CompileControl, ExternalToolchain, NotExternalToolchain, SemanticAuthorityInput,
    ToolchainSelection,
};
use backend_frontend_clang::ClangAuthorityEnvironment;
use backend_frontend_csharp::legacy::{
    CSharpAuthorityConfiguration, CSharpAuthorityControl, CSharpAuthorityError,
    CSharpAuthorityRequest, CSharpOracle,
};
use backend_frontend_go::legacy::oracle::{
    GoPackageAuthorityWitness, GoPackageAuthorityWitnessError,
};
use backend_frontend_go::legacy::{ConfiguredGoOracle, OracleError};
use backend_frontend_java::legacy::harness::{
    Harness, HarnessError, HarnessRequest, JavaSource, JdkToolchain,
};
use backend_frontend_python::legacy::checker::{
    NativePythonProjectAuthority, PythonProjectBytesSource, PythonProjectControl,
    PythonProjectReport, PythonProjectSource,
};
use backend_frontend_python::legacy::{
    CheckerError as PyreflyError, CheckerReport as PythonReport, ExtractionError, Pyrefly, extract,
};
use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustAuthorityError, RustCargoMetadataPolicy, RustFeatureControl,
    RustToolchain, RustWorkspace, SourceByteLimit,
};
use backend_frontend_typescript::legacy::{
    CheckerError as TypeScriptCheckerError, ExplicitTypeScriptChecker, Report as TypeScriptReport,
};
use backend_library::interface::{CompilerToolFailure, CompilerToolIssue, CompilerToolRequirement};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, TypeScriptSource};
use backend_version::{ContentId, ToolchainDomain};
use thiserror::Error;

use super::runtime::LocalRuntimeRustToolchainSelection;
use super::typescript_host::TypeScriptProjectHost;

/// Bounded, explicit package-authority adapters selected by the application
/// owner.  Every optional field names one independently configured producer;
/// absence is a typed terminal, never a default process lookup.
#[derive(Clone, Copy, Debug)]
pub struct PackageAuthorityConfiguration<'config> {
    /// Exact driver, resource, include, and loaded libclang selection for C/C++.
    pub clang: Option<&'config ClangAuthorityEnvironment>,
    /// TypeScript checker that stages the explicitly selected package root.
    pub typescript: Option<&'config ExplicitTypeScriptChecker>,
    /// Closed host inputs for request-scoped project compiler admission.
    pub typescript_project_host: Option<&'config TypeScriptProjectHost>,
    /// Closed Python checker state; a ready value carries both adapter and version proof.
    pub python_checker: super::LocalRuntimePythonCheckerAdmission<&'config Pyrefly>,
    /// Rust Analyzer/Cargo authority configuration.
    pub rust: Option<RustPackageAuthorityConfiguration<'config>>,
    /// Go package oracle selected by the application owner.
    pub go: Option<&'config ConfiguredGoOracle>,
    /// Typed Go-only host admission failure retained by the application owner.
    pub go_unavailable: Option<super::LocalRuntimeGoAuthorityFailure>,
    /// Roslyn helper producer selected by the application owner.
    pub csharp: Option<CSharpPackageAuthorityConfiguration<'config>>,
    /// Java doclet/JDK authority configuration.
    pub java: Option<JavaPackageAuthorityConfiguration<'config>>,
    /// Largest borrowed Go or Java authority image this owner will retain.
    pub maximum_image_bytes: usize,
}

impl PackageAuthorityConfiguration<'static> {
    /// Explicit absence of every sidecar authority producer.
    ///
    /// Every package profile reaches a typed unavailable terminal unless its
    /// caller selects the corresponding explicit authority configuration.
    pub const UNAVAILABLE: Self = Self {
        clang: None,
        typescript: None,
        typescript_project_host: None,
        python_checker: super::LocalRuntimePythonCheckerAdmission::Unconfigured,
        rust: None,
        go: None,
        go_unavailable: None,
        csharp: None,
        java: None,
        maximum_image_bytes: 0,
    };
}

/// Explicit Roslyn authority configuration for one compiler-owner lifetime.
#[derive(Clone, Copy, Debug)]
pub struct CSharpPackageAuthorityConfiguration<'config> {
    /// Producer bound to the exact published Roslyn helper assembly.
    pub producer: &'config CSharpOracle,
    /// Closed helper policy borrowed by each C# authority transaction.
    pub configuration: CSharpAuthorityConfiguration<'config>,
}

/// Explicit Rust project authority inputs that cannot be inferred from source
/// text or a native executable selection alone.
#[derive(Clone, Copy, Debug)]
pub struct RustPackageAuthorityConfiguration<'config> {
    /// Rust authority selected for this owner; a typed deferred default is realized only for a
    /// Rust request. Its executable must match the request's resolved compiler.
    pub toolchain: &'config LocalRuntimeRustToolchainSelection,
    /// Exact source budget checked before Cargo graph loading.
    pub maximum_source_bytes: SourceByteLimit,
    /// Caller-selected Cargo feature policy.
    pub features: RustFeatureControl<'config>,
    /// Registry metadata network policy for complete Cargo resolution.
    pub metadata_policy: RustCargoMetadataPolicy,
}

impl RustPackageAuthorityConfiguration<'_> {
    pub(crate) fn resolve_toolchain(
        &self,
        profile: LanguageProfile,
        control: CompileControl<'_>,
    ) -> Result<&RustToolchain, PackageAuthorityError> {
        self.toolchain
            .resolve(control)
            .map_err(|cause| match cause {
                super::LocalRuntimeRustAuthorityFailure::Cancelled => {
                    PackageAuthorityError::Cancelled {
                        profile,
                        stage: PackageAuthorityStage::RustProject,
                    }
                }
                super::LocalRuntimeRustAuthorityFailure::Deadline => {
                    PackageAuthorityError::Deadline {
                        profile,
                        stage: PackageAuthorityStage::RustProject,
                    }
                }
                cause => PackageAuthorityError::RustToolchainAdmission { profile, cause },
            })
    }
}

/// Explicit Java authority inputs.  The source path is converted to a package
/// relative name only after the request proves containment under `package_root`.
#[derive(Clone, Copy, Debug)]
pub struct JavaPackageAuthorityConfiguration<'config> {
    /// Caller-validated JDK used to compile the embedded extractor and source.
    pub toolchain: &'config JdkToolchain<'config>,
    /// Caller-selected classpath visible to the exact Java package source.
    pub classpath: &'config [&'config Path],
}

/// One fully explicit package authority admission request.
///
/// `package_root`, `source_path`, and `source` are supplied together by the
/// package resolver; this owner does not discover an alternate root or entry
/// file.  The driver toolchain selection and control are retained as typed
/// facts so authority admission cannot outlive cancellation/deadline policy.
#[derive(Clone, Copy, Debug)]
pub struct PackageAuthorityRequest<'request, 'config> {
    /// Canonical local package root selected by the package resolver.
    pub package_root: &'request Path,
    /// Canonical source file selected beneath `package_root`.
    pub source_path: &'request Path,
    /// Exact source bytes read by the package resolver.
    pub source: &'request [u8],
    /// Exact captured Java siblings available to javac without becoming explicit image inputs.
    pub java_sourcepath: &'request [JavaSource<'request>],
    /// Exact native compilation unit selected by the admitted package target.
    pub unit_key: &'request CompilationUnitKeyV2,
    /// Closed language profile that determines the one allowed authority.
    pub profile: LanguageProfile,
    /// Native-tool selection already routed by the compiler application.
    pub toolchain: ToolchainSelection<'request>,
    /// Exact cancellation and deadline control for this compilation request.
    pub control: CompileControl<'request>,
    /// Explicit authority adapter configuration.
    pub configuration: PackageAuthorityConfiguration<'config>,
}

/// Retained report, image, or project state for one admitted package source.
///
/// This owner is intentionally separate from [`SemanticAuthorityInput`].
/// Calling [`Self::input`] borrows this enum after construction, so no report
/// pointer can outlive the transaction that owns it.
pub enum PackageAuthorityOwner<'workspace> {
    /// C or C++ compilation with the exact selected libclang authority.
    Clang {
        /// Exact C-family profile admitted by the caller.
        profile: LanguageProfile,
        /// Checked project root and entry translation unit retained for the
        /// driver's whole-project cross-file reference plane.
        project: backend_frontend_clang::ClangProject,
    },
    /// Package-aware TypeScript report bound to the exact source bytes.
    TypeScript {
        /// Exact checked profile.
        profile: TypeScriptSource,
        /// Report retained for the driver borrow.
        report: TypeScriptReport,
    },
    /// Pyrefly report bound to the exact Python source bytes.
    Python {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// Report retained for the driver borrow.
        report: PythonReport,
    },
    /// Cargo/rust-analyzer workspace retained for package-wide driver entry.
    Rust {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// One analyzer database and VFS shared by all package sources.
        workspace: RustWorkspace,
        /// Original bounded source admission policy.
        maximum_source_bytes: SourceByteLimit,
    },
    /// Borrowed lane-owned analyzer workspace retained by an in-progress session lease.
    RustBorrowed {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// Analyzer workspace that remains owned by the caller's cache lease.
        workspace: &'workspace RustWorkspace,
        /// Original bounded source admission policy.
        maximum_source_bytes: SourceByteLimit,
    },
    /// Go authority image owned for the driver's borrowed image input.
    Go {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// Validated producer image bytes.
        image: Box<[u8]>,
    },
    /// Roslyn authority image owned for the driver's borrowed image input.
    CSharp {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// Validated source-bound producer image bytes.
        image: Box<[u8]>,
    },
    /// Java authority image and its doclet session retained until lowering.
    Java {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// Validated producer image bytes.
        image: Box<[u8]>,
        /// Session whose private extraction workspace remains uniquely owned.
        _session: Harness,
    },
}

impl PackageAuthorityOwner<'_> {
    /// Borrows this retained authority in the exact shape accepted by the
    /// driver. C/C++ use the exact retained project and selected libclang
    /// rather than an external authority image.
    #[must_use]
    pub fn input<'source>(
        &'source self,
        source_path: &'source Path,
    ) -> SemanticAuthorityInput<'source> {
        match self {
            Self::Clang { project, .. } => SemanticAuthorityInput::Clang { project },
            Self::TypeScript { report, .. } => SemanticAuthorityInput::TypeScript { report },
            Self::Python { report, .. } => SemanticAuthorityInput::Python { report },
            Self::Rust {
                workspace,
                maximum_source_bytes,
                ..
            } => SemanticAuthorityInput::RustWorkspace {
                workspace,
                source_path,
                maximum_source_bytes: *maximum_source_bytes,
            },
            Self::RustBorrowed {
                workspace,
                maximum_source_bytes,
                ..
            } => SemanticAuthorityInput::RustWorkspace {
                workspace: *workspace,
                source_path,
                maximum_source_bytes: *maximum_source_bytes,
            },
            Self::Go { image, .. } => SemanticAuthorityInput::Go { image },
            Self::CSharp { image, .. } => SemanticAuthorityInput::CSharp { image },
            Self::Java { image, .. } => SemanticAuthorityInput::Java { image },
        }
    }

    /// Returns the exact profile preserved by the owned authority lane.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        match self {
            Self::Clang { profile, .. }
            | Self::Python { profile, .. }
            | Self::Rust { profile, .. }
            | Self::RustBorrowed { profile, .. }
            | Self::Go { profile, .. }
            | Self::CSharp { profile, .. }
            | Self::Java { profile, .. } => *profile,
            Self::TypeScript { profile, .. } => LanguageProfile::TypeScript(*profile),
        }
    }
}

/// Builds the one retained authority owner allowed for `request.profile`.
///
/// No authority value is returned after a cancellation or deadline checkpoint.
/// For subprocess adapters that cannot receive the driver's atomic flag, the
/// owner checks before entry and again before accepting their output; this
/// prevents a completed report/image from becoming visible after cancellation
/// or deadline won the enclosing compilation.
pub fn enter_package_authority<'request, 'config>(
    request: PackageAuthorityRequest<'request, 'config>,
) -> Result<PackageAuthorityOwner<'static>, PackageAuthorityError> {
    enter_package_authority_with_go_authority_witness(request, None)
}

/// Go authority shared by every source of one package request.
///
/// One session admits the package once (toolchain, workspace and dependency
/// closure proofs plus the oracle helper), runs a single oracle child for each
/// bounded batch of same-directory sources, and re-proves the same inputs once
/// in [`Self::finish`]. Entering authority per source repeated all of that for
/// every file, so a Go package's indexing time grew with its file count times the
/// cost of re-hashing the Go installation and re-listing its dependency graph.
pub(crate) struct GoPackageAuthority<'run> {
    session: backend_frontend_go::legacy::oracle::GoPackageAuthoritySession<'run>,
    profile: LanguageProfile,
    package_root: std::path::PathBuf,
    control: CompileControl<'run>,
    maximum_image_bytes: usize,
}

impl<'run> GoPackageAuthority<'run> {
    /// Admits the package of `first` (any one of its sources) and plans the
    /// oracle runs for `sources`, the files whose images will be requested.
    pub(crate) fn begin(
        first: PackageAuthorityRequest<'run, 'run>,
        captured: Option<&'run GoPackageAuthorityWitness>,
        sources: &[std::path::PathBuf],
    ) -> Result<Self, PackageAuthorityError> {
        checkpoint(
            first.control,
            first.profile,
            PackageAuthorityStage::Admission,
        )?;
        if first.unit_key != &CompilationUnitKeyV2::PackageRoot {
            return Err(PackageAuthorityError::CompilationUnitMismatch {
                profile: first.profile,
            });
        }
        require_resolved_toolchain(first.toolchain, first.profile)?;
        let oracle = match first.configuration.go {
            Some(oracle) => oracle,
            None => {
                if let Some(cause) = first.configuration.go_unavailable {
                    return Err(PackageAuthorityError::GoAuthorityUnavailable {
                        profile: first.profile,
                        cause,
                    });
                }
                return Err(PackageAuthorityError::AdapterUnavailable {
                    profile: first.profile,
                    stage: PackageAuthorityStage::GoOracle,
                });
            }
        };
        let witness = match captured {
            Some(witness) => std::borrow::Cow::Borrowed(witness),
            None => std::borrow::Cow::Owned(oracle.package_authority_witness_controlled(
                first.package_root,
                backend_frontend_go::legacy::GoOracleControl::new(
                    Some(first.control.cancelled),
                    first.control.deadline,
                ),
            )?),
        };
        checkpoint(
            first.control,
            first.profile,
            PackageAuthorityStage::GoOracle,
        )?;
        let session = oracle
            .begin_package_authority_controlled(
                first.package_root,
                witness,
                sources,
                backend_frontend_go::legacy::GoOracleControl::new(
                    Some(first.control.cancelled),
                    first.control.deadline,
                ),
            )
            .map_err(PackageAuthorityError::GoOracle)?;
        Ok(Self {
            session,
            profile: first.profile,
            package_root: first.package_root.to_path_buf(),
            control: first.control,
            maximum_image_bytes: first.configuration.maximum_image_bytes,
        })
    }

    /// Returns the retained authority of one source of the package.
    ///
    /// A refusal here is that source's own: its siblings keep their images.
    pub(crate) fn owner(
        &mut self,
        source_path: &Path,
    ) -> Result<PackageAuthorityOwner<'static>, PackageAuthorityError> {
        checkpoint(self.control, self.profile, PackageAuthorityStage::GoOracle)?;
        if source_path.strip_prefix(&self.package_root).is_err() {
            return Err(PackageAuthorityError::SourceOutsidePackage {
                profile: self.profile,
                package_root: self.package_root.clone().into_boxed_path(),
                source_path: source_path.to_path_buf().into_boxed_path(),
            });
        }
        let image = self
            .session
            .image(source_path)
            .map_err(PackageAuthorityError::GoOracle)?;
        let image = retain_image(
            image,
            self.maximum_image_bytes,
            self.profile,
            PackageAuthorityStage::GoOracle,
        )?;
        require_go_module_language(self.profile, &image)?;
        checkpoint(self.control, self.profile, PackageAuthorityStage::GoOracle)?;
        Ok(PackageAuthorityOwner::Go {
            profile: self.profile,
            image,
        })
    }

    /// Proves the admitted inputs did not change while the oracle ran. No
    /// image of this package may be published when this fails.
    pub(crate) fn finish(self) -> Result<(), PackageAuthorityError> {
        self.session
            .finish()
            .map_err(PackageAuthorityError::GoOracle)
    }
}

fn require_go_module_language(
    profile: LanguageProfile,
    bytes: &[u8],
) -> Result<(), PackageAuthorityError> {
    let image = backend_frontend_go::legacy::GoImage::open(bytes).map_err(|_| {
        PackageAuthorityError::GoModuleLanguage {
            profile,
            observed: "invalid native image".to_owned(),
        }
    })?;
    let module = image
        .module()
        .map_err(|_| PackageAuthorityError::GoModuleLanguage {
            profile,
            observed: "invalid native module metadata".to_owned(),
        })?;
    if let Some(module) = module {
        admit_go_module_version(profile, module.go_version)?;
    }
    Ok(())
}

fn admit_go_module_version(
    profile: LanguageProfile,
    version: &[u8],
) -> Result<(), PackageAuthorityError> {
    use backend_semantic::vocabulary::GoVersion;
    if version.is_empty() {
        return Ok(());
    }
    let maximum = match profile {
        LanguageProfile::Go(GoVersion::Go122) => 22,
        LanguageProfile::Go(GoVersion::Go123) => 23,
        LanguageProfile::Go(GoVersion::Go124) => 24,
        LanguageProfile::Go(GoVersion::Go125) => 25,
        LanguageProfile::Go(GoVersion::Go126) => 26,
        _ => 0,
    };
    // This is the actual Go producer's already validated module version, not
    // a second parser for untrusted go.mod syntax. Patch/prerelease spelling
    // does not change the language minor.
    let observed = std::str::from_utf8(version).ok();
    let supported = observed
        .and_then(|text| text.strip_prefix("1."))
        .and_then(|rest| {
            let end = rest.bytes().take_while(u8::is_ascii_digit).count();
            rest.get(..end)?.parse::<u32>().ok()
        })
        .is_some_and(|minor| minor <= maximum);
    if supported {
        return Ok(());
    }
    Err(PackageAuthorityError::GoModuleLanguage {
        profile,
        observed: observed
            .filter(|value| value.len() <= 128)
            .unwrap_or("invalid or oversized native module version")
            .to_owned(),
    })
}

/// Admits one native Python project session through the same closed package
/// authority guards used by the per-module entry point.
pub(crate) fn enter_python_project_authority(
    request: PackageAuthorityRequest<'_, '_>,
    package_name: &str,
    sources: &[PythonProjectBytesSource<'_>],
    environment: Option<&backend_frontend_python::legacy::checker::PythonEnvironmentInput>,
) -> Result<PythonProjectReport, PackageAuthorityError> {
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::Admission,
    )?;
    if request.unit_key != &CompilationUnitKeyV2::PackageRoot {
        return Err(PackageAuthorityError::CompilationUnitMismatch {
            profile: request.profile,
        });
    }
    let resolved = require_resolved_toolchain(request.toolchain, request.profile)?;
    request
        .source_path
        .strip_prefix(request.package_root)
        .map_err(|_| PackageAuthorityError::SourceOutsidePackage {
            profile: request.profile,
            package_root: request.package_root.to_path_buf().into_boxed_path(),
            source_path: request.source_path.to_path_buf().into_boxed_path(),
        })?;
    let LanguageProfile::Python(profile) = request.profile else {
        return Err(PackageAuthorityError::AdapterUnavailable {
            profile: request.profile,
            stage: PackageAuthorityStage::PythonPyrefly,
        });
    };
    let super::LocalRuntimePythonCheckerAdmission::Native { authority } =
        request.configuration.python_checker
    else {
        return Err(PackageAuthorityError::RequiredTool {
            profile: request.profile,
            stage: PackageAuthorityStage::PythonPyrefly,
            issue: CompilerToolIssue {
                requirement: CompilerToolRequirement::PythonChecker,
                failure: CompilerToolFailure::Missing,
            },
        });
    };
    require_native_python_toolchain(authority, resolved.identity)?;
    let environment = environment
        .ok_or_else(|| {
            PackageAuthorityError::PythonPyrefly(
                backend_frontend_python::legacy::checker::CheckerError::EnvironmentCaptureMissing,
            )
        })?
        .captured()
        .map_err(PackageAuthorityError::PythonPyrefly)?;
    let report = authority
        .analyze_project_bytes_with_environment(
            request.package_root,
            package_name,
            sources,
            Some(environment),
            profile,
            PythonProjectControl {
                cancelled: request.control.cancelled,
                deadline: request.control.deadline,
            },
        )
        .map_err(PackageAuthorityError::PythonPyrefly)?;
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::PythonPyrefly,
    )?;
    Ok(report)
}

fn require_native_python_toolchain(
    authority: NativePythonProjectAuthority,
    observed: ContentId<ToolchainDomain>,
) -> Result<(), PackageAuthorityError> {
    let expected = ContentId::from_digest(authority.producer_identity().as_bytes());
    if observed != expected {
        return Err(
            PackageAuthorityError::NativePythonToolchainIdentityMismatch { expected, observed },
        );
    }
    Ok(())
}

fn require_python_checker(
    admission: super::LocalRuntimePythonCheckerAdmission<&Pyrefly>,
    profile: LanguageProfile,
) -> Result<&Pyrefly, PackageAuthorityError> {
    match admission {
        super::LocalRuntimePythonCheckerAdmission::DeferredNativePython
        | super::LocalRuntimePythonCheckerAdmission::Native { .. }
        | super::LocalRuntimePythonCheckerAdmission::Unconfigured => {
            Err(PackageAuthorityError::RequiredTool {
                profile,
                stage: PackageAuthorityStage::PythonPyrefly,
                issue: CompilerToolIssue {
                    requirement: CompilerToolRequirement::PythonChecker,
                    failure: CompilerToolFailure::Missing,
                },
            })
        }
        super::LocalRuntimePythonCheckerAdmission::ProbeFailed { .. } => {
            Err(PackageAuthorityError::RequiredTool {
                profile,
                stage: PackageAuthorityStage::PythonPyrefly,
                issue: CompilerToolIssue {
                    requirement: CompilerToolRequirement::PythonChecker,
                    failure: CompilerToolFailure::ProbeFailed,
                },
            })
        }
        super::LocalRuntimePythonCheckerAdmission::Ready { adapter, .. } => Ok(adapter),
    }
}

pub(crate) fn enter_package_authority_with_go_authority_witness<'request, 'config>(
    request: PackageAuthorityRequest<'request, 'config>,
    captured_go_authority: Option<&GoPackageAuthorityWitness>,
) -> Result<PackageAuthorityOwner<'static>, PackageAuthorityError> {
    enter_package_authority_with_retained_rust_workspace(request, captured_go_authority, None)
}

pub(crate) fn enter_package_authority_with_rust_workspace<'request, 'config, 'workspace>(
    request: PackageAuthorityRequest<'request, 'config>,
    captured_go_authority: Option<&GoPackageAuthorityWitness>,
    rust_workspace: &'workspace RustWorkspace,
) -> Result<PackageAuthorityOwner<'workspace>, PackageAuthorityError> {
    enter_package_authority_with_retained_rust_workspace(
        request,
        captured_go_authority,
        Some(rust_workspace),
    )
}

fn enter_package_authority_with_retained_rust_workspace<'request, 'config, 'workspace>(
    request: PackageAuthorityRequest<'request, 'config>,
    captured_go_authority: Option<&GoPackageAuthorityWitness>,
    rust_workspace: Option<&'workspace RustWorkspace>,
) -> Result<PackageAuthorityOwner<'workspace>, PackageAuthorityError> {
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::Admission,
    )?;
    let unit_supported = match request.unit_key {
        CompilationUnitKeyV2::PackageRoot => true,
        CompilationUnitKeyV2::RustCrate { .. } => {
            request.profile.language() == backend_semantic::vocabulary::Language::Rust
        }
        CompilationUnitKeyV2::CSharpProject { .. } => {
            request.profile.language() == backend_semantic::vocabulary::Language::CSharp
        }
        _ => false,
    };
    if !unit_supported {
        return Err(PackageAuthorityError::CompilationUnitMismatch {
            profile: request.profile,
        });
    }
    let resolved = require_resolved_toolchain(request.toolchain, request.profile)?;
    let relative = request
        .source_path
        .strip_prefix(request.package_root)
        .map_err(|_| PackageAuthorityError::SourceOutsidePackage {
            profile: request.profile,
            package_root: request.package_root.to_path_buf().into_boxed_path(),
            source_path: request.source_path.to_path_buf().into_boxed_path(),
        })?;

    let owner =
        match request.profile {
            profile @ (LanguageProfile::C(_) | LanguageProfile::Cxx(_)) => {
                let environment = request.configuration.clang.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile,
                        stage: PackageAuthorityStage::ClangProject,
                    },
                )?;
                if environment.driver() != resolved.executable() {
                    return Err(PackageAuthorityError::ClangToolchainExecutableMismatch {
                        profile,
                    });
                }
                environment
                    .load_configured_libclang()
                    .map_err(PackageAuthorityError::ClangProject)?;
                let project = backend_frontend_clang::ClangProject::open_with_environment(
                    request.package_root,
                    request.source_path,
                    environment.clone(),
                )
                .map_err(PackageAuthorityError::ClangProject)?;
                PackageAuthorityOwner::Clang { profile, project }
            }
            LanguageProfile::TypeScript(profile) => {
                let checker = request.configuration.typescript.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::TypeScriptChecker,
                    },
                )?;
                validate_typescript_source_path(relative, profile)?;
                let report = checker
                    .run_in_package_at(profile, request.source, request.package_root, relative)
                    .map_err(PackageAuthorityError::TypeScript)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::TypeScriptChecker,
                )?;
                PackageAuthorityOwner::TypeScript { profile, report }
            }
            LanguageProfile::Python(profile) => {
                let syntax = extract(request.source, profile)
                    .map_err(PackageAuthorityError::PythonSyntax)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::PythonSyntax,
                )?;
                let report = match request.configuration.python_checker {
                    super::LocalRuntimePythonCheckerAdmission::Native { authority } => {
                        require_native_python_toolchain(authority, resolved.identity)?;
                        let relative_path = request
                            .source_path
                            .strip_prefix(request.package_root)
                            .expect("source root already admitted")
                            .to_str()
                            .ok_or_else(|| {
                                PackageAuthorityError::PythonPyrefly(PyreflyError::ProjectReport {
                                    path: request.source_path.to_path_buf(),
                                    message: "non-UTF8 selected module path".to_owned(),
                                })
                            })?;
                        let source = std::str::from_utf8(request.source).map_err(|_| {
                            PackageAuthorityError::PythonPyrefly(PyreflyError::ProjectReport {
                                path: request.source_path.to_path_buf(),
                                message: "non-UTF8 selected source".to_owned(),
                            })
                        })?;
                        let project = authority
                            .analyze_project(
                                request.package_root,
                                "selected",
                                &[PythonProjectSource {
                                    relative_path,
                                    source,
                                }],
                                profile,
                                PythonProjectControl {
                                    cancelled: request.control.cancelled,
                                    deadline: request.control.deadline,
                                },
                            )
                            .map_err(PackageAuthorityError::PythonPyrefly)?;
                        project
                            .module(relative_path)
                            .expect("selected native module")
                            .clone()
                    }
                    admission => require_python_checker(admission, request.profile)?
                        .analyze_in_package(request.source, profile, &syntax, request.package_root)
                        .map_err(PackageAuthorityError::PythonPyrefly)?,
                };
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::PythonPyrefly,
                )?;
                PackageAuthorityOwner::Python {
                    profile: request.profile,
                    report,
                }
            }
            LanguageProfile::Rust(profile) => {
                let configuration = request.configuration.rust.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::RustProject,
                    },
                )?;
                if configuration.toolchain.selected_rustc() != resolved.as_ref() {
                    return Err(PackageAuthorityError::RustToolchainExecutableMismatch {
                        profile: request.profile,
                        configured: configuration
                            .toolchain
                            .selected_rustc()
                            .to_path_buf()
                            .into_boxed_path(),
                        resolved: resolved.as_ref().to_path_buf().into_boxed_path(),
                    });
                }
                match request.unit_key {
                    CompilationUnitKeyV2::PackageRoot => {}
                    CompilationUnitKeyV2::RustCrate { root, .. } => {
                        let selected = request.package_root.join(root.as_ref());
                        if selected != request.source_path {
                            return Err(PackageAuthorityError::CompilationUnitSourceMismatch {
                                profile: request.profile,
                            });
                        }
                    }
                    _ => {
                        return Err(PackageAuthorityError::CompilationUnitMismatch {
                            profile: request.profile,
                        });
                    }
                }
                if request.source.len() > *configuration.maximum_source_bytes as usize {
                    return Err(PackageAuthorityError::RustProject(
                        RustAuthorityError::SourceBudget {
                            actual: u64::try_from(request.source.len()).unwrap_or(u64::MAX),
                            maximum: configuration.maximum_source_bytes,
                        },
                    ));
                }
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::RustProject,
                )?;
                let rust_toolchain =
                    configuration.resolve_toolchain(request.profile, request.control)?;
                if rust_toolchain.tool.as_path() != resolved.as_ref() {
                    return Err(PackageAuthorityError::RustToolchainExecutableMismatch {
                        profile: request.profile,
                        configured: rust_toolchain.tool.clone().into_boxed_path(),
                        resolved: resolved.as_ref().to_path_buf().into_boxed_path(),
                    });
                }
                if let Some(workspace) = rust_workspace {
                    workspace
                        .validate_binding(request.package_root, profile)
                        .map_err(PackageAuthorityError::RustProject)?;
                    checkpoint(
                        request.control,
                        request.profile,
                        PackageAuthorityStage::RustProject,
                    )?;
                    PackageAuthorityOwner::RustBorrowed {
                        profile: request.profile,
                        workspace,
                        maximum_source_bytes: configuration.maximum_source_bytes,
                    }
                } else {
                    let workspace = RustWorkspace::open_with_features_and_metadata_policy(
                        request.package_root,
                        rust_toolchain,
                        profile,
                        configuration.features,
                        configuration.metadata_policy,
                        RustAnalysisControl {
                            cancelled: request.control.cancelled,
                            maximum_source_bytes: configuration.maximum_source_bytes,
                            deadline: request.control.deadline,
                        },
                    )
                    .map_err(PackageAuthorityError::RustProject)?;
                    checkpoint(
                        request.control,
                        request.profile,
                        PackageAuthorityStage::RustProject,
                    )?;
                    PackageAuthorityOwner::Rust {
                        profile: request.profile,
                        workspace,
                        maximum_source_bytes: configuration.maximum_source_bytes,
                    }
                }
            }
            LanguageProfile::Go(_) => {
                let oracle = match request.configuration.go {
                    Some(oracle) => oracle,
                    None => {
                        if let Some(cause) = request.configuration.go_unavailable {
                            return Err(PackageAuthorityError::GoAuthorityUnavailable {
                                profile: request.profile,
                                cause,
                            });
                        }
                        return Err(PackageAuthorityError::AdapterUnavailable {
                            profile: request.profile,
                            stage: PackageAuthorityStage::GoOracle,
                        });
                    }
                };
                let owned_witness;
                let go_authority = match captured_go_authority {
                    Some(witness) => witness,
                    None => {
                        owned_witness = oracle.package_authority_witness_controlled(
                            request.package_root,
                            backend_frontend_go::legacy::GoOracleControl::new(
                                Some(request.control.cancelled),
                                request.control.deadline,
                            ),
                        )?;
                        &owned_witness
                    }
                };
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::GoOracle,
                )?;
                // Scoped to exactly the package that owns `source_path`:
                // sibling packages are import context only and are never
                // serialized, so a module whose subpackages share a
                // declaration spelling (e.g. `errgroup.Group` next to
                // `singleflight.Group` in `golang.org/x/sync`) cannot inject
                // a coordinate-free `DuplicateDeclarationIdentity` collision
                // into the selected package's image.
                let image = oracle
                    .authority_image_for_package_with_authority_witness_controlled(
                        request.source_path,
                        request.package_root,
                        go_authority,
                        backend_frontend_go::legacy::GoOracleControl::new(
                            Some(request.control.cancelled),
                            request.control.deadline,
                        ),
                    )
                    .map_err(PackageAuthorityError::GoOracle)?;
                let image = retain_image(
                    image,
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::GoOracle,
                )?;
                require_go_module_language(request.profile, &image)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::GoOracle,
                )?;
                PackageAuthorityOwner::Go {
                    profile: request.profile,
                    image,
                }
            }
            LanguageProfile::Java(profile) => {
                let configuration = request.configuration.java.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::JavaHarness,
                    },
                )?;
                let mut session = Harness::new().map_err(PackageAuthorityError::JavaHarness)?;
                session
                    .prepare(configuration.toolchain)
                    .map_err(PackageAuthorityError::JavaHarness)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::JavaHarness,
                )?;
                let source = [JavaSource {
                    name: relative,
                    bytes: request.source,
                }];
                let captured_sourcepath = request
                    .java_sourcepath
                    .iter()
                    .copied()
                    .filter(|dependency| dependency.name != relative)
                    .collect::<Vec<_>>();
                let mut image = Vec::new();
                session
                    .image_with_captured_sourcepath(
                        configuration.toolchain,
                        HarnessRequest {
                            sources: &source,
                            classpath: configuration.classpath,
                            release: profile,
                        },
                        &captured_sourcepath,
                        &mut image,
                    )
                    .map_err(PackageAuthorityError::JavaHarness)?;
                let image = retain_image(
                    image,
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::JavaHarness,
                )?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::JavaHarness,
                )?;
                PackageAuthorityOwner::Java {
                    profile: request.profile,
                    image,
                    _session: session,
                }
            }
            LanguageProfile::CSharp(profile) => {
                let configuration = request.configuration.csharp.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::CSharpRoslyn,
                    },
                )?;
                let project_file = match request.unit_key {
                    CompilationUnitKeyV2::PackageRoot => None,
                    CompilationUnitKeyV2::CSharpProject { project_path } => {
                        Some(request.package_root.join(project_path.as_ref()))
                    }
                    _ => {
                        return Err(PackageAuthorityError::CompilationUnitMismatch {
                            profile: request.profile,
                        });
                    }
                };
                let image = configuration
                    .producer
                    .produce(CSharpAuthorityRequest {
                        package_root: request.package_root,
                        source_path: request.source_path,
                        project_file: project_file.as_deref(),
                        source: request.source,
                        profile,
                        native_tool: resolved.tool,
                        toolchain: resolved.as_ref(),
                        control: CSharpAuthorityControl {
                            deadline: request.control.deadline,
                            cancelled: request.control.cancelled,
                        },
                        configuration: configuration.configuration,
                    })
                    .map_err(PackageAuthorityError::CSharp)?;
                let image = retain_image(
                    image.into_bytes().into_vec(),
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::CSharpRoslyn,
                )?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::CSharpRoslyn,
                )?;
                PackageAuthorityOwner::CSharp {
                    profile: request.profile,
                    image,
                }
            }
        };
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::Admission,
    )?;
    Ok(owner)
}

fn checkpoint(
    control: CompileControl<'_>,
    profile: LanguageProfile,
    stage: PackageAuthorityStage,
) -> Result<(), PackageAuthorityError> {
    if control.cancelled.load(Ordering::Acquire) {
        return Err(PackageAuthorityError::Cancelled { profile, stage });
    }
    if Instant::now() >= control.deadline {
        return Err(PackageAuthorityError::Deadline { profile, stage });
    }
    Ok(())
}

fn require_resolved_toolchain<'toolchain>(
    selection: ToolchainSelection<'toolchain>,
    profile: LanguageProfile,
) -> Result<ExternalToolchain<'toolchain>, PackageAuthorityError> {
    match selection {
        ToolchainSelection::ResolvedNative(resolved) => resolved
            .external()
            .map_err(|source| PackageAuthorityError::ExternalToolchainRequired { profile, source }),
        ToolchainSelection::ExplicitlyUnavailable { tool } => {
            Err(PackageAuthorityError::ToolchainUnavailable { profile, tool })
        }
    }
}

fn validate_typescript_source_path(
    relative: &Path,
    profile: TypeScriptSource,
) -> Result<(), PackageAuthorityError> {
    let safe_relative = !relative.as_os_str().is_empty()
        && !relative.is_absolute()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return Err(PackageAuthorityError::TypeScriptSourcePath {
            profile,
            source_relative: relative.to_path_buf().into_boxed_path(),
        });
    };
    let declaration = [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| name.ends_with(suffix));
    let extension_matches = declaration
        || match profile {
            TypeScriptSource::TypeScript => [".ts", ".mts", ".cts", ".js", ".mjs", ".cjs"]
                .iter()
                .any(|suffix| name.ends_with(suffix)),
            TypeScriptSource::Tsx => [".tsx", ".jsx"].iter().any(|suffix| name.ends_with(suffix)),
        };
    if safe_relative && extension_matches {
        return Ok(());
    }
    Err(PackageAuthorityError::TypeScriptSourcePath {
        profile,
        source_relative: relative.to_path_buf().into_boxed_path(),
    })
}

fn retain_image(
    image: Vec<u8>,
    maximum: usize,
    profile: LanguageProfile,
    stage: PackageAuthorityStage,
) -> Result<Box<[u8]>, PackageAuthorityError> {
    if image.len() > maximum {
        return Err(PackageAuthorityError::ImageTooLarge {
            profile,
            stage,
            actual: image.len(),
            maximum,
        });
    }
    Ok(image.into_boxed_slice())
}

/// Closed authority stage, used by cancellation and unavailable-adapter
/// terminals without collapsing them into a display string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageAuthorityStage {
    /// Request control and resolved toolchain admission.
    Admission,
    /// Explicit driver and loaded-libclang project admission.
    ClangProject,
    /// TypeScript package staging and checker execution.
    TypeScriptChecker,
    /// Python syntax extraction before pyrefly admission.
    PythonSyntax,
    /// Pyrefly semantic-type and resolution transaction.
    PythonPyrefly,
    /// Rust Cargo/rust-analyzer project admission.
    RustProject,
    /// Go package-oracle image production.
    GoOracle,
    /// Roslyn authority-image production.
    CSharpRoslyn,
    /// Java doclet preparation and authority-image extraction.
    JavaHarness,
}

/// Human spelling of a language profile in authority failure text.
struct ProfileName<'a>(&'a LanguageProfile);

impl std::fmt::Display for ProfileName<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use backend_semantic::vocabulary::Language;
        formatter.write_str(match self.0.language() {
            Language::Rust => "Rust",
            Language::TypeScript => "TypeScript",
            Language::Python => "Python",
            Language::Go => "Go",
            Language::Java => "Java",
            Language::CSharp => "C#",
            Language::Clang => "C/C++",
        })
    }
}

/// Human spelling of a native tool in authority failure text.
struct ToolName<'a>(&'a NativeTool);

impl std::fmt::Display for ToolName<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self.0 {
            NativeTool::Rustc => "rustc",
            NativeTool::Clang => "clang",
            NativeTool::Python => "python",
            NativeTool::TypeScriptCompiler => "tsc",
            NativeTool::GoCompiler => "go",
            NativeTool::JavaCompiler => "javac",
            NativeTool::CSharpCompiler => "dotnet",
        })
    }
}

impl std::fmt::Display for PackageAuthorityStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Admission => "admitting the request",
            Self::ClangProject => "opening the C/C++ project",
            Self::TypeScriptChecker => "checking the TypeScript project",
            Self::PythonSyntax => "reading Python syntax",
            Self::PythonPyrefly => "running the Python checker",
            Self::RustProject => "loading the Cargo project",
            Self::GoOracle => "loading the Go package",
            Self::CSharpRoslyn => "running the C# compiler",
            Self::JavaHarness => "running the Java compiler",
        })
    }
}

/// Exact package-authority admission terminal.
#[derive(Debug, Error)]
pub enum PackageAuthorityError {
    /// This adapter requires a child-process authority rather than a linked checker.
    #[error("external compiler invocation required for {profile:?}: {source}")]
    ExternalToolchainRequired {
        /// Requested language profile.
        profile: LanguageProfile,
        /// Linked authority has no executable capability.
        #[source]
        source: NotExternalToolchain,
    },
    /// The caller's native tool selection was explicitly unavailable.
    #[error("{} package authority cannot enter because {} is not available", ProfileName(.profile), ToolName(.tool))]
    ToolchainUnavailable {
        /// Requested profile.
        profile: LanguageProfile,
        /// Exact unavailable native tool.
        tool: NativeTool,
    },
    /// Cancellation was observed at an exact package authority checkpoint.
    #[error("{} package authority was cancelled while {}", ProfileName(.profile), .stage)]
    Cancelled {
        /// Requested profile.
        profile: LanguageProfile,
        /// Checkpoint that observed cancellation.
        stage: PackageAuthorityStage,
    },
    /// Deadline was observed at an exact package authority checkpoint.
    #[error("{} package authority reached its deadline while {}", ProfileName(.profile), .stage)]
    Deadline {
        /// Requested profile.
        profile: LanguageProfile,
        /// Checkpoint that observed the deadline.
        stage: PackageAuthorityStage,
    },
    /// The selected profile requires a producer absent from explicit config.
    #[error("no {} package authority is configured for {}", ProfileName(.profile), .stage)]
    AdapterUnavailable {
        /// Requested profile.
        profile: LanguageProfile,
        /// Exact missing producer stage.
        stage: PackageAuthorityStage,
    },
    /// A specifically named native or auxiliary producer is missing or failed its version probe.
    #[error("{} is required for {} but {}; set {}", .issue.executable(), ProfileName(.profile), match .issue.failure { CompilerToolFailure::Missing => "is not configured", CompilerToolFailure::ProbeFailed => "failed its version probe" }, .issue.configuration_variable())]
    RequiredTool {
        /// Requested profile.
        profile: LanguageProfile,
        /// Exact missing or unadmitted producer stage.
        stage: PackageAuthorityStage,
        /// Exact required executable and bounded admission failure.
        issue: CompilerToolIssue,
    },
    /// The selected source path was not beneath the explicitly selected root.
    #[error(
        "{} source {} is outside the package root {}", ProfileName(.profile), .source_path.display(), .package_root.display()
    )]
    SourceOutsidePackage {
        /// Requested profile.
        profile: LanguageProfile,
        /// Explicit package root.
        package_root: Box<Path>,
        /// Explicit source path that failed containment.
        source_path: Box<Path>,
    },
    /// A witnessed Go manifest, workspace, or local target changed after dispatch.
    #[error("the Go module, workspace or dependency files changed while indexing {}; retry", ProfileName(.profile))]
    GoAuthorityInputsChanged {
        /// Requested profile.
        profile: LanguageProfile,
    },
    /// A selected native module requires language semantics beyond the requested profile.
    #[error("Go module requires {observed}, beyond the selected {} language profile", ProfileName(.profile))]
    GoModuleLanguage {
        /// Exact requested compiler profile.
        profile: LanguageProfile,
        /// Actual module requirement retained by the native authority.
        observed: String,
    },
    /// The exact unit key is not implemented for this language authority.
    #[error("the compilation unit is incompatible with {} package authority", ProfileName(.profile))]
    CompilationUnitMismatch {
        /// Requested profile.
        profile: LanguageProfile,
    },
    /// A Rust crate key selected a source other than its exact crate root.
    #[error("the {} source is not its crate root", ProfileName(.profile))]
    CompilationUnitSourceMismatch {
        /// Requested Rust profile.
        profile: LanguageProfile,
    },
    /// The selected TypeScript grammar does not match the exact source path.
    #[error(
        "TypeScript source path {} is unsafe or incompatible with its grammar ({})", .source_relative.display(), match .profile { TypeScriptSource::TypeScript => "TypeScript", TypeScriptSource::Tsx => "TSX" }
    )]
    TypeScriptSourcePath {
        /// Requested TypeScript grammar profile.
        profile: TypeScriptSource,
        /// Exact source path relative to that root.
        source_relative: Box<Path>,
    },
    /// An externally produced authority image exceeded the owner's retained bound.
    #[error(
        "{} {} produced {} bytes; the retained maximum is {}", ProfileName(.profile), .stage, .actual, .maximum
    )]
    ImageTooLarge {
        /// Requested profile.
        profile: LanguageProfile,
        /// Producing authority stage.
        stage: PackageAuthorityStage,
        /// Exact returned image size.
        actual: usize,
        /// Explicit retention bound.
        maximum: usize,
    },
    /// The retained rust-analyzer project was configured for a compiler other
    /// than the exact caller-resolved compiler that will lower the same source.
    #[error(
        "the Rust analyzer toolchain {} does not match the resolved compiler {} for {}", .configured.display(), .resolved.display(), ProfileName(.profile)
    )]
    RustToolchainExecutableMismatch {
        /// Requested Rust profile.
        profile: LanguageProfile,
        /// Compiler executable already bound into the rust-analyzer project.
        configured: Box<Path>,
        /// Compiler executable selected for the enclosing driver request.
        resolved: Box<Path>,
    },
    /// A selected deferred Rust authority failed only when a Rust package requested it.
    #[error("Rust toolchain admission for {} failed: {}", ProfileName(.profile), .cause.explanation())]
    RustToolchainAdmission {
        /// Requested Rust profile.
        profile: LanguageProfile,
        /// Exact bounded deferred-admission outcome.
        cause: super::LocalRuntimeRustAuthorityFailure,
    },
    /// The fresh Rust workspace could not retain its exact Cargo resolution witness.
    #[error("the {} workspace has no complete Cargo resolution witness", ProfileName(.profile))]
    RustWorkspaceWitnessUnavailable {
        /// Requested Rust profile.
        profile: LanguageProfile,
    },
    /// The selected Clang driver differs from the executable bound by the driver.
    #[error("the {} authority driver differs from the resolved compiler", ProfileName(.profile))]
    ClangToolchainExecutableMismatch {
        /// Requested C-family profile.
        profile: LanguageProfile,
    },
    /// The resolved toolchain is not the domain-encoded identity of the admitted native producer.
    #[error(
        "compiled native Python toolchain identity differs: expected {expected:?}, observed {observed:?}"
    )]
    NativePythonToolchainIdentityMismatch {
        /// Expected domain-encoded toolchain identity derived from the admitted producer digest.
        expected: ContentId<ToolchainDomain>,
        /// Exact resolved toolchain identity selected by the runtime.
        observed: ContentId<ToolchainDomain>,
    },
    /// The package-aware TypeScript checker returned its exact terminal.
    #[error(transparent)]
    TypeScript(#[from] TypeScriptCheckerError),
    /// Native TSZ project admission returned its exact terminal.
    #[error(transparent)]
    TypeScriptTsz(#[from] backend_frontend_typescript::TszAuthorityError),
    /// The selected project's TypeScript compiler or module installation was rejected.
    #[error(transparent)]
    TypeScriptProjectHost(#[from] super::typescript_host::TypeScriptProjectHostError),
    /// Python syntax extraction returned its exact terminal.
    #[error(transparent)]
    PythonSyntax(#[from] ExtractionError),
    /// Pyrefly returned its exact semantic authority terminal.
    #[error(transparent)]
    PythonPyrefly(#[from] PyreflyError),
    /// Rust project admission returned its exact terminal.
    #[error(transparent)]
    RustProject(#[from] RustAuthorityError),
    /// Clang project admission returned its exact terminal.
    #[error(transparent)]
    ClangProject(#[from] backend_frontend_clang::ClangAuthorityError),
    /// Go authority-image production returned its exact terminal.
    #[error(transparent)]
    GoOracle(#[from] OracleError),
    /// The exact Go authority filesystem witness could not be captured or revalidated.
    #[error(transparent)]
    GoAuthorityWitness(#[from] GoPackageAuthorityWitnessError),
    /// A Go installation was selected but failed admission; unrelated language owners remain usable.
    #[error("Go package authority is unavailable: {}", .cause.explanation())]
    GoAuthorityUnavailable {
        /// Requested Go profile.
        profile: LanguageProfile,
        /// Retained Go-only failure category.
        cause: super::LocalRuntimeGoAuthorityFailure,
    },
    /// Roslyn authority-image production returned its exact terminal.
    #[error(transparent)]
    CSharp(#[from] CSharpAuthorityError),
    /// Java harness preparation or image extraction returned its exact terminal.
    #[error(transparent)]
    JavaHarness(#[from] HarnessError),
}

#[cfg(test)]
mod tests {
    #[test]
    fn go_native_module_requirement_cannot_fall_back_to_an_older_profile() {
        use backend_semantic::vocabulary::{GoVersion, LanguageProfile};
        let go125 = LanguageProfile::Go(GoVersion::Go125);
        let go126 = LanguageProfile::Go(GoVersion::Go126);
        assert!(super::admit_go_module_version(go126, b"1.26.2").is_ok());
        assert!(super::admit_go_module_version(go125, b"1.25.9").is_ok());
        assert!(super::admit_go_module_version(go126, b"1.24.3").is_ok());
        for (profile, version) in [(go125, b"1.26.2".as_slice()), (go126, b"1.27.0".as_slice())] {
            assert!(matches!(
                super::admit_go_module_version(profile, version),
                Err(super::PackageAuthorityError::GoModuleLanguage { profile: actual, .. }) if actual == profile
            ));
        }
    }

    use crate::driver::ResolvedToolchain;
    use crate::test_support::host_path;
    use std::{
        path::Path,
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    use backend_frontend_go::legacy::{GoOracle, GoOracleConfiguration};
    use backend_frontend_typescript::legacy::Checker as TypeScriptChecker;
    use backend_semantic::vocabulary::{CSharpVersion, CStandard, GoVersion};

    use super::*;

    fn configuration() -> PackageAuthorityConfiguration<'static> {
        PackageAuthorityConfiguration {
            clang: None,
            typescript: None,
            typescript_project_host: None,
            python_checker: crate::application::LocalRuntimePythonCheckerAdmission::Unconfigured,
            rust: None,
            go: None,
            go_unavailable: None,
            csharp: None,
            java: None,
            maximum_image_bytes: 0,
        }
    }

    #[test]
    fn package_typescript_authority_admits_nested_javascript_roots_only_as_ts() {
        let selected = Path::new("src/beacon.js");
        assert!(validate_typescript_source_path(selected, TypeScriptSource::TypeScript).is_ok());
        assert!(matches!(
            validate_typescript_source_path(selected, TypeScriptSource::Tsx),
            Err(PackageAuthorityError::TypeScriptSourcePath { .. })
        ));
        assert!(matches!(
            validate_typescript_source_path(
                Path::new("../beacon.js"),
                TypeScriptSource::TypeScript
            ),
            Err(PackageAuthorityError::TypeScriptSourcePath { .. })
        ));
    }

    #[test]
    fn retained_package_configuration_accepts_only_explicit_ts_and_go_authorities() {
        let typescript = TypeScriptChecker::default()
            .with_node(
                host_path("/configured/node").to_path_buf(),
                host_path("/configured/lib/node_modules").to_path_buf(),
            )
            .expect("absolute Node runtime is admissible");
        let go = GoOracle::default().with_configuration(
            GoOracleConfiguration::go_toolchain(host_path("/configured/go").to_path_buf())
                .expect("absolute Go toolchain is admissible"),
        );
        let configuration = PackageAuthorityConfiguration {
            clang: None,
            typescript: Some(&typescript),
            typescript_project_host: None,
            python_checker: crate::application::LocalRuntimePythonCheckerAdmission::Unconfigured,
            rust: None,
            go: Some(&go),
            go_unavailable: None,
            csharp: None,
            java: None,
            maximum_image_bytes: 0,
        };
        assert!(configuration.typescript.is_some());
        assert!(configuration.go.is_some());
    }

    fn clang_toolchain() -> ResolvedToolchain<'static> {
        ResolvedToolchain::from_version(
            NativeTool::Clang,
            host_path("/configured/clang"),
            b"package-authority-test-toolchain",
        )
        .expect("absolute fixture executable is admissible")
    }

    fn csharp_toolchain() -> ResolvedToolchain<'static> {
        ResolvedToolchain::from_version(
            NativeTool::CSharpCompiler,
            host_path("/configured/dotnet"),
            b"package-authority-test-dotnet",
        )
        .expect("absolute fixture executable is admissible")
    }

    fn go_toolchain() -> ResolvedToolchain<'static> {
        ResolvedToolchain::from_version(
            NativeTool::GoCompiler,
            host_path("/configured/go"),
            b"package-authority-test-go",
        )
        .expect("absolute Go executable fixture is admissible")
    }

    #[test]
    fn retained_go_admission_failure_is_reported_only_at_go_package_use() {
        let root = std::env::temp_dir().join(format!(
            "nudox-go-unavailable-authority-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create package root");
        let source_path = root.join("main.go");
        let source = b"package example\n";
        std::fs::write(&source_path, source).expect("write Go source");
        let mut config = configuration();
        config.go_unavailable =
            Some(crate::application::LocalRuntimeGoAuthorityFailure::ToolchainIdentityUnavailable);
        let cancelled = AtomicBool::new(false);

        let result = enter_package_authority(PackageAuthorityRequest {
            package_root: &root,
            source_path: &source_path,
            source,
            java_sourcepath: &[],
            unit_key: &CompilationUnitKeyV2::PackageRoot,
            profile: LanguageProfile::Go(GoVersion::Go125),
            toolchain: ToolchainSelection::ResolvedNative(go_toolchain()),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: config,
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("Go package use must surface its retained typed admission failure"),
        };

        assert!(matches!(
            error,
            PackageAuthorityError::GoAuthorityUnavailable {
                profile: LanguageProfile::Go(GoVersion::Go125),
                cause:
                    crate::application::LocalRuntimeGoAuthorityFailure::ToolchainIdentityUnavailable,
            }
        ));
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn clang_requires_an_explicit_project_authority() {
        let root =
            std::env::temp_dir().join(format!("nudox-package-authority-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp package root is creatable");
        let entry = root.join("main.c");
        std::fs::write(&entry, b"int main(void) { return 0; }").expect("entry is writable");
        let cancelled = AtomicBool::new(false);
        let error = enter_package_authority(PackageAuthorityRequest {
            package_root: &root,
            source_path: &entry,
            source: b"int main(void) { return 0; }",
            java_sourcepath: &[],
            unit_key: &CompilationUnitKeyV2::PackageRoot,
            profile: LanguageProfile::C(CStandard::C11),
            toolchain: ToolchainSelection::ResolvedNative(clang_toolchain()),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: configuration(),
        })
        .err()
        .expect("C authority requires explicit selected Clang environment");
        assert!(matches!(
            error,
            PackageAuthorityError::AdapterUnavailable {
                profile: LanguageProfile::C(CStandard::C11),
                stage: PackageAuthorityStage::ClangProject
            }
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancellation_is_retained_before_any_adapter_admission() {
        let cancelled = AtomicBool::new(true);
        let error = match enter_package_authority(PackageAuthorityRequest {
            package_root: Path::new("/packages/example"),
            source_path: Path::new("/packages/example/main.c"),
            source: b"int main(void) { return 0; }",
            java_sourcepath: &[],
            unit_key: &CompilationUnitKeyV2::PackageRoot,
            profile: LanguageProfile::C(CStandard::C11),
            toolchain: ToolchainSelection::ResolvedNative(clang_toolchain()),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: configuration(),
        }) {
            Ok(_) => panic!("cancelled work cannot expose even a direct authority owner"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            PackageAuthorityError::Cancelled {
                profile: LanguageProfile::C(CStandard::C11),
                stage: PackageAuthorityStage::Admission,
            }
        ));
    }

    #[test]
    fn csharp_requires_the_typed_roslyn_producer_at_its_exact_stage() {
        let cancelled = AtomicBool::new(false);
        let error = match enter_package_authority(PackageAuthorityRequest {
            package_root: Path::new("/packages/example"),
            source_path: Path::new("/packages/example/Library.cs"),
            source: b"public sealed class Library {}",
            java_sourcepath: &[],
            unit_key: &CompilationUnitKeyV2::PackageRoot,
            profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
            toolchain: ToolchainSelection::ResolvedNative(csharp_toolchain()),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: configuration(),
        }) {
            Ok(_) => panic!("C# must never admit a source-only fallback authority"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            PackageAuthorityError::AdapterUnavailable {
                profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
                stage: PackageAuthorityStage::CSharpRoslyn,
            }
        ));
    }
}
