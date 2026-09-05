//! Owns package-scoped language authority until driver lowering consumes it.
//!
//! A [`compiler_driver::SemanticAuthorityInput`] only borrows its reports,
//! images, or project context.  This module is the deliberately non-
//! self-referential owner that keeps those values alive across the driver's
//! fused semantic transaction.  It never manufactures a source-only fallback
//! for a profile whose authoritative package adapter was unavailable.

use std::{path::Path, sync::atomic::Ordering, time::Instant};

use compiler_driver::{
    CompileControl, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection,
};
use compiler_languages_csharp::{
    CSharpAuthorityConfiguration, CSharpAuthorityControl, CSharpAuthorityError,
    CSharpAuthorityRequest, CSharpOracle,
};
use compiler_languages_go::{ConfiguredGoOracle, OracleError};
use compiler_languages_java::harness::{
    Harness, HarnessError, HarnessRequest, JavaSource, JdkToolchain,
};
use compiler_languages_python::{
    CheckerError as PyreflyError, CheckerReport as PythonReport, ExtractionError, Pyrefly, extract,
};
use compiler_languages_rust::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use compiler_languages_typescript::{
    CheckerError as TypeScriptCheckerError, ExplicitTypeScriptChecker, Report as TypeScriptReport,
};
use compiler_vocabulary::{LanguageProfile, NativeTool, TypeScriptSource};
use thiserror::Error;

/// Bounded, explicit package-authority adapters selected by the application
/// owner.  Every optional field names one independently configured producer;
/// absence is a typed terminal, never a default process lookup.
#[derive(Clone, Copy, Debug)]
pub struct PackageAuthorityConfiguration<'config> {
    /// TypeScript checker that stages the explicitly selected package root.
    pub typescript: Option<&'config ExplicitTypeScriptChecker>,
    /// Python pyrefly adapter that owns inferred-type and resolution facts.
    pub python: Option<&'config Pyrefly>,
    /// Rust Analyzer/Cargo authority configuration.
    pub rust: Option<RustPackageAuthorityConfiguration<'config>>,
    /// Go package oracle selected by the application owner.
    pub go: Option<&'config ConfiguredGoOracle>,
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
    /// C and C++ remain usable because libclang is entered directly by the
    /// driver. Every other package profile reaches a typed unavailable
    /// terminal unless its caller selects a configured authority table.
    pub const UNAVAILABLE: Self = Self {
        typescript: None,
        python: None,
        rust: None,
        go: None,
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
    /// Rust toolchain and sysroot already established for rust-analyzer.  Its
    /// executable must exactly match the request's resolved Rust compiler.
    pub toolchain: &'config RustToolchain,
    /// Exact source budget checked before Cargo graph loading.
    pub maximum_source_bytes: SourceByteLimit,
    /// Caller-selected Cargo feature policy.
    pub features: RustFeatureControl<'config>,
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
pub enum PackageAuthorityOwner<'config> {
    /// C or C++ compilation, where libclang is the driver's direct authority.
    Clang {
        /// Exact C-family profile admitted by the caller.
        profile: LanguageProfile,
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
    /// Cargo/rust-analyzer project state retained for direct driver entry.
    Rust {
        /// Exact checked profile.
        profile: LanguageProfile,
        /// Exact Cargo project and selected crate source.
        project: RustProject,
        /// Original bounded source admission policy.
        maximum_source_bytes: SourceByteLimit,
        /// Exact caller-selected Cargo feature policy.
        features: RustFeatureControl<'config>,
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
    /// driver.  C/C++ are the only profiles permitted to carry `None` here:
    /// their semantic authority is direct libclang collection in compilation.
    #[must_use]
    pub fn input(&self) -> SemanticAuthorityInput<'_> {
        match self {
            Self::Clang { .. } => SemanticAuthorityInput::None,
            Self::TypeScript { report, .. } => SemanticAuthorityInput::TypeScript { report },
            Self::Python { report, .. } => SemanticAuthorityInput::Python { report },
            Self::Rust {
                project,
                maximum_source_bytes,
                features,
                ..
            } => SemanticAuthorityInput::Rust {
                project,
                maximum_source_bytes: *maximum_source_bytes,
                features: *features,
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
            Self::Clang { profile }
            | Self::Python { profile, .. }
            | Self::Rust { profile, .. }
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
) -> Result<PackageAuthorityOwner<'config>, PackageAuthorityError> {
    checkpoint(
        request.control,
        request.profile,
        PackageAuthorityStage::Admission,
    )?;
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
                PackageAuthorityOwner::Clang { profile }
            }
            LanguageProfile::TypeScript(profile) => {
                let checker = request.configuration.typescript.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::TypeScriptChecker,
                    },
                )?;
                validate_typescript_entry(request.package_root, relative, profile)?;
                let report = checker
                    .run_in_package(profile, request.source, request.package_root)
                    .map_err(PackageAuthorityError::TypeScript)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::TypeScriptChecker,
                )?;
                PackageAuthorityOwner::TypeScript { profile, report }
            }
            LanguageProfile::Python(profile) => {
                let pyrefly = request.configuration.python.ok_or(
                    PackageAuthorityError::AdapterUnavailable {
                        profile: request.profile,
                        stage: PackageAuthorityStage::PythonPyrefly,
                    },
                )?;
                let syntax = extract(request.source, profile)
                    .map_err(PackageAuthorityError::PythonSyntax)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::PythonSyntax,
                )?;
                let report = pyrefly
                    .analyze(request.source, profile, &syntax)
                    .map_err(PackageAuthorityError::PythonPyrefly)?;
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
                if configuration.toolchain.tool.as_path() != resolved.as_ref() {
                    return Err(PackageAuthorityError::RustToolchainExecutableMismatch {
                        profile: request.profile,
                        configured: configuration.toolchain.tool.clone().into_boxed_path(),
                        resolved: resolved.as_ref().to_path_buf().into_boxed_path(),
                    });
                }
                let project = RustProject::open_with_source(
                    request.package_root,
                    request.source_path,
                    configuration.toolchain,
                    profile,
                )
                .map_err(PackageAuthorityError::RustProject)?;
                checkpoint(
                    request.control,
                    request.profile,
                    PackageAuthorityStage::RustProject,
                )?;
                PackageAuthorityOwner::Rust {
                    profile: request.profile,
                    project,
                    maximum_source_bytes: configuration.maximum_source_bytes,
                    features: configuration.features,
                }
            }
            LanguageProfile::Go(_) => {
                let oracle =
                    request
                        .configuration
                        .go
                        .ok_or(PackageAuthorityError::AdapterUnavailable {
                            profile: request.profile,
                            stage: PackageAuthorityStage::GoOracle,
                        })?;
                let image = oracle
                    .authority_image(request.source_path, request.package_root)
                    .map_err(PackageAuthorityError::GoOracle)?;
                let image = retain_image(
                    image,
                    request.configuration.maximum_image_bytes,
                    request.profile,
                    PackageAuthorityStage::GoOracle,
                )?;
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
                let mut image = Vec::new();
                session
                    .image(
                        configuration.toolchain,
                        HarnessRequest {
                            sources: &source,
                            classpath: configuration.classpath,
                            release: java_authority_release(profile),
                        },
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
                let image = configuration
                    .producer
                    .produce(CSharpAuthorityRequest {
                        package_root: request.package_root,
                        source_path: request.source_path,
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

const fn java_authority_release(
    release: compiler_vocabulary::JavaRelease,
) -> compiler_languages_java::JavaRelease {
    match release {
        compiler_vocabulary::JavaRelease::Java8 => compiler_languages_java::JavaRelease::Java8,
        compiler_vocabulary::JavaRelease::Java11 => compiler_languages_java::JavaRelease::Java11,
        compiler_vocabulary::JavaRelease::Java17 => compiler_languages_java::JavaRelease::Java17,
        compiler_vocabulary::JavaRelease::Java21 => compiler_languages_java::JavaRelease::Java21,
        compiler_vocabulary::JavaRelease::Java25 => compiler_languages_java::JavaRelease::Java25,
    }
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
) -> Result<ResolvedToolchain<'toolchain>, PackageAuthorityError> {
    match selection {
        ToolchainSelection::ResolvedNative(resolved) => Ok(resolved),
        ToolchainSelection::ExplicitlyUnavailable { tool } => {
            Err(PackageAuthorityError::ToolchainUnavailable { profile, tool })
        }
    }
}

fn validate_typescript_entry(
    package_root: &Path,
    relative: &Path,
    profile: TypeScriptSource,
) -> Result<(), PackageAuthorityError> {
    let expected = match profile {
        TypeScriptSource::TypeScript => Path::new("index.ts"),
        TypeScriptSource::Tsx => Path::new("index.tsx"),
    };
    if relative == expected {
        return Ok(());
    }
    Err(PackageAuthorityError::TypeScriptEntryPath {
        package_root: package_root.to_path_buf().into_boxed_path(),
        source_relative: relative.to_path_buf().into_boxed_path(),
        expected: expected.to_path_buf().into_boxed_path(),
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

/// Exact package-authority admission terminal.
#[derive(Debug, Error)]
pub enum PackageAuthorityError {
    /// The caller's native tool selection was explicitly unavailable.
    #[error("package authority for {profile:?} cannot enter because {tool:?} is unavailable")]
    ToolchainUnavailable {
        /// Requested profile.
        profile: LanguageProfile,
        /// Exact unavailable native tool.
        tool: NativeTool,
    },
    /// Cancellation was observed at an exact package authority checkpoint.
    #[error("package authority for {profile:?} was cancelled during {stage:?}")]
    Cancelled {
        /// Requested profile.
        profile: LanguageProfile,
        /// Checkpoint that observed cancellation.
        stage: PackageAuthorityStage,
    },
    /// Deadline was observed at an exact package authority checkpoint.
    #[error("package authority for {profile:?} reached its deadline during {stage:?}")]
    Deadline {
        /// Requested profile.
        profile: LanguageProfile,
        /// Checkpoint that observed the deadline.
        stage: PackageAuthorityStage,
    },
    /// The selected profile requires a producer absent from explicit config.
    #[error("package authority adapter for {profile:?} is unavailable during {stage:?}")]
    AdapterUnavailable {
        /// Requested profile.
        profile: LanguageProfile,
        /// Exact missing producer stage.
        stage: PackageAuthorityStage,
    },
    /// The selected source path was not beneath the explicitly selected root.
    #[error(
        "package authority source {source_path:?} is outside root {package_root:?} for {profile:?}"
    )]
    SourceOutsidePackage {
        /// Requested profile.
        profile: LanguageProfile,
        /// Explicit package root.
        package_root: Box<Path>,
        /// Explicit source path that failed containment.
        source_path: Box<Path>,
    },
    /// The current package-aware TypeScript adapter cannot preserve a source
    /// path other than its exact staged entry path.
    #[error(
        "TypeScript source {source_relative:?} is not the exact staged entry {expected:?} beneath {package_root:?}"
    )]
    TypeScriptEntryPath {
        /// Explicit package root.
        package_root: Box<Path>,
        /// Exact source path relative to that root.
        source_relative: Box<Path>,
        /// Only path the current package-aware adapter can stage faithfully.
        expected: Box<Path>,
    },
    /// An externally produced authority image exceeded the owner's retained bound.
    #[error(
        "package authority {stage:?} for {profile:?} produced {actual} bytes; maximum is {maximum}"
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
        "Rust authority toolchain {configured:?} does not match resolved compiler {resolved:?} for {profile:?}"
    )]
    RustToolchainExecutableMismatch {
        /// Requested Rust profile.
        profile: LanguageProfile,
        /// Compiler executable already bound into the rust-analyzer project.
        configured: Box<Path>,
        /// Compiler executable selected for the enclosing driver request.
        resolved: Box<Path>,
    },
    /// The package-aware TypeScript checker returned its exact terminal.
    #[error(transparent)]
    TypeScript(#[from] TypeScriptCheckerError),
    /// Python syntax extraction returned its exact terminal.
    #[error(transparent)]
    PythonSyntax(#[from] ExtractionError),
    /// Pyrefly returned its exact semantic authority terminal.
    #[error(transparent)]
    PythonPyrefly(#[from] PyreflyError),
    /// Rust project admission returned its exact terminal.
    #[error(transparent)]
    RustProject(#[from] RustAuthorityError),
    /// Go authority-image production returned its exact terminal.
    #[error(transparent)]
    GoOracle(#[from] OracleError),
    /// Roslyn authority-image production returned its exact terminal.
    #[error(transparent)]
    CSharp(#[from] CSharpAuthorityError),
    /// Java harness preparation or image extraction returned its exact terminal.
    #[error(transparent)]
    JavaHarness(#[from] HarnessError),
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    use compiler_languages_go::{GoOracle, GoOracleConfiguration};
    use compiler_languages_typescript::Checker as TypeScriptChecker;
    use compiler_vocabulary::{CSharpVersion, CStandard};

    use super::*;

    fn configuration() -> PackageAuthorityConfiguration<'static> {
        PackageAuthorityConfiguration {
            typescript: None,
            python: None,
            rust: None,
            go: None,
            csharp: None,
            java: None,
            maximum_image_bytes: 0,
        }
    }

    #[test]
    fn retained_package_configuration_accepts_only_explicit_ts_and_go_authorities() {
        let typescript = TypeScriptChecker::default()
            .with_node(
                Path::new("/configured/node").to_path_buf(),
                Path::new("/configured/lib/node_modules").to_path_buf(),
            )
            .expect("absolute Node runtime is admissible");
        let go = GoOracle::default().with_configuration(
            GoOracleConfiguration::go_toolchain(Path::new("/configured/go").to_path_buf())
                .expect("absolute Go toolchain is admissible"),
        );
        let configuration = PackageAuthorityConfiguration {
            typescript: Some(&typescript),
            python: None,
            rust: None,
            go: Some(&go),
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
            Path::new("/configured/clang"),
            b"package-authority-test-toolchain",
        )
        .expect("absolute fixture executable is admissible")
    }

    fn csharp_toolchain() -> ResolvedToolchain<'static> {
        ResolvedToolchain::from_version(
            NativeTool::CSharpCompiler,
            Path::new("/configured/dotnet"),
            b"package-authority-test-dotnet",
        )
        .expect("absolute fixture executable is admissible")
    }

    #[test]
    fn clang_is_the_only_direct_none_authority() {
        let cancelled = AtomicBool::new(false);
        let owner = enter_package_authority(PackageAuthorityRequest {
            package_root: Path::new("/packages/example"),
            source_path: Path::new("/packages/example/main.c"),
            source: b"int main(void) { return 0; }",
            profile: LanguageProfile::C(CStandard::C11),
            toolchain: ToolchainSelection::ResolvedNative(clang_toolchain()),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: configuration(),
        })
        .expect("direct libclang authority needs no borrowed sidecar");

        assert_eq!(owner.profile(), LanguageProfile::C(CStandard::C11));
        assert!(matches!(owner.input(), SemanticAuthorityInput::None));
    }

    #[test]
    fn cancellation_is_retained_before_any_adapter_admission() {
        let cancelled = AtomicBool::new(true);
        let error = match enter_package_authority(PackageAuthorityRequest {
            package_root: Path::new("/packages/example"),
            source_path: Path::new("/packages/example/main.c"),
            source: b"int main(void) { return 0; }",
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
