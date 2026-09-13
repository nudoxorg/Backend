//! Native executable and semantic-authority fixture construction.
//!
//! This module keeps producer setup, bounded helper invocation, and typed
//! local-unavailability causes separate from the source/output observation
//! oracle. No fallback bytes are fabricated when a producer is absent.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeUnavailableKind {
    MissingPath,
    MissingTool,
    Canonicalize,
    ProbeVersion,
    VersionRejected,
    EmptyVersion,
    Resolve,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeUnavailableCause {
    pub(super) tool: NativeTool,
    pub(super) kind: NativeUnavailableKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuthorityUnavailableCause {
    Native(NativeUnavailableCause),
    Source(SourceUnavailableCause),
    RustAuthority,
    GoOracle,
    JavaHarness,
    CSharpHelper,
    TypeScriptChecker,
    PythonChecker,
    ObserverUnavailable,
}

/// Closed source-inventory terminal. A missing package root or fixture is a
/// real input absence, never permission to substitute generated source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SourceUnavailableCause {
    pub(super) language: CorpusLanguage,
    pub(super) kind: SourceUnavailableKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceUnavailableKind {
    RootUnset,
    PackageDirectoryMissing,
    SourceFileMissing,
    RepositoryFixtureMissing,
    ReadFailure,
    SourceTooLarge,
    SourceTreeTooDeep,
    SourceDirectoryEntryCountExceeded,
    SourceFileCountExceeded,
}

#[derive(Debug, Error)]
pub(super) enum AuthorityBuildError {
    #[error("Rust native authority could not load its selected toolchain")]
    RustLoad(#[from] compiler_languages_rust::LoadError),
    #[error("Rust Cargo authority could not open its fixture")]
    RustProject(#[from] compiler_languages_rust::RustAuthorityError),
    #[error("Go authority image producer failed")]
    Go(#[from] compiler_languages_go::OracleError),
    #[error("Java authority harness failed")]
    Java(#[from] compiler_languages_java::harness::HarnessError),
    #[error("C# authority helper failed")]
    CSharp(#[from] CSharpHelperError),
    #[error("C# authority image validation failed")]
    CSharpImage(#[from] compiler_languages_csharp::ImageError),
    #[error("authority fixture filesystem phase {phase:?} failed")]
    Io {
        phase: AuditIoPhase,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub(super) enum CSharpHelperError {
    #[error("C# helper filesystem phase {phase:?} failed")]
    Io {
        phase: AuditIoPhase,
        #[source]
        source: io::Error,
    },
    #[error("C# helper could not start")]
    Spawn(#[source] io::Error),
    #[error("C# helper exceeded its bounded deadline")]
    Timeout,
    #[error("C# helper exited with {status}")]
    Command {
        status: ExitStatus,
        stderr: Box<[u8]>,
    },
    #[error("C# helper did not emit its authority image")]
    MissingImage,
    #[error("C# authority image exceeded the bounded {limit}-byte lease")]
    ImageLimit { limit: usize, observed: usize },
}

pub(super) struct ToolSlot {
    pub(super) tool: NativeTool,
    pub(super) host: Option<HostTool>,
    pub(super) cause: NativeUnavailableCause,
}

impl ToolSlot {
    pub(super) fn resolve(tool: NativeTool) -> Self {
        match HostTool::resolve(tool) {
            Ok(host) => Self {
                tool,
                host: Some(host),
                cause: NativeUnavailableCause {
                    tool,
                    kind: NativeUnavailableKind::Resolve,
                },
            },
            Err(error) => Self {
                tool,
                host: None,
                cause: native_unavailable(tool, &error),
            },
        }
    }

    pub(super) fn host(&self) -> Option<&HostTool> {
        self.host.as_ref()
    }
}

pub(super) struct HostTools {
    pub(super) rust: ToolSlot,
    pub(super) python: ToolSlot,
    pub(super) clang: ToolSlot,
    pub(super) typescript: ToolSlot,
    pub(super) go: ToolSlot,
    pub(super) java: ToolSlot,
    pub(super) csharp: ToolSlot,
}

impl HostTools {
    pub(super) fn resolve() -> Self {
        Self {
            rust: ToolSlot::resolve(NativeTool::Rustc),
            python: ToolSlot::resolve(NativeTool::Python),
            clang: ToolSlot::resolve(NativeTool::Clang),
            typescript: ToolSlot::resolve(NativeTool::TypeScriptCompiler),
            go: ToolSlot::resolve(NativeTool::GoCompiler),
            java: ToolSlot::resolve(NativeTool::JavaCompiler),
            csharp: ToolSlot::resolve(NativeTool::CSharpCompiler),
        }
    }
}

pub(super) struct ResolvedTools<'path> {
    pub(super) rust: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
    pub(super) python: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
    pub(super) clang: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
    pub(super) typescript: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
    pub(super) go: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
    pub(super) java: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
    pub(super) csharp: Result<ResolvedToolchain<'path>, NativeUnavailableCause>,
}

impl<'path> ResolvedTools<'path> {
    pub(super) fn from_hosts(hosts: &'path HostTools) -> Self {
        Self {
            rust: resolved(&hosts.rust),
            python: resolved(&hosts.python),
            clang: resolved(&hosts.clang),
            typescript: resolved(&hosts.typescript),
            go: resolved(&hosts.go),
            java: resolved(&hosts.java),
            csharp: resolved(&hosts.csharp),
        }
    }
}

fn resolved<'path>(
    slot: &'path ToolSlot,
) -> Result<ResolvedToolchain<'path>, NativeUnavailableCause> {
    let host = slot.host().ok_or(slot.cause)?;
    host.toolchain()
        .map_err(|error| native_unavailable(slot.tool, &error))
}

fn native_unavailable(requested: NativeTool, error: &NativeToolingError) -> NativeUnavailableCause {
    let (tool, kind) = match error {
        NativeToolingError::MissingPath { tool }
        | NativeToolingError::MissingTool { tool }
        | NativeToolingError::Canonicalize { tool, .. }
        | NativeToolingError::ProbeVersion { tool, .. }
        | NativeToolingError::VersionRejected { tool, .. }
        | NativeToolingError::EmptyVersion { tool } => {
            let kind = match error {
                NativeToolingError::MissingPath { .. } => NativeUnavailableKind::MissingPath,
                NativeToolingError::MissingTool { .. } => NativeUnavailableKind::MissingTool,
                NativeToolingError::Canonicalize { .. } => NativeUnavailableKind::Canonicalize,
                NativeToolingError::ProbeVersion { .. } => NativeUnavailableKind::ProbeVersion,
                NativeToolingError::VersionRejected { .. } => {
                    NativeUnavailableKind::VersionRejected
                }
                NativeToolingError::EmptyVersion { .. } => NativeUnavailableKind::EmptyVersion,
                NativeToolingError::Resolve(_) => NativeUnavailableKind::Resolve,
                NativeToolingError::CreateWork(_)
                | NativeToolingError::InspectWork(_)
                | NativeToolingError::WorkNotEmpty => NativeUnavailableKind::Resolve,
            };
            (*tool, kind)
        }
        NativeToolingError::Resolve(_) => (requested, NativeUnavailableKind::Resolve),
        NativeToolingError::CreateWork(_)
        | NativeToolingError::InspectWork(_)
        | NativeToolingError::WorkNotEmpty => (requested, NativeUnavailableKind::Resolve),
    };
    NativeUnavailableCause { tool, kind }
}

pub(super) fn native_slot<'path>(
    language: CorpusLanguage,
    resolved: &'path ResolvedTools<'path>,
) -> Result<(LanguageProfile, ResolvedToolchain<'path>), NativeUnavailableCause> {
    match language {
        CorpusLanguage::Rust => resolved
            .rust
            .as_ref()
            .map(|tool| (LanguageProfile::Rust(RustEdition::Rust2024), *tool))
            .map_err(|cause| *cause),
        CorpusLanguage::TypeScript => resolved
            .typescript
            .as_ref()
            .map(|tool| {
                (
                    LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                    *tool,
                )
            })
            .map_err(|cause| *cause),
        CorpusLanguage::Python => resolved
            .python
            .as_ref()
            .map(|tool| (LanguageProfile::Python(PythonVersion::Python314), *tool))
            .map_err(|cause| *cause),
        CorpusLanguage::Go => resolved
            .go
            .as_ref()
            .map(|tool| (LanguageProfile::Go(GoVersion::Go125), *tool))
            .map_err(|cause| *cause),
        CorpusLanguage::Java => resolved
            .java
            .as_ref()
            .map(|tool| (LanguageProfile::Java(JavaRelease::Java21), *tool))
            .map_err(|cause| *cause),
        CorpusLanguage::CSharp => resolved
            .csharp
            .as_ref()
            .map(|tool| (LanguageProfile::CSharp(CSharpVersion::CSharp14), *tool))
            .map_err(|cause| *cause),
        CorpusLanguage::Clang => resolved
            .clang
            .as_ref()
            .map(|tool| (LanguageProfile::Cxx(CxxStandard::Cxx23), *tool))
            .map_err(|cause| *cause),
    }
}

pub(super) const fn native_tool(language: CorpusLanguage) -> NativeTool {
    match language {
        CorpusLanguage::Rust => NativeTool::Rustc,
        CorpusLanguage::TypeScript => NativeTool::TypeScriptCompiler,
        CorpusLanguage::Python => NativeTool::Python,
        CorpusLanguage::Go => NativeTool::GoCompiler,
        CorpusLanguage::Java => NativeTool::JavaCompiler,
        CorpusLanguage::CSharp => NativeTool::CSharpCompiler,
        CorpusLanguage::Clang => NativeTool::Clang,
    }
}

pub(super) struct RustAuthorityFixture {
    pub(super) _root: FixtureDir,
    pub(super) project: compiler_languages_rust::RustProject,
    pub(super) features: compiler_languages_rust::RustFeatureControl<'static>,
}

pub(super) struct GoAuthorityFixture {
    pub(super) _root: FixtureDir,
    pub(super) image: Vec<u8>,
}

pub(super) struct JavaAuthorityProvider {
    jdk: compiler_languages_java::harness::JdkToolchain<'static>,
    harness: compiler_languages_java::harness::Harness,
}

pub(super) struct CSharpAuthorityProvider {
    _root: FixtureDir,
    dotnet: PathBuf,
    oracle: PathBuf,
}

pub(super) enum ProviderSlot<T> {
    Ready(T),
    Unavailable(AuthorityUnavailableCause),
}

pub(super) struct AuthorityFactory {
    pub(super) java: ProviderSlot<JavaAuthorityProvider>,
    pub(super) csharp: ProviderSlot<CSharpAuthorityProvider>,
}

impl AuthorityFactory {
    pub(super) fn new(hosts: &HostTools) -> Result<Self, AuthorityBuildError> {
        let java = match hosts.java.host() {
            Some(host) => match JavaAuthorityProvider::new(host) {
                Ok(provider) => ProviderSlot::Ready(provider),
                Err(error) if java_setup_is_unavailable(&error) => {
                    ProviderSlot::Unavailable(AuthorityUnavailableCause::JavaHarness)
                }
                Err(error) => return Err(error),
            },
            None => ProviderSlot::Unavailable(AuthorityUnavailableCause::Native(hosts.java.cause)),
        };
        let csharp = match hosts.csharp.host() {
            Some(host) => match CSharpAuthorityProvider::new(host) {
                Ok(provider) => ProviderSlot::Ready(provider),
                Err(error) if csharp_setup_is_unavailable(&error) => {
                    ProviderSlot::Unavailable(AuthorityUnavailableCause::CSharpHelper)
                }
                Err(error) => return Err(error),
            },
            None => {
                ProviderSlot::Unavailable(AuthorityUnavailableCause::Native(hosts.csharp.cause))
            }
        };
        Ok(Self { java, csharp })
    }
}

impl JavaAuthorityProvider {
    fn new(host: &HostTool) -> Result<Self, AuthorityBuildError> {
        let root = host
            .executable()
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| AuthorityBuildError::Io {
                phase: AuditIoPhase::Fixture,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "javac executable has no JDK root",
                ),
            })?
            .to_owned();
        let root = Box::leak(root.into_boxed_path());
        let jdk = compiler_languages_java::harness::JdkToolchain::new(root)?;
        let mut harness = compiler_languages_java::harness::Harness::new()?;
        harness.prepare(&jdk)?;
        Ok(Self { jdk, harness })
    }

    pub(super) fn image(&self, source: &[u8]) -> Result<Vec<u8>, AuthorityBuildError> {
        let sources = [compiler_languages_java::harness::JavaSource {
            name: Path::new("Package.java"),
            bytes: source,
        }];
        let request = compiler_languages_java::harness::HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: compiler_languages_java::JavaRelease::Java21,
        };
        let mut image = Vec::with_capacity(64 * 1024);
        self.harness.image(&self.jdk, request, &mut image)?;
        Ok(image)
    }
}

impl CSharpAuthorityProvider {
    fn new(host: &HostTool) -> Result<Self, AuthorityBuildError> {
        let root =
            FixtureDir::new("csharp-provider").map_err(|source| AuthorityBuildError::Io {
                phase: AuditIoPhase::Fixture,
                source,
            })?;
        let output = root.child("publish");
        fs::create_dir(&output).map_err(|source| AuthorityBuildError::Io {
            phase: AuditIoPhase::CSharpPublish,
            source,
        })?;
        let helper =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../compiler-language-csharp/helper");
        let mut command = Command::new(host.executable());
        command
            .args([
                "publish",
                "oracle.csproj",
                "-c",
                "Release",
                "--nologo",
                "--no-restore",
                "-o",
            ])
            .arg(&output)
            .current_dir(&helper);
        run_bounded_command(command, DEADLINE).map_err(AuthorityBuildError::CSharp)?;
        let oracle = output.join("oracle.dll");
        if !oracle.is_file() {
            return Err(AuthorityBuildError::CSharp(CSharpHelperError::MissingImage));
        }
        Ok(Self {
            _root: root,
            dotnet: host.executable().to_owned(),
            oracle,
        })
    }

    pub(super) fn image(
        &self,
        package: CorpusPackage,
        source: &[u8],
    ) -> Result<Vec<u8>, AuthorityBuildError> {
        self.image_for_source(package.case_id.raw(), source)
    }

    pub(super) fn image_for_source(
        &self,
        case_id: u16,
        source: &[u8],
    ) -> Result<Vec<u8>, AuthorityBuildError> {
        let root = self._root.child(&format!("case-{case_id}"));
        fs::create_dir(&root).map_err(|source| {
            AuthorityBuildError::CSharp(CSharpHelperError::Io {
                phase: AuditIoPhase::CSharpSource,
                source,
            })
        })?;
        let source_path = root.join("Package.cs");
        fs::write(&source_path, source).map_err(|source| {
            AuthorityBuildError::CSharp(CSharpHelperError::Io {
                phase: AuditIoPhase::CSharpSource,
                source,
            })
        })?;
        let image_path = root.join("authority.image");
        let assembly = format!("Corpus{case_id}");
        let mut command = Command::new(&self.dotnet);
        command
            .arg("exec")
            .arg(&self.oracle)
            .arg("--mode")
            .arg("source")
            .arg("--assembly-name")
            .arg(&assembly)
            .arg("--authority-image")
            .arg("--source-binding")
            .arg(&source_path)
            .arg("--out")
            .arg(&image_path)
            .arg("--root")
            .arg(&root);
        run_bounded_command(command, DEADLINE).map_err(AuthorityBuildError::CSharp)?;
        let metadata = fs::metadata(&image_path).map_err(|source| {
            AuthorityBuildError::CSharp(CSharpHelperError::Io {
                phase: AuditIoPhase::CSharpImage,
                source,
            })
        })?;
        let observed = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if observed > CSHARP_IMAGE_BYTES {
            return Err(AuthorityBuildError::CSharp(CSharpHelperError::ImageLimit {
                limit: CSHARP_IMAGE_BYTES,
                observed,
            }));
        }
        let image = fs::read(&image_path).map_err(|source| {
            AuthorityBuildError::CSharp(CSharpHelperError::Io {
                phase: AuditIoPhase::CSharpImage,
                source,
            })
        })?;
        compiler_languages_csharp::CSharpImage::open(&image)?;
        Ok(image)
    }
}

fn java_setup_is_unavailable(error: &AuthorityBuildError) -> bool {
    matches!(
        error,
        AuthorityBuildError::Java(
            compiler_languages_java::harness::HarnessError::MissingExecutable { .. }
                | compiler_languages_java::harness::HarnessError::Spawn { .. }
                | compiler_languages_java::harness::HarnessError::Command { .. }
        )
    )
}

fn csharp_setup_is_unavailable(error: &AuthorityBuildError) -> bool {
    matches!(
        error,
        AuthorityBuildError::CSharp(
            CSharpHelperError::Spawn(_)
                | CSharpHelperError::Timeout
                | CSharpHelperError::Command { .. }
                | CSharpHelperError::MissingImage
        )
    )
}

pub(super) fn rust_error_is_unavailable(error: &AuthorityBuildError) -> bool {
    matches!(error, AuthorityBuildError::RustLoad(_))
}

pub(super) fn go_error_is_unavailable(error: &compiler_languages_go::OracleError) -> bool {
    matches!(
        error,
        compiler_languages_go::OracleError::ToolingUnavailable { .. }
            | compiler_languages_go::OracleError::Spawn { .. }
            | compiler_languages_go::OracleError::Timeout { .. }
    )
}

pub(super) fn rust_fixture(
    package: CorpusPackage,
    source: &[u8],
    host: &HostTool,
) -> Result<RustAuthorityFixture, AuthorityBuildError> {
    rust_fixture_for_source(package.case_id.raw(), source, host)
}

pub(super) fn rust_fixture_for_source(
    case_id: u16,
    source: &[u8],
    host: &HostTool,
) -> Result<RustAuthorityFixture, AuthorityBuildError> {
    let root = FixtureDir::new("rust-case").map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::Fixture,
        source,
    })?;
    let source_dir = root.child("src");
    fs::create_dir(&source_dir).map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::RustManifest,
        source,
    })?;
    fs::write(
        root.child("Cargo.toml"),
        b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::RustManifest,
        source,
    })?;
    let source_path = source_dir.join("lib.rs");
    fs::write(&source_path, source).map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::RustSource,
        source,
    })?;
    let toolchain = compiler_languages_rust::RustToolchain::discover(host.executable().to_owned())
        .map_err(AuthorityBuildError::RustLoad)?;
    let project = compiler_languages_rust::RustProject::open_with_source(
        &root.path,
        &source_path,
        &toolchain,
        RustEdition::Rust2024,
    )
    .map_err(AuthorityBuildError::RustProject)?;
    Ok(RustAuthorityFixture {
        _root: root,
        project,
        features: compiler_languages_rust::RustFeatureControl::default(),
    })
}

pub(super) fn go_fixture(
    package: CorpusPackage,
    source: &[u8],
) -> Result<GoAuthorityFixture, AuthorityBuildError> {
    go_fixture_for_source(package.case_id.raw(), source)
}

pub(super) fn go_fixture_for_source(
    case_id: u16,
    source: &[u8],
) -> Result<GoAuthorityFixture, AuthorityBuildError> {
    let root = FixtureDir::new("go-case").map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::Fixture,
        source,
    })?;
    fs::write(root.child("go.mod"), b"module fixture\n\ngo 1.23\n").map_err(|source| {
        AuthorityBuildError::Io {
            phase: AuditIoPhase::GoManifest,
            source,
        }
    })?;
    let source_path = root.child("package.go");
    fs::write(&source_path, source).map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::GoSource,
        source,
    })?;
    let oracle = compiler_languages_go::GoOracle {
        output_limit: AUTHORITY_BYTES,
        timeout: DEADLINE,
    };
    let image = oracle
        .authority_image(&source_path, &root.path)
        .map_err(AuthorityBuildError::Go)?;
    Ok(GoAuthorityFixture { _root: root, image })
}

fn run_bounded_command(mut command: Command, deadline: Duration) -> Result<(), CSharpHelperError> {
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(CSharpHelperError::Spawn)?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CSharpHelperError::Timeout);
            }
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CSharpHelperError::Spawn(source));
            }
        }
    };
    let mut stderr = Vec::new();
    if let Some(pipe) = child.stderr.take() {
        pipe.take((CSHARP_DIAGNOSTIC_BYTES + 1) as u64)
            .read_to_end(&mut stderr)
            .map_err(|source| CSharpHelperError::Io {
                phase: AuditIoPhase::CSharpPublish,
                source,
            })?;
    }
    if stderr.len() > CSHARP_DIAGNOSTIC_BYTES {
        stderr.truncate(CSHARP_DIAGNOSTIC_BYTES);
    }
    if status.success() {
        Ok(())
    } else {
        Err(CSharpHelperError::Command {
            status,
            stderr: stderr.into_boxed_slice(),
        })
    }
}
