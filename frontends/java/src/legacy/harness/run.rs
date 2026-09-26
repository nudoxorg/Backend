//! Builds one javac doclet image from caller-owned source bytes.

use super::*;

impl Harness {
    /// Creates the private scratch directory used by this harness.
    pub fn new() -> Result<Self, HarnessError> {
        for attempt in 0..64 {
            let serial = DIRECTORY_SERIAL.fetch_add(1, Ordering::Relaxed);
            let root = env::temp_dir().join(format!(
                "nudox-java-harness-{}-{serial}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    return Ok(Self {
                        doclet_classes: root.join("classes"),
                        root,
                        cleanup_error: None,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(HarnessError::Io { path: root, source }),
            }
        }
        Err(HarnessError::UniqueDirectory)
    }

    /// Returns the compiled classes directory, useful for lifecycle observability.
    pub fn classes_dir(&self) -> &Path {
        &self.doclet_classes
    }

    /// Returns the preparation marker path.
    pub fn marker_path(&self) -> PathBuf {
        self.doclet_classes.join(MARKER)
    }

    /// Returns a cleanup failure observed by `Drop`, if any.
    pub fn take_cleanup_error(&mut self) -> Option<io::Error> {
        self.cleanup_error.take()
    }

    /// Compiles the embedded doclet once per source digest.
    pub fn prepare(&mut self, toolchain: &JdkToolchain<'_>) -> Result<(), HarnessError> {
        fs::create_dir(&self.doclet_classes)
            .or_else(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|source| HarnessError::Io {
                path: self.doclet_classes.clone(),
                source,
            })?;
        let digest = source_digest();
        if fs::read(self.marker_path()).ok().as_deref() == Some(digest.as_slice()) {
            return Ok(());
        }
        let source_dir = self.root.join("doclet-source");
        fs::create_dir(&source_dir).map_err(|source| HarnessError::Io {
            path: source_dir.clone(),
            source,
        })?;
        let authority = source_dir.join("AuthorityImage.java");
        let extractor = source_dir.join("CompilerExtractor.java");
        fs::write(&authority, AUTHORITY_SOURCE).map_err(|source| HarnessError::Io {
            path: authority.clone(),
            source,
        })?;
        fs::write(&extractor, EXTRACTOR_SOURCE).map_err(|source| HarnessError::Io {
            path: extractor.clone(),
            source,
        })?;
        let stderr = self.root.join("prepare.stderr");
        let mut command = Command::new(toolchain.executable("javac"));
        command
            .args([
                "--release",
                "21",
                "--add-modules",
                "jdk.compiler,jdk.javadoc",
                "-d",
            ])
            .arg(&self.doclet_classes)
            .args([authority, extractor]);
        run_command(command, "javac", &stderr)?;
        fs::write(self.marker_path(), digest).map_err(|source| HarnessError::Io {
            path: self.marker_path(),
            source,
        })
    }

    pub fn image_with_releases(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        sourcepath: &[&Path],
        releases: &[crate::legacy::JavaRelease],
        output: &mut Vec<u8>,
    ) -> Result<HarnessOutcome, HarnessError> {
        output.clear();
        let mut attempts = Vec::new();
        for &release in std::iter::once(&request.release).chain(releases.iter()) {
            if attempts
                .iter()
                .any(|attempt: &ReleaseFailure| attempt.release == release)
            {
                continue;
            }
            match self.image_with_sourcepath(
                toolchain,
                HarnessRequest { release, ..request },
                sourcepath,
                output,
            ) {
                Ok(()) => {
                    return Ok(HarnessOutcome::Available {
                        release,
                        prior_failures: attempts,
                    });
                }
                Err(error) => {
                    let Some(cause) = error.unavailable_cause() else {
                        return Err(error);
                    };
                    attempts.push(ReleaseFailure {
                        release,
                        cause,
                        error,
                    });
                    if cause != UnavailableCause::Compilation {
                        break;
                    }
                }
            }
        }
        Ok(HarnessOutcome::Unavailable { attempts })
    }

    /// Writes borrowed sources, runs the extractor, and replaces `output` with its image.
    pub fn image(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        self.image_with_sourcepath(toolchain, request, &[], output)
    }

    /// Writes borrowed sources, runs the extractor with a best-effort
    /// `-sourcepath`, and replaces `output` with its image.
    ///
    /// See [`Self::image_with_corpus`] for the extraction contract; the
    /// fleet corpus here is read from `$NUDOX_JAVA_CORPUS_DIR`.
    pub fn image_with_sourcepath(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        sourcepath: &[&Path],
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        self.image_with_corpus(toolchain, request, sourcepath, None, output)
    }

    /// Writes borrowed sources, runs the extractor, and replaces `output`
    /// with its image, discovering fleet siblings from `corpus` when given
    /// and from `$NUDOX_JAVA_CORPUS_DIR` otherwise.
    ///
    /// The entries are host source roots the extractor passes to javac so
    /// same-group sibling artifacts resolve without an explicit classpath.
    /// Missing or non-directory entries are ignored. The per-run staging
    /// directory always heads the path so module-mode compilation (a
    /// `module-info.java` among the sources) resolves the staged copies.
    ///
    /// After the caller's roots, every version root discovered under the
    /// corpus root is appended (see [`super::super::sourcepath`]), so cross-group
    /// fleet siblings — annotation jars like `org.jspecify` or
    /// `org.jetbrains.annotations`, and sibling APIs like `org.slf4j` —
    /// resolve the same way the Go oracle resolves corpus checkouts. Caller
    /// roots keep precedence and the corpus never shadows them. Root
    /// discovery skips `module-info.java`-carrying roots because javac turns
    /// any such source-path entry into a required-module lookup that fails
    /// every unnamed-module compilation. When the corpus is absent the
    /// behavior is exactly the caller-provided roots.
    ///
    /// JPMS module walls are crossed honestly, without rewriting sources:
    /// when the compile closure needs modules — a top-level
    /// `module-info.java` is staged among the sources or under a caller
    /// root — the extraction runs in module mode. The unnamed `-sourcepath`
    /// is replaced by repeated `--module-source-path <module>=<path>`
    /// entries (javac rejects the two options together): the target module
    /// maps to the staged run directory, and every module-walled corpus
    /// root maps to its canonical directory, deduplicated by module name
    /// with the target's own name never shadowed. javac then compiles the
    /// required corpus modules from source. A `requires` whose sources are
    /// absent stays a typed compilation refusal naming that module; when an
    /// unnamed attempt fails with `module not found: <name>` and the corpus
    /// can provision modules, the extraction retries once in module mode
    /// and preserves the retry's precise diagnostic. Release 8 predates the
    /// module system and never enters module mode. Non-modular closures
    /// keep the byte-identical default invocation.
    pub fn image_with_corpus(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        sourcepath: &[&Path],
        corpus: Option<&Path>,
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        output.clear();
        let first = request.sources.first().ok_or(HarnessError::NoSources)?;
        let discovered = super::super::sourcepath::merged_source_roots(sourcepath, corpus)
            .map_err(|source| HarnessError::Io {
                path: self.root.clone(),
                source,
            })?;
        let run = self.root.join(format!(
            "run-{}",
            DIRECTORY_SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&run).map_err(|source| HarnessError::Io {
            path: run.clone(),
            source,
        })?;
        for source in request.sources {
            validate_name(source.name)?;
            let path = run.join(source.name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| HarnessError::Io {
                    path: parent.to_owned(),
                    source,
                })?;
            }
            let source_path = path;
            fs::write(&source_path, source.bytes).map_err(|error| HarnessError::Io {
                path: source_path,
                source: error,
            })?;
        }
        let image = run.join("authority.image");
        let mut cp = vec![self.doclet_classes.clone()];
        cp.extend(request.classpath.iter().copied().map(Path::to_owned));
        let classpath = env::join_paths(cp).map_err(|source| HarnessError::Classpath { source })?;
        let plan = module_plan(request.release, request.sources, &discovered, corpus, &run)
            .map_err(|source| HarnessError::Io {
                path: run.clone(),
                source,
            })?;
        let attempt = |entries: Option<&[(String, PathBuf)]>| -> Result<Vec<u8>, HarnessError> {
            let mut command = Command::new(toolchain.executable("java"));
            command
                .args(["--add-modules", "jdk.compiler,jdk.javadoc", "-cp"])
                .arg(&classpath)
                .arg("nudox.oracle.CompilerExtractor")
                .args(["--release", release_text(request.release), "--outfile"])
                .arg(&image)
                .arg("--source-binding")
                .arg(run.join(first.name));
            // Module mode replaces the unnamed source path entirely: javac
            // rejects `--source-path` together with `--module-source-path`,
            // and a named compilation resolves everything required through
            // the module source path entries below. The target module's
            // entry points at the run directory, which holds the explicit
            // sources in their package layout.
            match entries {
                Some(entries) => {
                    for (name, path) in entries {
                        command
                            .arg("--module-source-path")
                            .arg(format!("{name}={}", path.display()));
                    }
                }
                None => {
                    // The run directory always heads the source path: it
                    // holds the explicit sources in their package layout,
                    // which module-mode javac requires (a `module-info.java`
                    // among the sources makes every explicit file resolve
                    // through the source path, and the host roots alone do
                    // not contain the staged copies). For unnamed-module
                    // packages the extra entry is benign: it carries the
                    // same files.
                    let mut roots: Vec<&Path> =
                        Vec::with_capacity(sourcepath.len().saturating_add(1));
                    roots.push(&run);
                    roots.extend(discovered.iter().map(PathBuf::as_path));
                    let joined = env::join_paths(&roots)
                        .map_err(|source| HarnessError::Classpath { source })?;
                    command.arg("--sourcepath").arg(joined);
                }
            }
            for source in request.sources {
                command.arg(run.join(source.name));
            }
            let stderr = run.join("extract.stderr");
            if let Err(mut error) = run_command(command, "java", &stderr) {
                // The child's stderr stream is not dependable on every host,
                // so the doclet also persists the exact diagnostic text beside
                // the image. When present it is the preserved diagnostic; the
                // captured stderr stays as the fallback.
                let sidecar = run.join("authority.diagnostics");
                if let HarnessError::Command {
                    command,
                    status,
                    stderr,
                } = &mut error
                {
                    if let Ok(bytes) = fs::read(&sidecar) {
                        if !bytes.is_empty() {
                            let capped = &bytes[..bytes.len().min(STDERR_LIMIT as usize)];
                            *stderr = String::from_utf8_lossy(capped).into_owned();
                        }
                    }
                    if let Some(packages) = unresolved_dependency_packages(stderr) {
                        return Err(HarnessError::UnresolvedDependencies {
                            packages,
                            exit_code: status
                                .code()
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| status.to_string()),
                            command: command.clone(),
                            stderr: stderr.clone(),
                        });
                    }
                }
                return Err(error);
            }
            let mut bytes = Vec::new();
            File::open(&image)
                .map_err(|source| HarnessError::Io {
                    path: image.clone(),
                    source,
                })?
                .read_to_end(&mut bytes)
                .map_err(|source| HarnessError::Io {
                    path: image.clone(),
                    source,
                })?;
            Ok(bytes)
        };
        let mut extraction = attempt(plan.as_deref());
        if extraction
            .as_ref()
            .is_err_and(HarnessError::names_missing_module)
            && plan.is_none()
        {
            // Detection by failure: an unnamed closure whose javac diagnostic
            // names an unresolvable module retries once with the fleet's
            // module roots. The retry's diagnostic replaces the first
            // attempt's, so the preserved refusal names the true missing
            // artifact; when the corpus cannot provision modules the first
            // attempt's error is kept exactly.
            let retry = super::super::sourcepath::corpus_module_roots(corpus).map(|modules| {
                let mut entries: Vec<(String, PathBuf)> = Vec::new();
                for module in modules {
                    if entries.iter().any(|(name, _)| *name == module.name) {
                        continue;
                    }
                    entries.push((module.name, module.path));
                }
                entries
            });
            if let Ok(entries) = retry {
                if !entries.is_empty() {
                    extraction = attempt(Some(&entries));
                }
            }
        }
        match extraction {
            Ok(bytes) => {
                output.clear();
                *output = bytes;
                fs::remove_dir_all(&run).map_err(|source| HarnessError::Io { path: run, source })
            }
            Err(error) => Err(error),
        }
    }
}
