//! Owns package-scoped language authority until driver lowering consumes it.
//!
//! A [`crate::driver::SemanticAuthorityInput`] only borrows its reports,
//! images, or project context.  This module is the deliberately non-
//! self-referential owner that keeps those values alive across the driver's
//! fused semantic transaction.  It never manufactures a source-only fallback
//! for a profile whose authoritative package adapter was unavailable.

use std::path::Path;

use crate::driver::{
    CompileControl, SemanticAuthorityInput, ToolchainSelection,
};
use backend_frontend_csharp::legacy::{
    CSharpAuthorityConfiguration, CSharpAuthorityError, CSharpOracle,
};
use backend_frontend_go::legacy::{ConfiguredGoOracle, OracleError};
use backend_frontend_java::legacy::harness::{Harness, HarnessError, JdkToolchain};
use backend_frontend_python::legacy::{
    CheckerError as PyreflyError, CheckerReport as PythonReport, ExtractionError, Pyrefly,
};
use backend_frontend_rust::legacy::{
    RustAuthorityError, RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_frontend_typescript::legacy::{
    CheckerError as TypeScriptCheckerError, ExplicitTypeScriptChecker, Report as TypeScriptReport,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, TypeScriptSource};
use thiserror::Error;

mod enter;

pub use enter::enter_package_authority;

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
            Self::Clang { project, .. } => SemanticAuthorityInput::Clang { project },
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
            Self::Clang { profile, .. }
            | Self::Python { profile, .. }
            | Self::Rust { profile, .. }
            | Self::Go { profile, .. }
            | Self::CSharp { profile, .. }
            | Self::Java { profile, .. } => *profile,
            Self::TypeScript { profile, .. } => LanguageProfile::TypeScript(*profile),
        }
    }
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
    /// Clang project admission returned its exact terminal.
    #[error(transparent)]
    ClangProject(#[from] backend_frontend_clang::ClangAuthorityError),
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

    use backend_frontend_go::legacy::{GoOracle, GoOracleConfiguration};
    use backend_frontend_typescript::legacy::Checker as TypeScriptChecker;
    use backend_semantic::vocabulary::{CSharpVersion, CStandard};

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
    fn clang_is_the_only_project_carrying_authority() {
        let root =
            std::env::temp_dir().join(format!("nudox-package-authority-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp package root is creatable");
        let entry = root.join("main.c");
        std::fs::write(&entry, b"int main(void) { return 0; }").expect("entry is writable");
        let cancelled = AtomicBool::new(false);
        let owner = enter_package_authority(PackageAuthorityRequest {
            package_root: &root,
            source_path: &entry,
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
        assert!(matches!(
            owner.input(),
            SemanticAuthorityInput::Clang { .. }
        ));
        if let PackageAuthorityOwner::Clang { project, .. } = &owner {
            assert_eq!(
                project.entry(),
                entry.canonicalize().expect("entry canonicalizes")
            );
        }
        let _ = std::fs::remove_dir_all(&root);
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
