//! Runs one bounded Roslyn helper and returns a source-bound authority image.

use super::*;

impl CSharpOracle {
    /// Creates a producer bound to one published helper assembly.
    ///
    /// The path is intentionally not searched or built by this constructor;
    /// an unavailable helper is reported by the process boundary with its
    /// exact configured path.
    #[must_use]
    pub fn new(oracle: PathBuf) -> Self {
        Self {
            oracle,
            output_limit: DEFAULT_OUTPUT_LIMIT,
            image_limit: DEFAULT_IMAGE_LIMIT,
        }
    }

    /// Returns the exact helper assembly path this producer will execute.
    #[must_use]
    pub fn oracle_path(&self) -> &Path {
        &self.oracle
    }

    /// Changes the per-stream child output bound.
    #[must_use]
    pub const fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    /// Changes the retained authority-image bound.
    #[must_use]
    pub const fn with_image_limit(mut self, limit: usize) -> Self {
        self.image_limit = limit;
        self
    }

    /// Runs the real Roslyn helper and returns a validated source-bound image.
    ///
    /// The helper receives the package root and exact binding path, so package
    /// context is available to Roslyn while the returned image remains bound
    /// to only the caller's source bytes.  The image is returned as owned
    /// bytes because the next compiler boundary may retain it while borrowing
    /// a validated [`CSharpImage`].
    ///
    /// # Errors
    ///
    /// Returns typed path, source-binding, cancellation, deadline, process,
    /// output-limit, and image-validation causes.  A child failure retains a
    /// bounded stderr tail rather than collapsing the helper's diagnosis into
    /// a boolean unavailable result.
    pub fn authority_image<'request, 'config, 'cancel>(
        &self,
        request: CSharpAuthorityRequest<'request, 'config, 'cancel>,
    ) -> Result<Vec<u8>, CSharpAuthorityError> {
        checkpoint(request.control, CSharpAuthorityPhase::Admission)?;
        if request.native_tool != NativeTool::CSharpCompiler {
            return Err(CSharpAuthorityError::WrongToolchain {
                observed: request.native_tool,
            });
        }
        validate_absolute_paths(&self.oracle, request)?;
        let maximum_source_bytes = request.configuration.maximum_source_bytes;
        if request.source.len() > maximum_source_bytes {
            return Err(CSharpAuthorityError::SourceLimit {
                observed: u64::try_from(request.source.len()).unwrap_or(u64::MAX),
                maximum: maximum_source_bytes,
            });
        }

        let package_root = fs::canonicalize(request.package_root).map_err(|source| {
            CSharpAuthorityError::Path {
                phase: CSharpAuthorityPhase::Admission,
                path: request.package_root.to_path_buf(),
                source,
            }
        })?;
        let source_path =
            fs::canonicalize(request.source_path).map_err(|source| CSharpAuthorityError::Path {
                phase: CSharpAuthorityPhase::Source,
                path: request.source_path.to_path_buf(),
                source,
            })?;
        if source_path.strip_prefix(&package_root).is_err() {
            return Err(CSharpAuthorityError::SourceOutsidePackage {
                package_root: package_root.into_boxed_path(),
                source_path: source_path.into_boxed_path(),
            });
        }

        checkpoint(request.control, CSharpAuthorityPhase::Source)?;
        let source_length = fs::metadata(&source_path)
            .map_err(|source| CSharpAuthorityError::Path {
                phase: CSharpAuthorityPhase::Source,
                path: source_path.clone(),
                source,
            })?
            .len();
        if source_length > u64::try_from(maximum_source_bytes).unwrap_or(u64::MAX) {
            return Err(CSharpAuthorityError::SourceLimit {
                observed: source_length,
                maximum: maximum_source_bytes,
            });
        }
        let on_disk = fs::read(&source_path).map_err(|source| CSharpAuthorityError::Path {
            phase: CSharpAuthorityPhase::Source,
            path: source_path.clone(),
            source,
        })?;
        if on_disk != request.source {
            return Err(CSharpAuthorityError::SourceBinding {
                source_path: source_path.into_boxed_path(),
                expected: Sha256::digest(request.source).into(),
                observed: Sha256::digest(&on_disk).into(),
            });
        }

        checkpoint(request.control, CSharpAuthorityPhase::Spawn)?;
        let mut command = Command::new(request.toolchain);
        command
            .arg("exec")
            .arg(&self.oracle)
            .args(["--mode", "source", "--root"])
            .arg(&package_root)
            .args(["--authority-image", "--source-binding"])
            .arg(&source_path)
            .args(["--lang-version", profile_tag(request.profile)])
            .current_dir(&package_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let configuration = request.configuration;
        if let Some(name) = configuration.assembly_name {
            command.args(["--assembly-name", name]);
        }
        if let Some(directory) = configuration.reference_directory {
            command.arg("--ref-dir").arg(directory);
        }
        for symbol in configuration.define_symbols {
            command.args(["--define", symbol]);
        }
        for namespace in configuration.extra_usings {
            command.args(["--using", namespace]);
        }
        if !configuration.implicit_usings {
            command.arg("--no-implicit-usings");
        }
        if !configuration.include_non_public {
            command.arg("--public-only");
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .map_err(|source| CSharpAuthorityError::Spawn {
                toolchain: request.toolchain.to_path_buf().into_boxed_path(),
                oracle: self.oracle.clone().into_boxed_path(),
                source,
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            terminate_child(&mut child);
            CSharpAuthorityError::Pipe {
                stream: "stdout",
                source: std::io::Error::other("child stdout was not piped"),
            }
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            terminate_child(&mut child);
            CSharpAuthorityError::Pipe {
                stream: "stderr",
                source: std::io::Error::other("child stderr was not piped"),
            }
        })?;
        let (limit_sender, limit_receiver) = mpsc::channel();
        let stdout_sender = limit_sender.clone();
        let output_limit = self.output_limit;
        let stdout_thread =
            thread::spawn(move || read_bounded(stdout, output_limit, "stdout", stdout_sender));
        let stderr_thread =
            thread::spawn(move || read_bounded(stderr, output_limit, "stderr", limit_sender));

        let terminal = wait_for_child(
            &mut child,
            request.control,
            &limit_receiver,
            self.output_limit,
        );
        let stdout = join_stream(stdout_thread, NativeWorker::StandardOutputReader)?;
        let stderr = join_stream(stderr_thread, NativeWorker::StandardErrorReader)?;

        let terminal = terminal?;
        if let Some((stream, observed)) = stdout.exceeded.or(stderr.exceeded) {
            return Err(CSharpAuthorityError::OutputLimit {
                stream,
                observed,
                limit: self.output_limit,
            });
        }
        if let Some(source) = stdout.error {
            return Err(CSharpAuthorityError::Pipe {
                stream: "stdout",
                source,
            });
        }
        if let Some(source) = stderr.error {
            return Err(CSharpAuthorityError::Pipe {
                stream: "stderr",
                source,
            });
        }
        if !terminal.success() {
            return Err(CSharpAuthorityError::Exit {
                status: terminal.to_string(),
                stderr: stderr_tail(&stderr.bytes),
            });
        }

        checkpoint(request.control, CSharpAuthorityPhase::Decode)?;
        if stdout.bytes.len() > self.image_limit {
            return Err(CSharpAuthorityError::ImageLimit {
                observed: stdout.bytes.len(),
                limit: self.image_limit,
            });
        }
        let image = CSharpImage::open(&stdout.bytes).map_err(CSharpAuthorityError::Image)?;
        let expected: [u8; 32] = Sha256::digest(request.source).into();
        let observed = image.source_digest();
        if observed != expected {
            return Err(CSharpAuthorityError::ImageBinding { expected, observed });
        }
        checkpoint(request.control, CSharpAuthorityPhase::Return)?;
        Ok(stdout.bytes)
    }

    /// Runs [`Self::authority_image`] and retains its bytes in an owned image.
    ///
    /// The returned owner is convenient for a package authority transaction
    /// whose driver input borrows the image for the duration of lowering.
    pub fn produce<'request, 'config, 'cancel>(
        &self,
        request: CSharpAuthorityRequest<'request, 'config, 'cancel>,
    ) -> Result<CSharpAuthorityImage, CSharpAuthorityError> {
        self.authority_image(request)
            .map(CSharpAuthorityImage::from_bytes)
    }
}
