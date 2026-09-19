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
    /// The row compiled, but one or more observer planes have no authority
    /// producer that can ground them on this lane. This is a typed
    /// not-compared outcome: counted separately from source/toolchain
    /// absence, never a parity mismatch, and never a fabricated success. A
    /// row carrying this cause is honestly not verified.
    NotCompared,
    /// The row's worker process died — by signal, nonzero exit, spawn fault,
    /// or hard wall-clock cap — without producing its typed outcome. This is
    /// the isolation disposition for the in-process native faults the audit
    /// previously suffered as a whole-run SIGSEGV: the row stays counted and
    /// visible, and the fault never reaches the other rows. It is a harness
    /// process fault, never a parity result.
    RowProcessCrash {
        /// Fatal signal number, `-1` when no process was ever started, `0`
        /// for a clean-exit or internal-error death.
        signal: i32,
        /// Process exit code, `-1` when the process was killed by a signal.
        code: i32,
    },
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
    RustLoad(#[from] backend_frontend_rust::legacy::LoadError),
    #[error("Rust Cargo authority could not open its fixture")]
    RustProject(#[from] backend_frontend_rust::legacy::RustAuthorityError),
    #[error("Go authority image producer failed")]
    Go(#[from] backend_frontend_go::legacy::OracleError),
    #[error("Java authority harness failed")]
    Java(#[from] backend_frontend_java::legacy::harness::HarnessError),
    #[error("C# authority helper failed")]
    CSharp(#[from] CSharpHelperError),
    #[error("C# authority image validation failed")]
    CSharpImage(#[from] backend_frontend_csharp::legacy::ImageError),
    #[error("Clang project authority could not open its entry translation unit")]
    Clang(#[from] backend_frontend_clang::ClangAuthorityError),
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
    pub(super) project: backend_frontend_rust::legacy::RustProject,
    pub(super) features: backend_frontend_rust::legacy::RustFeatureControl<'static>,
}

pub(super) struct GoAuthorityFixture {
    pub(super) _root: FixtureDir,
    pub(super) image: Vec<u8>,
}

pub(super) struct JavaAuthorityProvider {
    jdk: backend_frontend_java::legacy::harness::JdkToolchain<'static>,
    harness: backend_frontend_java::legacy::harness::Harness,
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

    /// A factory whose providers are typed-unavailable. The row worker builds
    /// this for languages whose row body never consults the Java or C#
    /// providers, so an isolated row never pays for the JVM harness or the
    /// `dotnet publish` it cannot reach.
    pub(super) fn absent() -> Self {
        Self {
            java: ProviderSlot::Unavailable(AuthorityUnavailableCause::JavaHarness),
            csharp: ProviderSlot::Unavailable(AuthorityUnavailableCause::CSharpHelper),
        }
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
        let jdk = backend_frontend_java::legacy::harness::JdkToolchain::new(root)?;
        let mut harness = backend_frontend_java::legacy::harness::Harness::new()?;
        harness.prepare(&jdk)?;
        Ok(Self { jdk, harness })
    }

    pub(super) fn image(&self, source: &[u8]) -> Result<Vec<u8>, AuthorityBuildError> {
        self.image_with_name(source, Path::new("Package.java"))
    }

    /// Extracts one image naming the source file after its real file name.
    /// javac requires a public top-level type to live in its own file, so a
    /// real package file keeps its exact name instead of the synthetic
    /// batch name.
    pub(super) fn image_with_name(
        &self,
        source: &[u8],
        name: &Path,
    ) -> Result<Vec<u8>, AuthorityBuildError> {
        let sources = [backend_frontend_java::legacy::harness::JavaSource {
            name,
            bytes: source,
        }];
        let request = backend_frontend_java::legacy::harness::HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: backend_frontend_java::legacy::JavaRelease::Java21,
        };
        let mut image = Vec::with_capacity(64 * 1024);
        self.harness.image(&self.jdk, request, &mut image)?;
        Ok(image)
    }

    /// Extracts one image for a real-package source by compiling the whole
    /// package source set: every `.java` file under `package_root` is passed
    /// explicitly (names relative to the root, selected file first so the
    /// source binding still names the audit's selected bytes), and javac gets
    /// a `-sourcepath` of the root plus same-group sibling artifact roots so
    /// cross-artifact imports already in the corpus can resolve.
    ///
    /// The emitted image intentionally carries the whole attributed package,
    /// matching the Go oracle's whole-module image: the source binding digest
    /// still proves the image was produced for the selected bytes, and the
    /// engine lowers the attributed facts rather than rescanning text. No
    /// declaration filtering is applied here. Sibling artifacts that are not
    /// in the corpus stay unresolvable; the extractor then refuses the row as
    /// a typed per-row terminal instead of a fabricated image.
    pub(super) fn image_with_package_source(
        &self,
        source: &[u8],
        selected_path: &Path,
        package_root: &Path,
    ) -> Result<Vec<u8>, AuthorityBuildError> {
        let mut files = Vec::new();
        collect_java_sources(package_root, &mut files).map_err(|source| {
            AuthorityBuildError::Io {
                phase: AuditIoPhase::Fixture,
                source,
            }
        })?;
        files.sort();
        let selected_relative = selected_path
            .strip_prefix(package_root)
            .map(Path::to_owned)
            .unwrap_or_else(|_| {
                selected_path
                    .file_name()
                    .map(Path::new)
                    .unwrap_or_else(|| Path::new("Package.java"))
                    .to_owned()
            });
        // Owned buffers behind the borrowed request: the selected file always
        // carries the audit's exact bytes so the image binds to them, while
        // siblings are read from the immutable corpus tree.
        let mut staged: Vec<(PathBuf, Vec<u8>)> = Vec::with_capacity(files.len().saturating_add(1));
        for file in &files {
            let relative = file
                .strip_prefix(package_root)
                .map(Path::to_owned)
                .unwrap_or_else(|_| {
                    file.file_name()
                        .map(Path::new)
                        .unwrap_or_else(|| Path::new("Package.java"))
                        .to_owned()
                });
            if file == selected_path || relative == selected_relative {
                continue;
            }
            let Ok(bytes) = fs::read(file) else {
                continue;
            };
            staged.push((relative, bytes));
        }
        let mut names: Vec<PathBuf> = Vec::with_capacity(staged.len().saturating_add(1));
        names.push(selected_relative);
        names.extend(staged.iter().map(|(name, _)| name.clone()));
        let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(names.len());
        buffers.push(source.to_vec());
        buffers.extend(staged.into_iter().map(|(_, bytes)| bytes));
        let sources: Vec<backend_frontend_java::legacy::harness::JavaSource<'_>> = names
            .iter()
            .zip(buffers.iter())
            .map(
                |(name, bytes)| backend_frontend_java::legacy::harness::JavaSource {
                    name: name.as_path(),
                    bytes: bytes.as_slice(),
                },
            )
            .collect();
        let request = backend_frontend_java::legacy::harness::HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: backend_frontend_java::legacy::JavaRelease::Java21,
        };
        let entries = java_sourcepath_entries(package_root);
        let roots: Vec<&Path> = entries.iter().map(PathBuf::as_path).collect();
        let mut image = Vec::with_capacity(64 * 1024);
        self.harness
            .image_with_sourcepath(&self.jdk, request, &roots, &mut image)?;
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
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontends/csharp/src/legacy/helper");
        // The no-restore publish below consumes exactly the locked closure in
        // packages.lock.json, so the assets must come from a locked restore:
        // the committed obj/ assets are no longer part of the source tree.
        let mut restore = Command::new(host.executable());
        restore
            .args(["restore", "oracle.csproj", "--locked-mode", "--nologo"])
            .current_dir(&helper);
        run_bounded_command(restore, DEADLINE).map_err(AuthorityBuildError::CSharp)?;
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
        self.extract_image(case_id, &root, &source_path, &root)
    }

    /// Extracts one source-bound image over the selected package's whole source
    /// set. The Roslyn loader collects every `.cs` under each `--root` so the
    /// bound tree binds against its siblings, but `AuthorityImage.Write` builds
    /// declarations from the source-binding `SyntaxTree` alone: sibling files
    /// are binding context, never image rows. A selected file whose entire body
    /// is preprocessor-gated (polyfill's `DefaultInterpolatedStringHandler.cs`
    /// is `#if HAS_SPAN && !NET6_0_OR_GREATER`, a gate the oracle's fixed
    /// environment never opens) therefore still yields zero declarations. The
    /// resolver selects a declaring sibling before this is called; see
    /// `inventory_resolve::is_csharp_conditional_shell`.
    pub(super) fn image_for_package_source(
        &self,
        case_id: u16,
        package_root: &Path,
        selected_path: &Path,
    ) -> Result<Vec<u8>, AuthorityBuildError> {
        let root = self._root.child(&format!("case-{case_id}"));
        fs::create_dir(&root).map_err(|source| {
            AuthorityBuildError::CSharp(CSharpHelperError::Io {
                phase: AuditIoPhase::CSharpSource,
                source,
            })
        })?;
        self.extract_image(case_id, package_root, selected_path, &root)
    }

    /// Runs the oracle over `source_root`, binding the image to
    /// `source_binding`, and writes the image under `output_dir`.
    fn extract_image(
        &self,
        case_id: u16,
        source_root: &Path,
        source_binding: &Path,
        output_dir: &Path,
    ) -> Result<Vec<u8>, AuthorityBuildError> {
        let image_path = output_dir.join("authority.image");
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
            .arg(source_binding)
            .arg("--out")
            .arg(&image_path)
            .arg("--root")
            .arg(source_root);
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
        backend_frontend_csharp::legacy::CSharpImage::open(&image)?;
        Ok(image)
    }
}

fn java_setup_is_unavailable(error: &AuthorityBuildError) -> bool {
    matches!(
        error,
        AuthorityBuildError::Java(
            backend_frontend_java::legacy::harness::HarnessError::MissingExecutable { .. }
                | backend_frontend_java::legacy::harness::HarnessError::Spawn { .. }
                | backend_frontend_java::legacy::harness::HarnessError::Command { .. }
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
    // A fixture the authority cannot open for this source (edition, budget,
    // workspace shape) is a per-row authority terminal, never a reason to
    // abort the audit. Filesystem plumbing failures stay hard errors.
    matches!(
        error,
        AuthorityBuildError::RustLoad(_) | AuthorityBuildError::RustProject(_)
    )
}

pub(super) fn go_error_is_unavailable(error: &backend_frontend_go::legacy::OracleError) -> bool {
    use backend_frontend_go::legacy::OracleError;
    matches!(
        error,
        OracleError::ToolingUnavailable { .. }
            | OracleError::Spawn { .. }
            | OracleError::Timeout { .. }
            // The oracle started and refused this source (package load
            // failure, missing siblings or dependencies) or the source
            // exceeded its bounded output: per-row terminals, not aborts.
            | OracleError::Exit { .. }
            | OracleError::OutputLimit { .. }
    )
}

/// A javac/doclet rejection of one source row. The authority ran and refused
/// the file (missing siblings, dependencies, or language level): a typed
/// per-row terminal. Plumbing and host faults stay hard errors.
pub(super) fn java_row_is_unavailable(error: &AuthorityBuildError) -> bool {
    matches!(
        error,
        AuthorityBuildError::Java(
            backend_frontend_java::legacy::harness::HarnessError::Command { .. }
        )
    )
}

/// An oracle rejection of one source row (nonzero exit, bounded deadline or
/// image limit exceeded). Spawn, plumbing, and missing-image faults stay
/// hard errors.
pub(super) fn csharp_row_is_unavailable(error: &AuthorityBuildError) -> bool {
    matches!(
        error,
        AuthorityBuildError::CSharp(
            CSharpHelperError::Command { .. }
                | CSharpHelperError::Timeout
                | CSharpHelperError::ImageLimit { .. }
        )
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
    let toolchain =
        backend_frontend_rust::legacy::RustToolchain::discover(host.executable().to_owned())
            .map_err(AuthorityBuildError::RustLoad)?;
    let project = backend_frontend_rust::legacy::RustProject::open_with_source(
        &root.path,
        &source_path,
        &toolchain,
        RustEdition::Rust2024,
    )
    .map_err(AuthorityBuildError::RustProject)?;
    Ok(RustAuthorityFixture {
        _root: root,
        project,
        features: backend_frontend_rust::legacy::RustFeatureControl::default(),
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
    let oracle = backend_frontend_go::legacy::GoOracle {
        output_limit: AUTHORITY_BYTES,
        timeout: DEADLINE,
    };
    let image = oracle
        .authority_image(&source_path, &root.path)
        .map_err(AuthorityBuildError::Go)?;
    Ok(GoAuthorityFixture { _root: root, image })
}

/// Stages the selected file's whole Go module for the real-package audit.
///
/// The fixture is rooted at the selected file's real module (`…/@version`),
/// carries a synthesized `go.mod` naming the real module path, and copies
/// every non-`_test.go` `.go` file of the selected package and of its
/// same-module import closure under the same relative paths. Subpackages are
/// therefore resolvable import context (the analogue of javac's
/// `-sourcepath`), while the oracle's package-selected mode serializes exactly
/// the selected package — sibling declarations can never enter the image and
/// collide with the selected package's declarations.
///
/// External imports whose module checkout exists under `$NUDOX_GO_CORPUS_DIR`
/// gain `require`/`replace` directives pointing at that checkout, so present
/// siblings resolve. Modules that are genuinely absent stay unresolved; the
/// oracle then refuses the row as a typed per-row terminal rather than a
/// fabricated image. The binding path always carries the audit's exact
/// selected bytes.
pub(super) fn go_fixture_for_real_source(
    _case_id: u16,
    selected_path: &Path,
    source: &[u8],
) -> Result<GoAuthorityFixture, AuthorityBuildError> {
    let root = FixtureDir::new("go-case").map_err(|source| AuthorityBuildError::Io {
        phase: AuditIoPhase::Fixture,
        source,
    })?;
    let staged = backend_frontend_go::legacy::stage_module(&root.path, selected_path, source)
        .map_err(|error| match error {
            backend_frontend_go::legacy::StagingError::Io(source) => AuthorityBuildError::Io {
                phase: AuditIoPhase::GoSource,
                source,
            },
        })?;
    let corpus_root = go_corpus_root();
    complete_staged_module_requirements(&staged.root, corpus_root.as_deref()).map_err(
        |source| AuthorityBuildError::Io {
            phase: AuditIoPhase::GoManifest,
            source,
        },
    )?;
    let oracle = backend_frontend_go::legacy::GoOracle {
        output_limit: AUTHORITY_BYTES,
        timeout: DEADLINE,
    };
    let image = oracle
        .authority_image_for_package(&staged.source, &staged.root)
        .map_err(AuthorityBuildError::Go)?;
    Ok(GoAuthorityFixture { _root: root, image })
}

/// How many corpus-completion rounds may grow the staged `go.mod` before the
/// pass stops. The corpus module graph is shallow; eight rounds cover it with
/// a wide margin while guaranteeing termination on adversarial checkout
/// shapes (a require cycle between checkouts cannot grow the manifest past
/// the corpus module set).
const MAX_REQUIRE_COMPLETION_ROUNDS: usize = 8;

/// The corpus root the audit resolves Go module checkouts against, when the
/// lane root is named on this host.
fn go_corpus_root() -> Option<PathBuf> {
    std::env::var_os("NUDOX_GO_CORPUS_DIR").map(PathBuf::from)
}

/// Folds the checked-in requirements of every corpus-replaced module into the
/// staged fixture manifest.
///
/// [`backend_frontend_go::legacy::stage_module`] synthesizes
/// `require`/`replace` pairs for the external imports of the staged closure,
/// but a replaced module's own requirements are invisible to that scan:
/// loading `golang.org/x/tools` through its corpus checkout dies with
/// `go: updates to go.mod needed` because the tools module itself requires
/// `golang.org/x/sync`, absent from the synthesized manifest. This pass reads
/// each replaced module's checked-in `go.mod` and appends any missing
/// `require` the corpus can also satisfy — exact version directory present —
/// together with its `replace`, repeating until the graph stops growing.
///
/// Modules the corpus cannot satisfy stay out of the manifest; the oracle
/// then refuses the row as a typed per-row terminal, exactly as it does for a
/// genuinely absent direct import. This is an invocation fix on the audit
/// side only: the staged module is otherwise untouched.
fn complete_staged_module_requirements(root: &Path, corpus_root: Option<&Path>) -> io::Result<()> {
    let Some(corpus_root) = corpus_root else {
        return Ok(());
    };
    let manifest_path = root.join("go.mod");
    let mut manifest = String::from_utf8_lossy(&fs::read(&manifest_path)?).into_owned();
    let mut replaced: Vec<(String, PathBuf)> = parse_replace_directives(&manifest)
        .into_iter()
        .filter(|(_, target)| target.is_dir())
        .collect();
    let mut required: Vec<(String, String)> = parse_require_directives(&manifest);
    for _round in 0..MAX_REQUIRE_COMPLETION_ROUNDS {
        let mut additions: Vec<(String, String, PathBuf)> = Vec::new();
        for (_, target) in &replaced {
            let Ok(bytes) = fs::read(target.join("go.mod")) else {
                continue;
            };
            for (path, version) in parse_require_directives(&String::from_utf8_lossy(&bytes)) {
                if required.iter().any(|(known, _)| known == &path)
                    || replaced.iter().any(|(known, _)| known == &path)
                    || additions.iter().any(|(known, _, _)| known == &path)
                {
                    continue;
                }
                if let Some(directory) =
                    inventory::corpus_module_checkout_exact(corpus_root, &path, &version)
                {
                    additions.push((path, version, directory));
                }
            }
        }
        if additions.is_empty() {
            return Ok(());
        }
        for (path, version, directory) in &additions {
            manifest.push_str(&format!("\nrequire {path} {version}\n"));
            manifest.push_str(&format!("replace {path} => {}\n", directory.display()));
        }
        fs::write(&manifest_path, manifest.as_bytes())?;
        for (path, version, directory) in additions {
            required.push((path.clone(), version));
            replaced.push((path, directory));
        }
    }
    Ok(())
}

/// Reads every `require <path> <version>` directive, both the single-line and
/// the parenthesized block form, dropping `//` comments.
fn parse_require_directives(manifest: &str) -> Vec<(String, String)> {
    let mut required = Vec::new();
    let mut in_block = false;
    for line in manifest.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if in_block {
            if line.starts_with(')') {
                in_block = false;
                continue;
            }
            let mut fields = line.split_whitespace();
            if let (Some(path), Some(version)) = (fields.next(), fields.next()) {
                required.push((path.to_owned(), version.to_owned()));
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("require") else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('(') {
            in_block = true;
            continue;
        }
        let mut fields = rest.split_whitespace();
        if let (Some(path), Some(version)) = (fields.next(), fields.next()) {
            required.push((path.to_owned(), version.to_owned()));
        }
    }
    required
}

/// Reads every `replace <path> => <target>` directive.
fn parse_replace_directives(manifest: &str) -> Vec<(String, PathBuf)> {
    let mut replaced = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("replace") else {
            continue;
        };
        let Some((path, target)) = rest.split_once("=>") else {
            continue;
        };
        replaced.push((path.trim().to_owned(), PathBuf::from(target.trim())));
    }
    replaced
}

/// Recursively collects every `.java` file under `root`. Hidden directories
/// (`.git`, …) are never part of a source set, and symlinked directories are
/// never followed, matching the inventory resolver's closed traversal.
fn collect_java_sources(root: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let hidden = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'));
            if !hidden {
                collect_java_sources(&path, out)?;
            }
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("java")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Builds the `-sourcepath` roots for one Maven package root: the root
/// itself, always, plus every same-group sibling artifact version directory.
/// A package root is `<lane>/<group-path>/<artifact>/<version>`, so siblings
/// are the version directories under the group path's parent. Siblings that
/// carry their own top-level `module-info.java` are excluded: javac treats
/// each such directory as a JPMS module root and fails pre-resolution module
/// lookups for unrelated modules. The target root itself is always kept even
/// when it carries one.
fn java_sourcepath_entries(package_root: &Path) -> Vec<PathBuf> {
    let mut entries = vec![package_root.to_owned()];
    let Some(group_dir) = package_root.parent().and_then(|parent| parent.parent()) else {
        return entries;
    };
    let Ok(artifacts) = fs::read_dir(group_dir) else {
        return entries;
    };
    for artifact in artifacts.flatten() {
        let artifact_path = artifact.path();
        if !artifact_path.is_dir() {
            continue;
        }
        let Ok(versions) = fs::read_dir(&artifact_path) else {
            continue;
        };
        for version in versions.flatten() {
            let candidate = version.path();
            if candidate.as_path() == package_root || !candidate.is_dir() {
                continue;
            }
            if candidate.join("module-info.java").is_file() {
                continue;
            }
            entries.push(candidate);
        }
    }
    entries.sort();
    entries.dedup();
    entries
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

#[cfg(test)]
mod go_manifest_tests {
    use super::{
        complete_staged_module_requirements, parse_replace_directives, parse_require_directives,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    /// A unique scratch directory, removed on drop. Test-support only; no
    /// fixture subtree outlives its test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "compiler-corpus-{label}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("directories");
            fs::write(&path, contents).expect("file");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn require_directives_are_read_from_lines_blocks_and_comments() {
        let manifest = "module m\n\nrequire a.example/x v1.0.0\n\nrequire (\n\tb.example/y v2.0.0 // indirect\n\tc.example/z v3.0.0\n)\n\nreplace a.example/x => /tmp/x\nreplace b.example/y => /tmp/y\n";
        let required = parse_require_directives(manifest);
        assert_eq!(
            required,
            vec![
                ("a.example/x".to_owned(), "v1.0.0".to_owned()),
                ("b.example/y".to_owned(), "v2.0.0".to_owned()),
                ("c.example/z".to_owned(), "v3.0.0".to_owned()),
            ]
        );
        let replaced = parse_replace_directives(manifest);
        assert_eq!(
            replaced,
            vec![
                ("a.example/x".to_owned(), PathBuf::from("/tmp/x")),
                ("b.example/y".to_owned(), PathBuf::from("/tmp/y")),
            ]
        );
    }

    /// A replaced module's own checked-in requirements must be folded into
    /// the staged manifest exactly when the corpus can satisfy them at the
    /// requested version, and the pass must be idempotent.
    #[test]
    fn completion_folds_corpus_satisfiable_requirements_and_stops() {
        let corpus = Scratch::new("go-complete-corpus");
        let staged = Scratch::new("go-complete-staged");

        // The replaced module itself requires two modules: one the corpus
        // pins at the exact version, one it does not provision at all.
        let tool = corpus.path().join("host.example/tool@v0.30.0");
        fs::create_dir_all(&tool).expect("tool checkout");
        fs::write(
            tool.join("go.mod"),
            "module host.example/tool\n\ngo 1.23\n\nrequire (\n\tdep.example/sync v0.1.0 // indirect\n\tabsent.example/gone v9.9.9\n)\n",
        )
        .expect("tool go.mod");
        let sync = corpus.path().join("dep.example/sync@v0.1.0");
        fs::create_dir_all(&sync).expect("sync checkout");
        fs::write(sync.join("go.mod"), "module dep.example/sync\n\ngo 1.23\n")
            .expect("sync go.mod");

        let manifest = staged.write(
            "go.mod",
            &format!(
                "module example.com/mod\n\ngo 1.23\n\nrequire host.example/tool v0.30.0\nreplace host.example/tool => {}\n",
                tool.display(),
            ),
        );

        complete_staged_module_requirements(staged.path(), Some(corpus.path()))
            .expect("completion pass");
        let completed = fs::read_to_string(&manifest).expect("manifest");
        assert!(
            completed.contains("require dep.example/sync v0.1.0"),
            "the corpus-satisfiable requirement must be folded in: {completed}"
        );
        assert!(
            completed.contains(&format!("replace dep.example/sync => {}", sync.display())),
            "the folded requirement must carry its corpus replace: {completed}"
        );
        assert!(
            !completed.contains("absent.example/gone"),
            "an unprovisioned module must stay out of the manifest: {completed}"
        );

        let requires_before = completed.matches("require dep.example/sync").count();
        complete_staged_module_requirements(staged.path(), Some(corpus.path()))
            .expect("second completion pass");
        let after = fs::read_to_string(&manifest).expect("manifest");
        assert_eq!(
            after.matches("require dep.example/sync").count(),
            requires_before,
            "the completion pass must be idempotent: {after}"
        );
    }
}
