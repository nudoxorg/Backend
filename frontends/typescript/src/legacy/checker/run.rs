//! Runs the bounded TypeScript checker subprocess and decodes its report.
use super::*;

impl Checker {
    /// Binds this bounded checker configuration to one caller-selected
    /// absolute checker executable.
    pub fn with_program(
        self,
        executable: PathBuf,
    ) -> Result<ExplicitTypeScriptChecker, TypeScriptCheckerProgramError> {
        Ok(ExplicitTypeScriptChecker {
            checker: self,
            invocation: ExplicitCheckerInvocation::ReportProgram(TypeScriptCheckerProgram::new(
                executable,
            )?),
        })
    }

    /// Binds this checker to a caller-selected absolute Node runtime that
    /// executes the vendored driver without an ambient `node` lookup.
    pub fn with_node(
        self,
        executable: PathBuf,
        module_root: PathBuf,
    ) -> Result<ExplicitTypeScriptChecker, TypeScriptCheckerProgramError> {
        Ok(ExplicitTypeScriptChecker {
            checker: self,
            invocation: ExplicitCheckerInvocation::Node {
                program: TypeScriptCheckerProgram::new(executable)?,
                module_root: TypeScriptModuleRoot::new(module_root)?,
            },
        })
    }

    /// Decodes one report transcript without starting a process.
    ///
    /// # Errors
    ///
    /// Returns [`CheckerError::Decode`] for malformed transcripts and
    /// [`CheckerError::Staleness`] for a foreign schema version.
    pub fn decode(&self, bytes: &[u8]) -> Result<Report, CheckerError> {
        let report: Report =
            serde_json::from_slice(bytes).map_err(|error| CheckerError::Decode {
                message: error.to_string(),
                transcript: transcript_prefix(bytes),
            })?;
        if report.schema_version != REQUIRED_SCHEMA_VERSION {
            return Err(CheckerError::Staleness {
                found: report.schema_version,
                expected: REQUIRED_SCHEMA_VERSION,
            });
        }
        Ok(report)
    }

    /// Runs the vendored checker over one exact source under one profile.
    ///
    /// The source is written to one bounded work directory as the checker's
    /// single input file, the child is driven with bounded output and a
    /// deadline, and the report is decoded and bound to the exact source.
    ///
    /// # Errors
    ///
    /// Returns every [`CheckerError`] cause; tool and module unavailability
    /// are distinct typed causes so callers can report the exact rejection.
    pub fn run(&self, profile: TypeScriptSource, source: &[u8]) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let run = self.run_in(&work, profile, source);
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    /// Runs the checker against a read-only staged copy of a package tree.
    ///
    /// The source is installed as the package root's `index.ts` (or `index.tsx`)
    /// in the staged tree. The caller's tree is only read, so module resolution
    /// sees its relative imports and package-local `node_modules` without
    /// granting the child write access to caller-owned files.
    pub fn run_in_package(
        &self,
        profile: TypeScriptSource,
        source: &[u8],
        package_root: &Path,
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let started = Instant::now();
        let run = (|| {
            let staged = work.join("package");
            let mut budget = PackageBudget::new(started, self.timeout);
            let mut declaration_file = false;
            stage_package(
                package_root,
                &staged,
                &mut budget,
                source,
                &mut declaration_file,
            )?;
            let file = staged.join(package_entry_file(profile, declaration_file));
            std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
                phase: "prepare",
                source: cause,
            })?;
            let report = match std::env::var("NUDOX_TYPESCRIPT_CHECKER_BIN") {
                Ok(binary) => self.run_child(&work, Path::new(&binary), &file),
                Err(std::env::VarError::NotPresent) => {
                    let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/legacy/checker")
                        .join("main.cjs");
                    let mut command = Command::new("node");
                    command.arg(driver).arg(&file);
                    self.run_child_prepared(&work, command, &file)
                }
                Err(cause) => Err(CheckerError::ToolingUnavailable {
                    tool: "NUDOX_TYPESCRIPT_CHECKER_BIN",
                    source: std::io::Error::other(cause),
                }),
            }?;
            Ok(declaration_report(report, declaration_file))
        })();
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    /// Runs a package-staged authority transaction through one already
    /// admitted explicit command. The public explicit capability calls this
    /// rather than the compatibility environment/PATH resolution above.
    pub(super) fn run_in_package_with_invocation(
        &self,
        invocation: &ExplicitCheckerInvocation,
        profile: TypeScriptSource,
        source: &[u8],
        package_root: &Path,
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let started = Instant::now();
        let run = (|| {
            let staged = work.join("package");
            let mut budget = PackageBudget::new(started, self.timeout);
            let mut declaration_file = false;
            stage_package(
                package_root,
                &staged,
                &mut budget,
                source,
                &mut declaration_file,
            )?;
            let file = staged.join(package_entry_file(profile, declaration_file));
            std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
                phase: "prepare",
                source: cause,
            })?;
            let report = self.run_explicit_file(invocation, &work, &file)?;
            Ok(declaration_report(report, declaration_file))
        })();
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    /// Runs the vendored checker through one explicit child program.
    ///
    /// The program receives the work source file as its single argument and
    /// must print the report on stdout. This is the typed resolution seam
    /// used by the vendored driver tests; production runs go through
    /// [`Checker::run`], which resolves `NUDOX_TYPESCRIPT_CHECKER_BIN` or
    /// `node` with the vendored driver.
    ///
    /// # Errors
    ///
    /// Returns the same typed causes as [`Checker::run`].
    pub fn run_with_program(
        &self,
        program: &Path,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let run = self.run_program_in(&work, program, profile, source);
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    fn run_in(
        &self,
        work: &Path,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let file = work.join(source_file(profile));
        std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        match std::env::var("NUDOX_TYPESCRIPT_CHECKER_BIN") {
            Ok(binary) => self.run_child(work, Path::new(&binary), &file),
            Err(std::env::VarError::NotPresent) => {
                let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/legacy/checker")
                    .join("main.cjs");
                let mut command = Command::new("node");
                command.arg(driver).arg(&file);
                self.run_child_prepared(work, command, &file)
            }
            Err(cause) => Err(CheckerError::ToolingUnavailable {
                tool: "NUDOX_TYPESCRIPT_CHECKER_BIN",
                source: std::io::Error::other(cause),
            }),
        }
    }

    fn run_program_in(
        &self,
        work: &Path,
        program: &Path,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let file = work.join(source_file(profile));
        std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        self.run_child(work, program, &file)
    }

    /// Runs one already admitted explicit checker command over one source file.
    pub(super) fn run_with_explicit_invocation(
        &self,
        invocation: &ExplicitCheckerInvocation,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let run = (|| {
            let file = work.join(source_file(profile));
            std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
                phase: "prepare",
                source: cause,
            })?;
            self.run_explicit_file(invocation, &work, &file)
        })();
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    fn run_explicit_file(
        &self,
        invocation: &ExplicitCheckerInvocation,
        work: &Path,
        file: &Path,
    ) -> Result<Report, CheckerError> {
        match invocation {
            ExplicitCheckerInvocation::ReportProgram(program) => {
                self.run_child(work, program.as_ref(), file)
            }
            ExplicitCheckerInvocation::Node {
                program,
                module_root,
            } => {
                let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/legacy/checker")
                    .join("main.cjs");
                let mut command = Command::new(program.as_ref());
                command.env("NODE_PATH", module_root.as_ref());
                command.arg(driver).arg(file);
                self.run_child_prepared(work, command, file)
            }
        }
    }

    fn run_child(&self, work: &Path, program: &Path, file: &Path) -> Result<Report, CheckerError> {
        let mut command = Command::new(program);
        command.arg(file);
        self.run_child_prepared(work, command, file)
    }

    fn run_child_prepared(
        &self,
        work: &Path,
        mut command: Command,
        file: &Path,
    ) -> Result<Report, CheckerError> {
        let _ = work;
        let _ = file;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| CheckerError::Spawn {
            program: format!("{:?}", command.get_program()),
            source,
        })?;
        let stdout = child.stdout.take().ok_or_else(|| CheckerError::Pipe {
            stream: "stdout",
            source: std::io::Error::other("stdout was not piped"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| CheckerError::Pipe {
            stream: "stderr",
            source: std::io::Error::other("stderr was not piped"),
        })?;
        let limit = self.output_limit;
        let (limit_sender, limit_receiver) = std::sync::mpsc::channel();
        let out_sender = limit_sender.clone();
        let out_thread =
            std::thread::spawn(move || read_bounded(stdout, limit, "stdout", out_sender));
        let err_thread =
            std::thread::spawn(move || read_bounded(stderr, limit, "stderr", limit_sender));
        let started = Instant::now();
        let mut limit_failure = None;
        let terminal = loop {
            if let Ok((stream, observed)) = limit_receiver.try_recv() {
                limit_failure = Some((stream, observed));
                terminate_child(&mut child);
                break None;
            }
            if let Some(status) = child.try_wait().map_err(|cause| CheckerError::Pipe {
                stream: "process",
                source: cause,
            })? {
                break Some(status);
            }
            if started.elapsed() >= self.timeout {
                terminate_child(&mut child);
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let out_bytes = out_thread
            .join()
            .map_err(|payload| CheckerError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardOutputReader,
                    payload.as_ref(),
                ),
            })?;
        let err_bytes = err_thread
            .join()
            .map_err(|payload| CheckerError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardErrorReader,
                    payload.as_ref(),
                ),
            })?;
        if let Some((stream, cause)) = out_bytes.error.or(err_bytes.error) {
            return Err(CheckerError::Pipe {
                stream,
                source: cause,
            });
        }
        if let Some((stream, observed)) =
            limit_failure.or(out_bytes.exceeded).or(err_bytes.exceeded)
        {
            return Err(CheckerError::OutputLimit {
                phase: "collect",
                stream,
                observed,
                limit,
            });
        }
        let Some(status) = terminal else {
            return Err(CheckerError::Timeout {
                phase: "collect",
                milliseconds: self.timeout.as_millis(),
            });
        };
        let stderr_tail = tail(&err_bytes.bytes);
        if !status.success() {
            if status.code() == Some(MODULE_MISSING_EXIT) {
                return Err(CheckerError::ModuleUnavailable {
                    stderr: stderr_tail,
                });
            }
            return Err(CheckerError::Exit {
                status: status.to_string(),
                stderr: stderr_tail,
            });
        }
        self.decode(&out_bytes.bytes)
    }
}
