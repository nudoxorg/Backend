//! Runs the Go oracle process boundary and decodes its transcripts.

use super::*;

impl GoOracle {
    /// Binds this bounded adapter to one caller-selected executable
    /// configuration.  The returned capability never consults environment
    /// variables or `PATH`.
    #[must_use]
    pub fn with_configuration(self, configuration: GoOracleConfiguration) -> ConfiguredGoOracle {
        ConfiguredGoOracle {
            oracle: self,
            configuration,
        }
    }

    /// Decodes a transcript without starting a process.
    pub fn decode(&self, bytes: &[u8]) -> Result<Output, OracleError> {
        let output: Output =
            serde_json::from_slice(bytes).map_err(|error| OracleError::Decode {
                message: error.to_string(),
                transcript: transcript_prefix(bytes),
            })?;
        if output.schema_version != Output::REQUIRED_SCHEMA_VERSION {
            return Err(OracleError::Staleness {
                found: output.schema_version,
                expected: Output::REQUIRED_SCHEMA_VERSION,
            });
        }
        Ok(output)
    }

    /// Runs the override executable, or `go run . <module>` in the vendored directory.
    pub fn run(&self, module: &std::path::Path) -> Result<Output, OracleError> {
        let oracle_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/legacy/oracle");
        let override_bin = std::env::var("NUDOX_GO_ORACLE_BIN");
        let mut command = if let Ok(binary) = override_bin {
            let mut command = std::process::Command::new(binary);
            command.arg(module);
            command
        } else {
            let compiler = match std::env::var("COMPILER_GO_COMPILER") {
                Ok(path) => path,
                Err(_) => "go".to_owned(),
            };
            let mut command = std::process::Command::new(compiler);
            command
                .args(["run", "."])
                .arg(module)
                .current_dir(oracle_dir);
            command
        };
        let stdout = self.execute(&mut command)?;
        self.decode(&stdout)
    }

    /// Runs the authority-image producer for one caller-selected source
    /// file, returning the exact binary image bytes bound to that source's
    /// SHA-256 digest. Uses the same override executable as [`GoOracle::run`]
    /// (invoked as `<bin> --authority-image <source> <module>`) or
    /// `go run . --authority-image <source> <module>` in the vendored
    /// directory.
    pub fn authority_image(
        &self,
        source: &std::path::Path,
        module: &std::path::Path,
    ) -> Result<Vec<u8>, OracleError> {
        self.authority_image_with_mode("--authority-image", source, module)
    }

    /// Runs the authority-image producer for exactly the package that owns
    /// `source`, resolving imports from the module rooted at `module`. Unlike
    /// [`GoOracle::authority_image`], sibling packages are import context only
    /// and are never serialized, so a module whose subpackages share a
    /// declaration spelling cannot inject a coordinate-free collision into the
    /// selected package's image.
    pub fn authority_image_for_package(
        &self,
        source: &std::path::Path,
        module: &std::path::Path,
    ) -> Result<Vec<u8>, OracleError> {
        self.authority_image_with_mode("--authority-image-package", source, module)
    }

    fn authority_image_with_mode(
        &self,
        mode: &str,
        source: &std::path::Path,
        module: &std::path::Path,
    ) -> Result<Vec<u8>, OracleError> {
        let oracle_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/legacy/oracle");
        let override_bin = std::env::var("NUDOX_GO_ORACLE_BIN");
        let mut command = if let Ok(binary) = override_bin {
            let mut command = std::process::Command::new(binary);
            command.arg(mode).arg(source).arg(module);
            command
        } else {
            let compiler = match std::env::var("COMPILER_GO_COMPILER") {
                Ok(path) => path,
                Err(_) => "go".to_owned(),
            };
            let mut command = std::process::Command::new(compiler);
            command
                .args(["run", ".", mode])
                .arg(source)
                .arg(module)
                .current_dir(oracle_dir);
            command
        };
        self.execute(&mut command)
    }

    fn configured_command(
        configuration: &GoOracleConfiguration,
        authority_source: Option<(&str, &Path)>,
        module: &Path,
    ) -> std::process::Command {
        match configuration {
            GoOracleConfiguration::OracleBinary(executable) => {
                let mut command = std::process::Command::new(executable.as_ref());
                if let Some((mode, source)) = authority_source {
                    command.arg(mode).arg(source);
                }
                command.arg(module);
                command
            }
            GoOracleConfiguration::GoToolchain(executable) => {
                let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/legacy/oracle");
                let mut command = std::process::Command::new(executable.as_ref());
                command.args(["run", "."]);
                if let Some((mode, source)) = authority_source {
                    command.arg(mode).arg(source);
                }
                command.arg(module).current_dir(oracle_dir);
                command
            }
        }
    }

    /// Runs one explicit configuration and decodes its transcript.
    pub(super) fn run_configured(
        &self,
        configuration: &GoOracleConfiguration,
        module: &Path,
    ) -> Result<Output, OracleError> {
        let mut command = Self::configured_command(configuration, None, module);
        let stdout = self.execute_configured(&mut command)?;
        self.decode(&stdout)
    }

    /// Produces one authority image through an explicit configuration.
    pub(super) fn authority_image_configured(
        &self,
        configuration: &GoOracleConfiguration,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        let mut command =
            Self::configured_command(configuration, Some(("--authority-image", source)), module);
        self.execute_configured(&mut command)
    }

    /// Produces one package-scoped authority image through an explicit configuration.
    pub(super) fn authority_image_for_package_configured(
        &self,
        configuration: &GoOracleConfiguration,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        let mut command = Self::configured_command(
            configuration,
            Some(("--authority-image-package", source)),
            module,
        );
        self.execute_configured(&mut command)
    }

    /// Spawns one bounded oracle child and collects its standard output.
    /// The child runs in its own process group; oversized output, deadlines,
    /// and pipe faults all reap the child and fold into typed rejections.
    fn execute(&self, command: &mut std::process::Command) -> Result<Vec<u8>, OracleError> {
        self.execute_with_origin(command, false)
    }

    fn execute_configured(
        &self,
        command: &mut std::process::Command,
    ) -> Result<Vec<u8>, OracleError> {
        self.execute_with_origin(command, true)
    }

    fn execute_with_origin(
        &self,
        command: &mut std::process::Command,
        configured: bool,
    ) -> Result<Vec<u8>, OracleError> {
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| {
            if configured {
                OracleError::Spawn {
                    program: command.get_program().to_string_lossy().into_owned(),
                    source,
                }
            } else {
                OracleError::ToolingUnavailable {
                    tool: "NUDOX_GO_ORACLE_BIN or go run",
                    source,
                }
            }
        })?;
        let stdout = child.stdout.take().ok_or_else(|| OracleError::Pipe {
            stream: "stdout",
            source: std::io::Error::other("stdout was not piped"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| OracleError::Pipe {
            stream: "stderr",
            source: std::io::Error::other("stderr was not piped"),
        })?;
        let limit = self.output_limit;
        let (limit_tx, limit_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let out_tx = limit_tx.clone();
        let out_done = done_tx.clone();
        let out_thread = std::thread::spawn(move || {
            let result = read_bounded(stdout, limit, "stdout", out_tx);
            let _ = out_done.send("stdout");
            result
        });
        let err_thread = std::thread::spawn(move || {
            let result = read_bounded(stderr, limit, "stderr", limit_tx);
            let _ = done_tx.send("stderr");
            result
        });
        let started = std::time::Instant::now();
        let mut terminal = None;
        let mut stdout_done = false;
        let mut stderr_done = false;
        loop {
            if let Ok((stream, observed)) = limit_rx.try_recv() {
                terminate_child(&mut child);
                // A descendant may maliciously retain one of the inherited
                // pipes even after the direct child is reaped. Detach the
                // readers on this closed terminal instead of turning a
                // bounded output rejection into an unbounded join.
                drop(out_thread);
                drop(err_thread);
                return Err(OracleError::OutputLimit {
                    phase: "collect",
                    stream,
                    observed,
                    limit,
                });
            }
            while let Ok(stream) = done_rx.try_recv() {
                match stream {
                    "stdout" => stdout_done = true,
                    "stderr" => stderr_done = true,
                    _ => unreachable!("reader completion lane is closed"),
                }
            }
            if terminal.is_none() {
                terminal = child.try_wait().map_err(|source| OracleError::Pipe {
                    stream: "process",
                    source,
                })?;
            }
            if terminal.is_some() && stdout_done && stderr_done {
                break;
            }
            if started.elapsed() >= self.timeout {
                terminate_child(&mut child);
                drop(out_thread);
                drop(err_thread);
                return Err(OracleError::Timeout {
                    phase: "collect",
                    milliseconds: self.timeout.as_millis() as u64,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let stdout = out_thread
            .join()
            .map_err(|payload| OracleError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardOutputReader,
                    payload.as_ref(),
                ),
            })?;
        let stderr = err_thread
            .join()
            .map_err(|payload| OracleError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardErrorReader,
                    payload.as_ref(),
                ),
            })?;
        if let Some((stream, source)) = stdout.error.or(stderr.error) {
            return Err(OracleError::Pipe { stream, source });
        }
        if let Some((stream, observed)) = stdout.exceeded.or(stderr.exceeded) {
            return Err(OracleError::OutputLimit {
                phase: "collect",
                stream,
                observed,
                limit,
            });
        }
        let status = terminal.ok_or_else(|| OracleError::Pipe {
            stream: "process",
            source: std::io::Error::other("missing child status"),
        })?;
        if !status.success() {
            return Err(OracleError::Exit {
                status: status.to_string(),
                stderr: tail(&stderr.bytes),
            });
        }
        Ok(stdout.bytes)
    }
}
