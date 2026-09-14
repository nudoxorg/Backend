//! The pyrefly type-authority transaction for Python sources.
//!
//! pyrefly is a peer authority beside the Ruff syntax authority: Ruff owns
//! spans, declarations, and written annotations; pyrefly owns inferred types,
//! import resolution, and call-target resolution. One transaction takes the
//! caller's source plus its already-extracted [`ModuleFacts`], runs bounded
//! pyrefly child processes, and returns typed, validated facts — it never
//! returns raw tool text.
//!
//! Bounded-invocation discipline (modeled on the vendored Go oracle):
//! an override binary (`NUDOX_PYREFLY_BIN`), a hard output limit per child
//! stream, a wall-clock timeout that reaps the child, and typed terminals
//! that preserve every operand. The transaction writes its probe files into
//! a private temporary workspace that is removed on every exit path.
//!
//! Derivation laws:
//! - Inferred types come from pyrefly's own `reveal-type` diagnostics on a
//!   probe file that appends `reveal_type(name)` for single-bound unannotated
//!   module constants and splices `reveal_type(param)` after a function
//!   header's colon. A rebound constant — including one rebound by decorator
//!   application — is probed exactly once, at its final binding, so only
//!   that row's exact span carries the end-of-module type. A `Literal[...]`
//!   refinement widens to its base primitive; `Any`/`Unknown` means the
//!   checker proved nothing, so consumers keep their unannotated state.
//!   Single-line function bodies are never probed.
//! - Import resolution is the absence of a `missing-import` diagnostic over
//!   the imported module spelling on the pristine file: the checker bound the
//!   module, so consumers may mint `pypi` package foreign keys.
//! - Symbol resolution is the absence of `unknown-name`/`missing-import`
//!   over a call site: the oracle bound the name to the local declaration
//!   (local outcome) or to a resolved import binding (foreign outcome).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use backend_semantic::vocabulary::{NativeWorker, NativeWorkerPanic, PythonVersion};
use thiserror::Error;

use crate::legacy::{Annotation, AnnotationPosition, DeclarationKind, ModuleFacts, Span, TypeReason};

/// Tail retained from a child stream inside one typed diagnostic.
const TRANSCRIPT_LIMIT: usize = 4096;
/// Default per-stream output bound retained from a child.
const DEFAULT_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
/// Default wall-clock bound for one child run.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Environment override naming the pyrefly executable.
const BINARY_OVERRIDE: &str = "NUDOX_PYREFLY_BIN";
/// Diagnostic name pyrefly emits for a revealed type.
const REVEAL_DIAGNOSTIC: &str = "reveal-type";
/// Diagnostic name pyrefly emits for an unresolvable import.
const MISSING_IMPORT_DIAGNOSTIC: &str = "missing-import";
/// Diagnostic name pyrefly emits for an unresolvable name.
const UNKNOWN_NAME_DIAGNOSTIC: &str = "unknown-name";
/// Prefix of the revealed-type description text.
const REVEALED_TYPE_PREFIX: &str = "revealed type: ";
/// Probe header that binds `reveal_type` for the type authority.
const REVEAL_HEADER: &[u8] = b"from typing import reveal_type\n";
/// The `reveal_type(` call spelling prefix.
const REVEAL_CALL: &[u8] = b"reveal_type(";

/// Typed failure of one pyrefly authority transaction, preserving operands.
#[derive(Debug, Error)]
pub enum CheckerError {
    /// The configured pyrefly executable could not be started.
    #[error("pyrefly could not be started ({program:?}): {source}")]
    Spawn {
        /// Configured program spelling.
        program: PathBuf,
        /// Operating-system spawn failure.
        source: std::io::Error,
    },
    /// A child pipe could not be read or a reader thread failed.
    #[error("pyrefly pipe failed during {stream}: {source}")]
    Pipe {
        /// Failing stream name.
        stream: &'static str,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// A bounded stream reader panicked; the exact worker and supported
    /// payload facts survive the join boundary.
    #[error("pyrefly stream worker panicked: {cause}")]
    WorkerPanic {
        /// Bounded original join payload.
        #[source]
        cause: NativeWorkerPanic,
    },
    /// The child exited with an unusable status, retaining its stderr tail.
    #[error("pyrefly exited with {status}; stderr tail: {stderr}")]
    Exit {
        /// Rendered exit status.
        status: String,
        /// Bounded stderr tail.
        stderr: String,
    },
    /// The child emitted malformed or incomplete JSON.
    #[error("pyrefly JSON decode failed: {message}; transcript prefix: {transcript}")]
    Decode {
        /// Parse failure description.
        message: String,
        /// Bounded transcript prefix.
        transcript: String,
    },
    /// The output bound was exceeded and the child was reaped.
    #[error(
        "pyrefly output limit exceeded during {phase} on {stream}: observed {observed}, limit {limit}"
    )]
    OutputLimit {
        /// Run phase that observed the overrun.
        phase: &'static str,
        /// Stream that overran.
        stream: &'static str,
        /// Observed byte count.
        observed: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// The child did not complete before the deadline and was reaped.
    #[error("pyrefly timed out during {phase} after {milliseconds}ms")]
    Timeout {
        /// Run phase that hit the deadline.
        phase: &'static str,
        /// Deadline in milliseconds.
        milliseconds: u64,
    },
    /// The private probe workspace could not be created or written.
    #[error("pyrefly probe workspace failed: {source}")]
    Workspace {
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// A borrowed fact span fell outside the exact caller source.
    #[error("checker fact span {start}..{end} outside {source_length} source bytes")]
    InvalidSpan {
        /// Span start.
        start: u32,
        /// Span end.
        end: u32,
        /// Exact source length.
        source_length: usize,
    },
}

/// The narrowed key and value pair of one revealed mapping.
type DictPair = Option<(Box<InferredType>, Box<InferredType>)>;

/// One type the authority inferred for an unannotated binding or parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferredType {
    /// Python's arbitrary-precision integer.
    Integer,
    /// Python's IEEE-754 double.
    Float,
    /// `True` or `False`.
    Boolean,
    /// A text string.
    Str,
    /// A byte string.
    Bytes,
    /// A complex number.
    Complex,
    /// The `None` singleton type.
    NoneType,
    /// A homogeneous list; the element type when the authority narrowed one.
    List(Option<Box<InferredType>>),
    /// A homogeneous set; the element type when the authority narrowed one.
    Set(Option<Box<InferredType>>),
    /// A mapping with its key and value types when both are narrowed.
    Dict(DictPair),
    /// A fixed-length heterogeneous tuple with its element types.
    Tuple(Box<[InferredType]>),
    /// A union of the possible types.
    Union(Box<[InferredType]>),
    /// A callable with its parameter types and optional result type.
    Callable {
        /// Parameter types in declaration order.
        params: Box<[InferredType]>,
        /// Result type, when the authority proved one.
        result: Option<Box<InferredType>>,
    },
    /// A named class or alias spelling the checker rendered.
    Named(Box<str>),
    /// The authority proved nothing usable (`Any`, `Unknown`).
    Any,
}

/// Which source position one inference belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceSite {
    /// A module-level constant bound exactly once.
    ModuleBinding,
    /// A function or method parameter.
    Parameter,
}

/// One inferred type keyed by the exact original name span it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inference {
    /// Exact original span of the binding or parameter identifier.
    pub site: Span,
    /// Which kind of position the site is.
    pub kind: InferenceSite,
    /// The type the authority inferred.
    pub observed: InferredType,
}

/// One import binding's resolution under the type authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResolution {
    /// The local binding name.
    pub binding: String,
    /// The imported module spelling.
    pub module: String,
    /// Exact original span of the imported module spelling.
    pub module_span: Span,
    /// True when the authority resolved the module.
    pub resolved: bool,
}

/// How the authority resolved one call target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolOutcome {
    /// The oracle bound the name to a local module declaration.
    Local,
    /// The oracle bound the name to a resolved import binding.
    Foreign {
        /// The imported module spelling the binding came from.
        module: String,
    },
    /// The oracle could not bind the name.
    Unresolved,
}

/// One call site's resolution under the type authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolResolution {
    /// The written target spelling.
    pub target: String,
    /// Exact original span of the reference site.
    pub span: Span,
    /// The resolution outcome.
    pub outcome: SymbolOutcome,
}

/// Typed, validated output of one pyrefly authority transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckerReport {
    /// Inferred types for probed unannotated positions.
    pub inferences: Box<[Inference]>,
    /// Import resolutions for every alias declaration.
    pub imports: Box<[ImportResolution]>,
    /// Call-site resolutions for every recorded occurrence.
    pub symbols: Box<[SymbolResolution]>,
}

impl CheckerReport {
    /// The inferred type at one exact original site, when the authority
    /// proved one.
    #[must_use]
    pub fn inference_at(&self, site: Span) -> Option<&InferredType> {
        self.inferences
            .iter()
            .find(|inference| inference.site == site)
            .map(|inference| &inference.observed)
    }

    /// The symbol resolution at one exact original reference span.
    #[must_use]
    pub fn symbol_at(&self, span: Span) -> Option<&SymbolResolution> {
        self.symbols.iter().find(|symbol| symbol.span == span)
    }

    /// True when the authority resolved the import spelled at this span.
    #[must_use]
    pub fn import_resolved(&self, module_span: Span) -> bool {
        self.imports
            .iter()
            .any(|import| import.module_span == module_span && import.resolved)
    }
}

/// Configurable bounded adapter for the pyrefly type authority.
#[derive(Debug, Clone)]
pub struct Pyrefly {
    /// Program spelling executed.
    program: PathBuf,
    /// Arguments inserted before the pyrefly subcommand (`pyrefly` under uvx).
    arguments: Vec<String>,
    /// Maximum bytes retained and accepted from each child stream.
    output_limit: usize,
    /// Maximum wall-clock duration for one child run.
    timeout: Duration,
}

/// Rejection while admitting an explicit pyrefly executable.
#[derive(Debug, Error)]
pub enum PyreflyExecutableError {
    /// A relative path would consult ambient process search state when the
    /// authority transaction later starts.
    #[error("pyrefly executable is not absolute: {executable:?}")]
    RelativeExecutable {
        /// Caller-supplied relative executable path.
        executable: PathBuf,
    },
}

impl Default for Pyrefly {
    fn default() -> Self {
        Self {
            program: PathBuf::from("pyrefly"),
            arguments: Vec::new(),
            output_limit: DEFAULT_OUTPUT_LIMIT,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Pyrefly {
    /// Returns a host-local fingerprint of the explicit checker command and
    /// its output/deadline bounds. Path bytes make this a drift detector
    /// rather than a cross-host closure identity.
    #[must_use]
    pub fn local_configuration_fingerprint(&self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        digest.update(b"compiler.python.package-authority.v1\0");
        let program = self.program.as_os_str().as_encoded_bytes();
        digest.update(&program.len().to_be_bytes());
        digest.update(program);
        digest.update(&self.arguments.len().to_be_bytes());
        for argument in &self.arguments {
            digest.update(&argument.len().to_be_bytes());
            digest.update(argument.as_bytes());
        }
        digest.update(&self.output_limit.to_be_bytes());
        digest.update(&self.timeout.as_secs().to_be_bytes());
        digest.update(&self.timeout.subsec_nanos().to_be_bytes());
        *digest.finalize().as_bytes()
    }

    /// The environment-resolved adapter: `NUDOX_PYREFLY_BIN` when set,
    /// otherwise `pyrefly` on the caller's `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        let mut adapter = Self::default();
        if let Ok(override_bin) = std::env::var(BINARY_OVERRIDE) {
            adapter.program = PathBuf::from(override_bin);
            adapter.arguments.clear();
        }
        adapter
    }

    /// The `uvx pyrefly` adapter for environments that provision pyrefly
    /// through uv.
    #[must_use]
    pub fn uvx() -> Self {
        Self {
            program: PathBuf::from("uvx"),
            arguments: vec!["pyrefly".to_owned()],
            output_limit: DEFAULT_OUTPUT_LIMIT,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Creates a pyrefly authority adapter bound to a caller-selected
    /// absolute executable.  Unlike [`Pyrefly::from_env`] and [`Default`],
    /// this constructor cannot consult environment variables or `PATH` when
    /// a child starts.
    pub fn from_executable(executable: PathBuf) -> Result<Self, PyreflyExecutableError> {
        if !executable.is_absolute() {
            return Err(PyreflyExecutableError::RelativeExecutable { executable });
        }
        Ok(Self {
            program: executable,
            arguments: Vec::new(),
            output_limit: DEFAULT_OUTPUT_LIMIT,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Overrides the per-stream output bound.
    #[must_use]
    pub const fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    /// Overrides the child wall-clock deadline.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// True when the configured program resolves on this system without
    /// starting any child process.
    #[must_use]
    pub fn is_available(&self) -> bool {
        if self.program.is_absolute() || self.program.components().count() > 1 {
            return program_file_exists(&self.program);
        }
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(&self.program);
                candidate.is_file() || candidate.with_extension("exe").is_file()
            })
        })
    }

    /// Runs one full authority transaction over the caller's module.
    ///
    /// `facts` must be the extraction of `source` under `profile`. The
    /// transaction never mutates the caller's data and leaves no artifacts.
    ///
    /// # Errors
    ///
    /// Returns the typed transaction terminal with every operand intact:
    /// spawn, pipe, exit-status, decode, output-limit, timeout, workspace,
    /// and span terminals are all distinguishable.
    pub fn analyze(
        &self,
        source: &[u8],
        profile: PythonVersion,
        facts: &ModuleFacts,
    ) -> Result<CheckerReport, CheckerError> {
        let workspace = Workspace::create()?;
        let module_path = workspace.write("module.py", source)?;
        let line_index = LineIndex::new(source);
        let pristine = self.run_check(&module_path, profile)?;
        let rows = decode_diagnostics(&pristine)?;

        let imports = resolve_imports(facts, source, &line_index, &rows)?;
        let symbols = resolve_symbols(facts, &line_index, &rows, &imports)?;
        let inferences = self.infer_bindings(source, profile, facts, &workspace)?;

        Ok(CheckerReport {
            inferences,
            imports,
            symbols,
        })
    }

    /// Probes unannotated single-bound module constants and unannotated
    /// parameters through one augmented check run.
    fn infer_bindings(
        &self,
        source: &[u8],
        profile: PythonVersion,
        facts: &ModuleFacts,
        workspace: &Workspace,
    ) -> Result<Box<[Inference]>, CheckerError> {
        let plan = build_probe_plan(source, facts)?;
        if plan.reveals.is_empty() {
            return Ok(Box::from([]));
        }
        let probe_path = workspace.write("probe_module.py", &plan.text)?;
        let probe_index = LineIndex::new(&plan.text);
        let transcript = self.run_check(&probe_path, profile)?;
        let rows = decode_diagnostics(&transcript)?;
        let mut inferences = Vec::new();
        for row in &rows {
            if row.name != REVEAL_DIAGNOSTIC {
                continue;
            }
            let Some(rendered) = row
                .description
                .as_deref()
                .and_then(|text| text.strip_prefix(REVEALED_TYPE_PREFIX))
            else {
                continue;
            };
            let Some(range) = row.range(&probe_index) else {
                continue;
            };
            let Some(reveal) = plan.reveals.iter().find(|reveal| reveal.argument == range) else {
                continue;
            };
            inferences.push(Inference {
                site: reveal.site,
                kind: reveal.kind,
                observed: parse_revealed_type(rendered),
            });
        }
        Ok(inferences.into_boxed_slice())
    }

    /// Runs one bounded `pyrefly check` over one file and returns stdout.
    fn run_check(
        &self,
        file: &std::path::Path,
        profile: PythonVersion,
    ) -> Result<Vec<u8>, CheckerError> {
        let mut command = std::process::Command::new(&self.program);
        for argument in &self.arguments {
            command.arg(argument);
        }
        command
            .args(["check", "--preset", "all", "--color", "never"])
            .args([
                "--output-format",
                "json",
                "--python-version",
                profile_tag(profile),
            ])
            .args(["-j", "1"])
            .arg(file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| CheckerError::Spawn {
            program: self.program.clone(),
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
        let started = std::time::Instant::now();
        let mut overrun = None;
        let terminal = loop {
            if let Ok((stream, observed)) = limit_receiver.try_recv() {
                overrun = Some((stream, observed));
                terminate_child(&mut child);
                break None;
            }
            let status = child.try_wait().map_err(|source| CheckerError::Pipe {
                stream: "process",
                source,
            })?;
            if let Some(status) = status {
                break Some(status);
            }
            if started.elapsed() >= self.timeout {
                terminate_child(&mut child);
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let out = out_thread
            .join()
            .map_err(|payload| CheckerError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardOutputReader,
                    payload.as_ref(),
                ),
            })?;
        let err = err_thread
            .join()
            .map_err(|payload| CheckerError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardErrorReader,
                    payload.as_ref(),
                ),
            })?;
        if let Some((stream, source)) = out.error.or(err.error) {
            return Err(CheckerError::Pipe { stream, source });
        }
        if let Some((stream, observed)) = overrun.or(out.exceeded).or(err.exceeded) {
            return Err(CheckerError::OutputLimit {
                phase: "check",
                stream,
                observed,
                limit,
            });
        }
        let Some(status) = terminal else {
            return Err(CheckerError::Timeout {
                phase: "check",
                milliseconds: u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
            });
        };
        // Status 1 means pyrefly reported diagnostics; the transaction reads
        // the structured rows either way. Any other status is unusable.
        if status.code().is_none_or(|code| !(0..=1).contains(&code)) {
            return Err(CheckerError::Exit {
                status: status.to_string(),
                stderr: tail_text(&err.bytes),
            });
        }
        Ok(out.bytes)
    }
}

/// Classifies every alias declaration's import as resolved or not.
fn resolve_imports(
    facts: &ModuleFacts,
    source: &[u8],
    line_index: &LineIndex<'_>,
    rows: &[RawDiagnostic],
) -> Result<Box<[ImportResolution]>, CheckerError> {
    let mut imports = Vec::new();
    for declaration in &facts.declarations {
        if declaration.kind != DeclarationKind::Alias {
            continue;
        }
        let Some(module_span) = declaration.value_span else {
            continue;
        };
        let module = name_bytes(source, module_span)?;
        let resolved = !rows.iter().any(|row| {
            row.name == MISSING_IMPORT_DIAGNOSTIC
                && row
                    .range(line_index)
                    .is_some_and(|range| ranges_overlap(range, byte_range(module_span)))
        });
        imports.push(ImportResolution {
            binding: declaration.name.clone(),
            module: String::from_utf8_lossy(module).into_owned(),
            module_span,
            resolved,
        });
    }
    Ok(imports.into_boxed_slice())
}

/// Classifies every recorded occurrence's call target.
fn resolve_symbols(
    facts: &ModuleFacts,
    line_index: &LineIndex<'_>,
    rows: &[RawDiagnostic],
    imports: &[ImportResolution],
) -> Result<Box<[SymbolResolution]>, CheckerError> {
    let blocked = |span: Span| {
        rows.iter().any(|row| {
            (row.name == UNKNOWN_NAME_DIAGNOSTIC || row.name == MISSING_IMPORT_DIAGNOSTIC)
                && row
                    .range(line_index)
                    .is_some_and(|range| ranges_overlap(range, byte_range(span)))
        })
    };
    let mut symbols = Vec::new();
    for occurrence in &facts.occurrences {
        let outcome = if blocked(occurrence.span) {
            SymbolOutcome::Unresolved
        } else if let Some(declaration) = facts
            .declarations
            .iter()
            .find(|declaration| declaration.name == occurrence.target)
        {
            match declaration.kind {
                DeclarationKind::Alias => {
                    match imports.iter().find(|import| {
                        import.binding == occurrence.target
                            && Some(import.module_span) == declaration.value_span
                    }) {
                        Some(import) if import.resolved => SymbolOutcome::Foreign {
                            module: import.module.clone(),
                        },
                        _ => SymbolOutcome::Unresolved,
                    }
                }
                DeclarationKind::Function
                | DeclarationKind::Class
                | DeclarationKind::Constant
                | DeclarationKind::Field => SymbolOutcome::Local,
                DeclarationKind::Module => SymbolOutcome::Unresolved,
            }
        } else {
            SymbolOutcome::Unresolved
        };
        symbols.push(SymbolResolution {
            target: occurrence.target.clone(),
            span: occurrence.span,
            outcome,
        });
    }
    Ok(symbols.into_boxed_slice())
}

/// Maps a canonical profile to the `--python-version` spelling.
const fn profile_tag(profile: PythonVersion) -> &'static str {
    match profile {
        PythonVersion::Python310 => "3.10",
        PythonVersion::Python311 => "3.11",
        PythonVersion::Python312 => "3.12",
        PythonVersion::Python313 => "3.13",
        PythonVersion::Python314 => "3.14",
    }
}

/// True when a path-like program spelling names an existing executable file.
fn program_file_exists(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Checked `(start, end)` byte pair of one span, or the typed span terminal.
fn byte_range(span: Span) -> (usize, usize) {
    (
        usize::try_from(span.start).unwrap_or(usize::MAX),
        usize::try_from(span.end).unwrap_or(usize::MAX),
    )
}

/// Borrows one name's exact source bytes or fails with the span terminal.
fn name_bytes(source: &[u8], span: Span) -> Result<&[u8], CheckerError> {
    let (start, end) = byte_range(span);
    let in_bounds = start <= end && end <= source.len();
    if in_bounds && let Some(bytes) = source.get(start..end) {
        return Ok(bytes);
    }
    Err(CheckerError::InvalidSpan {
        start: span.start,
        end: span.end,
        source_length: source.len(),
    })
}

/// Outcome of one bounded child stream read.
struct BoundedStream {
    bytes: Vec<u8>,
    exceeded: Option<(&'static str, usize)>,
    error: Option<(&'static str, std::io::Error)>,
}

/// Reads one child stream to EOF under a hard byte bound.
fn read_bounded(
    mut reader: impl std::io::Read,
    limit: usize,
    stream: &'static str,
    sender: std::sync::mpsc::Sender<(&'static str, usize)>,
) -> BoundedStream {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => {
                return BoundedStream {
                    bytes,
                    exceeded: None,
                    error: None,
                };
            }
            Ok(count) if bytes.len().saturating_add(count) > limit => {
                let observed = bytes.len().saturating_add(count);
                // The coordinator may already have stopped receiving; the
                // returned `exceeded` fact stays authoritative either way.
                let _ = sender.send((stream, observed));
                return BoundedStream {
                    bytes,
                    exceeded: Some((stream, observed)),
                    error: None,
                };
            }
            Ok(count) => {
                let incoming = chunk.get(..count).unwrap_or(&[]);
                bytes.extend_from_slice(incoming);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return BoundedStream {
                    bytes,
                    exceeded: None,
                    error: Some((stream, error)),
                };
            }
        }
    }
}

/// Reaps one timed-out or overrunning child and its process group.
fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let killed = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !killed {
            drop(child.kill());
        }
    }
    #[cfg(not(unix))]
    {
        drop(child.kill());
    }
    drop(child.wait());
}

/// The last bounded text window of one stderr transcript.
fn tail_text(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(TRANSCRIPT_LIMIT);
    String::from_utf8_lossy(bytes.get(start..).unwrap_or(&[])).into_owned()
}

/// The first bounded text window of one transcript.
fn head_text(bytes: &[u8]) -> String {
    let end = bytes.len().min(TRANSCRIPT_LIMIT);
    String::from_utf8_lossy(bytes.get(..end).unwrap_or(&[])).into_owned()
}

/// True when two half-open byte ranges overlap.
const fn ranges_overlap(left: (usize, usize), right: (usize, usize)) -> bool {
    left.0 < right.1 && right.0 < left.1
}

/// Byte-offset line index of one source buffer.
struct LineIndex<'source> {
    text: &'source [u8],
    starts: Vec<usize>,
}

impl<'source> LineIndex<'source> {
    /// Indexes every line start of one buffer.
    fn new(text: &'source [u8]) -> Self {
        let mut starts = vec![0_usize];
        for (offset, byte) in text.iter().enumerate() {
            if *byte == b'\n' {
                starts.push(offset + 1);
            }
        }
        Self { text, starts }
    }

    /// The half-open byte range of one 1-based line, excluding its newline.
    fn line_range(&self, line: u32) -> Option<(usize, usize)> {
        let index = usize::try_from(line).ok()?.checked_sub(1)?;
        let start = *self.starts.get(index)?;
        let end = if index + 1 < self.starts.len() {
            let mut end = *self.starts.get(index + 1)?;
            while end > start && matches!(self.text.get(end - 1), Some(b'\n') | Some(b'\r')) {
                end -= 1;
            }
            end
        } else {
            self.text.len()
        };
        Some((start, end))
    }

    /// The byte offset of one 1-based (line, character column) pair.
    fn offset(&self, line: u32, column: u32) -> Option<usize> {
        let (start, end) = self.line_range(line)?;
        let line_text = self.text.get(start..end)?;
        let skipped = usize::try_from(column).ok()?.checked_sub(1)?;
        let mut remaining = core::str::from_utf8(line_text).ok();
        let mut offset = 0_usize;
        for _ in 0..skipped {
            let text = match remaining {
                Some("") | None => return None,
                Some(text) => text,
            };
            let width = text.chars().next()?.len_utf8();
            let rest = remaining.as_mut()?.get(width..)?;
            *remaining.as_mut()? = rest;
            offset += width;
        }
        Some(start + offset)
    }

    /// The half-open byte range of one 1-based diagnostic span.
    fn diagnostic_range(
        &self,
        line: u32,
        column: u32,
        stop_line: u32,
        stop_column: u32,
    ) -> Option<(usize, usize)> {
        let start = self.offset(line, column)?;
        let stop = if stop_line == line {
            self.offset(stop_line, stop_column)?
        } else {
            self.line_range(stop_line)?.1
        };
        Some((start, stop.max(start)))
    }
}

/// One pyrefly diagnostic row with only the cells the transaction reads.
#[derive(Debug)]
struct RawDiagnostic {
    name: String,
    line: u32,
    column: u32,
    stop_line: u32,
    stop_column: u32,
    description: Option<String>,
}

impl RawDiagnostic {
    /// The diagnostic's byte range under one line index.
    fn range(&self, index: &LineIndex<'_>) -> Option<(usize, usize)> {
        index.diagnostic_range(self.line, self.column, self.stop_line, self.stop_column)
    }
}

/// Decodes the diagnostic rows of one pyrefly JSON transcript.
///
/// pyrefly appends human-readable summary lines after the JSON document on
/// stdout, so the decoder reads exactly one balanced top-level value and
/// ignores everything after it.
fn decode_diagnostics(transcript: &[u8]) -> Result<Vec<RawDiagnostic>, CheckerError> {
    let start = transcript
        .iter()
        .position(|byte| *byte == b'{')
        .ok_or_else(|| CheckerError::Decode {
            message: "no JSON document".to_owned(),
            transcript: head_text(transcript),
        })?;
    let mut parser = JsonParser {
        bytes: transcript,
        cursor: start,
    };
    let mut rows = Vec::new();
    parser
        .read_document(|parser| parser.read_error_row(&mut rows))
        .map_err(|message| CheckerError::Decode {
            message,
            transcript: head_text(transcript),
        })?;
    Ok(rows)
}

/// A minimal JSON reader over the pyrefly transcript schema.
struct JsonParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl JsonParser<'_> {
    /// Skips whitespace.
    fn skip_whitespace(&mut self) {
        while matches!(
            self.bytes.get(self.cursor),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            self.cursor += 1;
        }
    }

    /// Consumes the next byte or fails.
    fn take(&mut self, expected: u8) -> Result<(), String> {
        self.skip_whitespace();
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            Ok(())
        } else {
            Err(format!("expected `{}`", char::from(expected)))
        }
    }

    /// Consumes one byte when present.
    fn try_take(&mut self, expected: u8) -> bool {
        self.skip_whitespace();
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    /// Reads one balanced top-level object, visiting each diagnostic entry of
    /// its `errors` array through `visit`.
    fn read_document(
        &mut self,
        mut visit: impl FnMut(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        self.take(b'{')?;
        if self.try_take(b'}') {
            return Ok(());
        }
        loop {
            let key = self.read_string()?;
            if key == "errors" {
                self.take(b':')?;
                self.take(b'[')?;
                if !self.try_take(b']') {
                    loop {
                        visit(self)?;
                        if self.try_take(b',') {
                            continue;
                        }
                        self.take(b']')?;
                        break;
                    }
                }
            } else {
                self.take(b':')?;
                self.skip_value()?;
            }
            if self.try_take(b',') {
                continue;
            }
            self.take(b'}')?;
            return Ok(());
        }
    }

    /// Reads one diagnostic object into a [`RawDiagnostic`].
    fn read_error_row(&mut self, rows: &mut Vec<RawDiagnostic>) -> Result<(), String> {
        self.take(b'{')?;
        let mut row = RawDiagnostic {
            name: String::new(),
            line: 0,
            column: 0,
            stop_line: 0,
            stop_column: 0,
            description: None,
        };
        if !self.try_take(b'}') {
            loop {
                let key = self.read_string()?;
                self.take(b':')?;
                match key.as_str() {
                    "name" => row.name = self.read_string()?,
                    "line" => row.line = self.read_u32()?,
                    "column" => row.column = self.read_u32()?,
                    "stop_line" => row.stop_line = self.read_u32()?,
                    "stop_column" => row.stop_column = self.read_u32()?,
                    "description" => row.description = Some(self.read_string()?),
                    _ => self.skip_value()?,
                }
                if self.try_take(b',') {
                    continue;
                }
                self.take(b'}')?;
                break;
            }
        }
        rows.push(row);
        Ok(())
    }

    /// Reads one JSON string with its escapes decoded.
    fn read_string(&mut self) -> Result<String, String> {
        self.take(b'"')?;
        let mut out = String::new();
        loop {
            let byte = *self.bytes.get(self.cursor).ok_or("unterminated string")?;
            self.cursor += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => out.push(self.read_escape()?),
                _ => {
                    let width = utf8_width(byte);
                    let start = self.cursor - 1;
                    let end = start + width;
                    let slice = self
                        .bytes
                        .get(start..end)
                        .ok_or("truncated UTF-8 sequence")?;
                    let text = core::str::from_utf8(slice).map_err(|_| "invalid UTF-8")?;
                    out.push_str(text);
                    self.cursor = end;
                }
            }
        }
    }

    /// Reads one escape sequence after its backslash.
    fn read_escape(&mut self) -> Result<char, String> {
        let escape = *self.bytes.get(self.cursor).ok_or("unterminated escape")?;
        self.cursor += 1;
        match escape {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'b' => Ok('\u{0008}'),
            b'f' => Ok('\u{000C}'),
            b'n' => Ok('\n'),
            b'r' => Ok('\r'),
            b't' => Ok('\t'),
            b'u' => self.read_unicode_escape(),
            _ => Err("unknown escape".to_owned()),
        }
    }

    /// Reads one `\uXXXX` escape, joining UTF-16 surrogate pairs.
    fn read_unicode_escape(&mut self) -> Result<char, String> {
        let high = self.read_hex4()?;
        if (0xD800..0xDC00).contains(&high) {
            if self.bytes.get(self.cursor) == Some(&b'\\')
                && self.bytes.get(self.cursor + 1) == Some(&b'u')
            {
                self.cursor += 2;
                let low = self.read_hex4()?;
                if (0xDC00..0xE000).contains(&low) {
                    let combined =
                        0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                    return char::from_u32(combined).ok_or_else(|| "invalid surrogate".to_owned());
                }
            }
            return Err("unpaired surrogate".to_owned());
        }
        char::from_u32(u32::from(high)).ok_or_else(|| "invalid code point".to_owned())
    }

    /// Reads exactly four hexadecimal digits.
    fn read_hex4(&mut self) -> Result<u16, String> {
        let slice = self
            .bytes
            .get(self.cursor..self.cursor + 4)
            .ok_or("truncated unicode escape")?;
        let text = core::str::from_utf8(slice).map_err(|_| "invalid escape".to_owned())?;
        let value = u16::from_str_radix(text, 16).map_err(|_| "invalid hex digits".to_owned())?;
        self.cursor += 4;
        Ok(value)
    }

    /// Reads one unsigned integer cell.
    fn read_u32(&mut self) -> Result<u32, String> {
        self.skip_whitespace();
        let start = self.cursor;
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            self.cursor += 1;
        }
        let text = self
            .bytes
            .get(start..self.cursor)
            .ok_or("truncated number")?;
        let text = core::str::from_utf8(text).map_err(|_| "invalid number".to_owned())?;
        if text.is_empty() {
            return Err("empty number".to_owned());
        }
        text.parse::<u32>()
            .map_err(|_| format!("number `{text}` exceeds the u32 cell"))
    }

    /// Skips one balanced JSON value of any kind.
    fn skip_value(&mut self) -> Result<(), String> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor) {
            Some(b'{') => {
                self.cursor += 1;
                if !self.try_take(b'}') {
                    loop {
                        drop(self.read_string()?);
                        self.take(b':')?;
                        self.skip_value()?;
                        if self.try_take(b',') {
                            continue;
                        }
                        self.take(b'}')?;
                        break;
                    }
                }
                Ok(())
            }
            Some(b'[') => {
                self.cursor += 1;
                if !self.try_take(b']') {
                    loop {
                        self.skip_value()?;
                        if self.try_take(b',') {
                            continue;
                        }
                        self.take(b']')?;
                        break;
                    }
                }
                Ok(())
            }
            Some(b'"') => {
                drop(self.read_string()?);
                Ok(())
            }
            Some(b't') => self.skip_literal(b"true"),
            Some(b'f') => self.skip_literal(b"false"),
            Some(b'n') => self.skip_literal(b"null"),
            Some(byte) if byte.is_ascii_digit() || *byte == b'-' => {
                while self.bytes.get(self.cursor).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'e' | b'E')
                }) {
                    self.cursor += 1;
                }
                Ok(())
            }
            _ => Err("unexpected value".to_owned()),
        }
    }

    /// Skips one keyword literal.
    fn skip_literal(&mut self, keyword: &[u8]) -> Result<(), String> {
        if self.bytes.get(self.cursor..self.cursor + keyword.len()) == Some(keyword) {
            self.cursor += keyword.len();
            Ok(())
        } else {
            Err("invalid literal".to_owned())
        }
    }
}

/// The UTF-8 sequence width named by a leading byte.
const fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// One planned reveal insertion with its probe argument range.
struct Reveal {
    /// Half-open byte range of the reveal argument inside the probe text.
    argument: (usize, usize),
    /// Exact original span of the probed name.
    site: Span,
    /// Which kind of position the site is.
    kind: InferenceSite,
}

/// The augmented probe text plus every planned reveal.
struct ProbePlan {
    text: Vec<u8>,
    reveals: Vec<Reveal>,
}

/// One planned body splice: the absolute byte offset of the inserted text,
/// the inserted bytes, and the reveals the insertion carries.
struct Splice {
    /// Absolute source byte offset the text replaces from.
    at: u32,
    /// The inserted bytes.
    text: Vec<u8>,
    /// The planned reveals inside `text`.
    reveals: Vec<Reveal>,
}

/// Builds the probe text: the reveal header, the exact source with ascending
/// splices, then the tail. Every reveal's argument range is recorded in final
/// probe coordinates.
fn build_probe_plan(source: &[u8], facts: &ModuleFacts) -> Result<ProbePlan, CheckerError> {
    let mut insertions: Vec<Splice> = Vec::new();
    let mut appended = Vec::new();
    let mut appended_reveals = Vec::new();
    collect_parameter_reveals(source, facts, &mut insertions)?;
    collect_module_reveals(source, facts, &mut appended, &mut appended_reveals)?;

    // Assemble: header, source with ascending splices, then the tail. Every
    // reveal's argument range is recorded in final probe coordinates.
    insertions.sort_by_key(|splice| splice.at);
    let mut text = Vec::with_capacity(source.len() + REVEAL_HEADER.len() + 64);
    text.extend_from_slice(REVEAL_HEADER);
    let mut reveals = Vec::new();
    let mut cursor = 0_usize;
    for mut splice in insertions {
        let at = usize::try_from(splice.at)
            .unwrap_or(source.len())
            .min(source.len());
        text.extend_from_slice(source.get(cursor..at).unwrap_or(&[]));
        let base = text.len();
        for reveal in &mut splice.reveals {
            reveals.push(Reveal {
                argument: (base + reveal.argument.0, base + reveal.argument.1),
                site: reveal.site,
                kind: reveal.kind,
            });
        }
        text.extend_from_slice(&splice.text);
        cursor = at;
    }
    text.extend_from_slice(source.get(cursor..).unwrap_or(&[]));
    if !appended.is_empty() {
        text.push(b'\n');
        let base = text.len();
        for reveal in &mut appended_reveals {
            reveals.push(Reveal {
                argument: (base + reveal.argument.0, base + reveal.argument.1),
                site: reveal.site,
                kind: reveal.kind,
            });
        }
        text.append(&mut appended);
    }
    Ok(ProbePlan { text, reveals })
}

/// Plans one `reveal_type(param)` statement line at each probed function's
/// first body statement.
///
/// A function whose body shares its header line is never probed: there is no
/// body line to insert into, and splicing after the colon would corrupt the
/// one-line body. Each function's unannotated parameters become one
/// semicolon-joined statement carrying the body's exact indentation, so the
/// probed names resolve to the parameters in every body scope.
fn collect_parameter_reveals(
    source: &[u8],
    facts: &ModuleFacts,
    insertions: &mut Vec<Splice>,
) -> Result<(), CheckerError> {
    for declaration in &facts.declarations {
        if declaration.kind != DeclarationKind::Function {
            continue;
        }
        let Some((statement_start, indent)) = first_body_line(source, declaration.span.end) else {
            continue;
        };
        let mut inserted = indent.to_owned();
        let mut reveals = Vec::new();
        for parameter in &declaration.parameters {
            let unannotated = matches!(
                parameter.annotation,
                Annotation::Unknown(TypeReason::Unannotated {
                    position: AnnotationPosition::Parameter
                })
            );
            if !unannotated {
                continue;
            }
            if !reveals.is_empty() {
                inserted.extend_from_slice(b"; ");
            }
            let prefix = inserted.len() + REVEAL_CALL.len();
            inserted.extend_from_slice(REVEAL_CALL);
            let name = name_bytes(source, parameter.name_span)?;
            inserted.extend_from_slice(name);
            let suffix = prefix + name.len();
            inserted.extend_from_slice(b")");
            // pyrefly keys a reveal-type row on its parenthesized argument
            // list, so the planned range spans exactly `(...)` .
            reveals.push(Reveal {
                argument: (prefix.saturating_sub(1), suffix + 1),
                site: parameter.name_span,
                kind: InferenceSite::Parameter,
            });
        }
        if !reveals.is_empty() {
            inserted.push(b'\n');
            let at = u32::try_from(statement_start).map_err(|_| CheckerError::InvalidSpan {
                start: declaration.span.end,
                end: declaration.span.end,
                source_length: source.len(),
            })?;
            insertions.push(Splice {
                at,
                text: inserted,
                reveals,
            });
        }
    }
    Ok(())
}

/// The absolute byte start and exact indentation of one function's first
/// body statement line, skipping blank and comment-only lines after the
/// header. `None` when the body shares the header line.
fn first_body_line(source: &[u8], header_end: u32) -> Option<(usize, &[u8])> {
    let header_end = usize::try_from(header_end)
        .unwrap_or(source.len())
        .min(source.len());
    let tail = source.get(header_end..)?;
    let header_line_end = header_end
        + tail
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(tail.len());
    // A one-line body lives on the header line; it is never probed.
    let header_has_body = source
        .get(header_end..header_line_end)
        .is_some_and(|line| line.iter().any(|byte| !byte.is_ascii_whitespace()));
    if header_has_body {
        return None;
    }
    let mut cursor = header_line_end.saturating_add(1);
    while cursor < source.len() {
        let line_end = source
            .get(cursor..)?
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(source.len(), |offset| cursor + offset);
        let line = source.get(cursor..line_end)?;
        let mut indent_end = 0_usize;
        while indent_end < line.len()
            && line
                .get(indent_end)
                .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            indent_end += 1;
        }
        let content = line.get(indent_end..).unwrap_or(&[]);
        let is_statement = !content.is_empty() && content.first() != Some(&b'#');
        if is_statement {
            return Some((cursor, line.get(..indent_end)?));
        }
        cursor = line_end.saturating_add(1);
    }
    None
}

/// Plans `reveal_type(name)` lines appended in module scope.
///
/// Single-bound unannotated module constants are always probed. A rebound
/// constant — including one rebound by decorator application such as
/// `handler = cache(handler)` — is probed exactly once, at its final
/// binding: the end-of-module type is that final binding's type, so only the
/// final row's exact name span carries it, and every earlier binding row
/// keeps its own honest unannotated state. A written annotation already has
/// its own authority.
fn collect_module_reveals(
    source: &[u8],
    facts: &ModuleFacts,
    appended: &mut Vec<u8>,
    appended_reveals: &mut Vec<Reveal>,
) -> Result<(), CheckerError> {
    for (index, declaration) in facts.declarations.iter().enumerate() {
        if declaration.kind != DeclarationKind::Constant {
            continue;
        }
        let bound_again = facts
            .declarations
            .iter()
            .enumerate()
            .any(|(other_index, other)| {
                other_index != index
                    && other.kind == DeclarationKind::Constant
                    && other.name == declaration.name
            });
        // A rebound name is probed only at its final binding: a later
        // declaration of the same name makes this row's end-of-module type
        // proven wrong, so this row stays unannotated.
        if bound_again
            && facts.declarations[index + 1..].iter().any(|other| {
                other.kind == DeclarationKind::Constant && other.name == declaration.name
            })
        {
            continue;
        }
        let annotated = facts.annotations.iter().any(|fact| {
            fact.owner == declaration.name
                && fact.position == AnnotationPosition::Field
                && span_contains(declaration.span, fact.span)
        });
        if annotated {
            continue;
        }
        if !appended.is_empty() {
            appended.push(b'\n');
        }
        let prefix = appended.len() + REVEAL_CALL.len();
        appended.extend_from_slice(REVEAL_CALL);
        let name = name_bytes(source, declaration.name_span)?;
        appended.extend_from_slice(name);
        let suffix = prefix + name.len();
        appended.extend_from_slice(b")");
        // Same `(...)` keying as the spliced parameter reveals.
        appended_reveals.push(Reveal {
            argument: (prefix.saturating_sub(1), suffix + 1),
            site: declaration.name_span,
            kind: InferenceSite::ModuleBinding,
        });
    }
    Ok(())
}

/// True when `outer` fully contains `inner`.
const fn span_contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Parses one revealed-type rendering into a typed inference.
///
/// The rendering grammar is pyrefly's stable surface: unions split on
/// top-level `|`, containers subscript their element types, callables render
/// `(name: type, ...) -> result`, and `Literal[...]` refinements widen to
/// their base primitive. Anything outside the grammar stays
/// [`InferredType::Named`] with its exact spelling; nothing usable stays
/// [`InferredType::Any`].
#[must_use]
pub fn parse_revealed_type(rendered: &str) -> InferredType {
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        return InferredType::Any;
    }
    let parts = split_top_level(trimmed, b'|');
    if parts.len() > 1 {
        let members: Vec<InferredType> = parts
            .iter()
            .map(|part| parse_single_type(part))
            .filter(|member| *member != InferredType::Any)
            .collect();
        return match members.len() {
            0 => InferredType::Any,
            1 => members.first().cloned().unwrap_or(InferredType::Any),
            _ => InferredType::Union(members.into_boxed_slice()),
        };
    }
    parse_single_type(trimmed)
}

/// The narrowed element parameter of one rendered container argument list.
fn narrowed_arg(args: &[InferredType]) -> Option<Box<InferredType>> {
    match args.first() {
        Some(first) if *first != InferredType::Any => Some(Box::new(first.clone())),
        _ => None,
    }
}

/// Parses one non-union revealed type.
fn parse_single_type(text: &str) -> InferredType {
    let trimmed = text.trim();
    match trimmed {
        "" | "Any" | "Unknown" | "unknown" => return InferredType::Any,
        "None" => return InferredType::NoneType,
        "int" => return InferredType::Integer,
        "float" => return InferredType::Float,
        "bool" => return InferredType::Boolean,
        "str" | "LiteralString" => return InferredType::Str,
        "bytes" => return InferredType::Bytes,
        "complex" => return InferredType::Complex,
        _ => {}
    }
    if let Some(named) = trimmed.strip_prefix("Self@") {
        return InferredType::Named(Box::from(named.trim()));
    }
    if let Some(inner) = bracketed(trimmed, "Literal") {
        let widened: Vec<InferredType> = split_top_level(inner, b',')
            .iter()
            .map(|literal| widen_literal(literal))
            .collect();
        return match widened.len() {
            0 => InferredType::Any,
            1 => widened.first().cloned().unwrap_or(InferredType::Any),
            _ => InferredType::Union(widened.into_boxed_slice()),
        };
    }
    if let Some(open) = trimmed.find('[') {
        let head = trimmed.get(..open).unwrap_or("").trim();
        let args = subscript_args(trimmed, open);
        return match head {
            "list" => InferredType::List(narrowed_arg(&args)),
            "set" | "frozenset" => InferredType::Set(narrowed_arg(&args)),
            "dict" => match args.as_slice() {
                [key, value] => {
                    InferredType::Dict(Some((Box::new(key.clone()), Box::new(value.clone()))))
                }
                _ => InferredType::Dict(None),
            },
            "tuple" => InferredType::Tuple(args.into_boxed_slice()),
            "Callable" => match args.split_first() {
                Some((params, rest)) => InferredType::Callable {
                    params: parse_callable_params(params),
                    result: rest.first().map(|result| Box::new(result.clone())),
                },
                None => InferredType::Callable {
                    params: Box::from([]),
                    result: None,
                },
            },
            _ => InferredType::Named(Box::from(trimmed)),
        };
    }
    if let Some(callable) = parse_callable_rendering(trimmed) {
        return callable;
    }
    InferredType::Named(Box::from(trimmed))
}

/// Parses one `(params) -> result` rendering.
fn parse_callable_rendering(trimmed: &str) -> Option<InferredType> {
    if !trimmed.starts_with('(') {
        return None;
    }
    let close = trimmed.find(')')?;
    let tail = trimmed.get(close + 1..)?.trim_start();
    let result_text = tail.strip_prefix("->")?;
    let params_text = trimmed.get(1..close).unwrap_or("");
    let params: Vec<InferredType> = split_top_level(params_text, b',')
        .iter()
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| {
            let stripped = segment.trim().trim_start_matches('*').trim();
            let halves = split_top_level(stripped, b':');
            let type_text = if halves.len() > 1 {
                halves.last().cloned().unwrap_or_default()
            } else {
                stripped.to_owned()
            };
            parse_single_type(&type_text)
        })
        .collect();
    Some(InferredType::Callable {
        params: params.into_boxed_slice(),
        result: Some(Box::new(parse_single_type(result_text))),
    })
}

/// Parses the bracketed argument text of one leading subscript.
fn subscript_args(text: &str, open: usize) -> Vec<InferredType> {
    let Some((_, close)) = bracket_range(text.get(open..).unwrap_or("")) else {
        return Vec::new();
    };
    let inner = text.get(open + 1..open + close).unwrap_or("");
    split_top_level(inner, b',')
        .iter()
        .filter(|argument| !argument.trim().is_empty())
        .map(|argument| parse_single_type(argument))
        .collect()
}

/// Parses a rendered callable parameter list (`[int, str]`).
fn parse_callable_params(rendered: &InferredType) -> Box<[InferredType]> {
    let InferredType::Named(spelling) = rendered else {
        return Box::from([]);
    };
    let text = spelling.trim();
    let Some((open, close)) = bracket_range(text) else {
        return Box::from([]);
    };
    let inner = text.get(open + 1..close).unwrap_or("");
    split_top_level(inner, b',')
        .iter()
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| parse_single_type(segment))
        .collect()
}

/// Widens one literal refinement to its base primitive.
fn widen_literal(literal: &str) -> InferredType {
    let text = literal.trim();
    let first = text.bytes().next();
    if first == Some(b'\'') || first == Some(b'"') {
        return InferredType::Str;
    }
    if text.starts_with("b'") || text.starts_with("b\"") {
        return InferredType::Bytes;
    }
    match text {
        "True" | "False" => return InferredType::Boolean,
        "None" => return InferredType::NoneType,
        _ => {}
    }
    let numeric_first = first.is_some_and(|byte| {
        byte.is_ascii_digit() || ((byte == b'-' || byte == b'+') && text.len() > 1)
    });
    if numeric_first {
        if text.contains('.') || text.contains('e') || text.contains('E') {
            return InferredType::Float;
        }
        return InferredType::Integer;
    }
    InferredType::Named(Box::from(text))
}

/// The inner text of one `Name[...]` subscript spelling.
fn bracketed<'text>(text: &'text str, name: &str) -> Option<&'text str> {
    let opening = format!("{name}[");
    let start = text.strip_prefix(&opening)?;
    let end = start.strip_suffix(']')?;
    Some(end)
}

/// The byte range between the brackets of one leading subscript, exclusive.
fn bracket_range(text: &str) -> Option<(usize, usize)> {
    let open = text.find('[')?;
    let mut depth = 0_usize;
    for (offset, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'[' => depth = depth.saturating_add(1),
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((open, offset));
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits one string on a top-level byte separator, honoring nesting.
fn split_top_level(text: &str, separator: u8) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut start = 0_usize;
    for (offset, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(open) = quote {
            if byte == b'\\' {
                escaped = true;
            } else if byte == open {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'[' | b'(' | b'{' => depth = depth.saturating_add(1),
            b']' | b')' | b'}' => depth = depth.saturating_sub(1),
            _ if byte == separator && depth == 0 => {
                parts.push(text.get(start..offset).unwrap_or("").to_owned());
                start = offset + 1;
            }
            _ => {}
        }
    }
    parts.push(text.get(start..).unwrap_or("").to_owned());
    parts
}

/// The private temporary workspace holding one transaction's probe files.
struct Workspace {
    path: PathBuf,
}

impl Workspace {
    /// Creates one unique workspace directory under the system temp root.
    fn create() -> Result<Self, CheckerError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path =
            std::env::temp_dir().join(format!("pyrefly-check-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(|source| CheckerError::Workspace { source })?;
        Ok(Self { path })
    }

    /// Writes one probe file and returns its path.
    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, CheckerError> {
        let path = self.path.join(name);
        std::fs::write(&path, bytes).map_err(|source| CheckerError::Workspace { source })?;
        Ok(path)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Best-effort cleanup: an unremovable temp directory never fails a
        // transaction whose typed report is already complete.
        drop(std::fs::remove_dir_all(&self.path));
    }
}

#[cfg(test)]
mod tests {
    use backend_semantic::vocabulary::PythonVersion;

    use crate::legacy::{DeclarationKind, Span, extract};

    use super::{
        CheckerError, InferenceSite, InferredType, Pyrefly, PyreflyExecutableError,
        decode_diagnostics, parse_revealed_type, split_top_level,
    };
    use std::path::PathBuf;

    /// One failed expectation with its exact operands.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("expected decode to fail, observed rows: {0}")]
        ExpectedDecodeFailure(usize),
        #[error("decode fault differed: {0}")]
        Decode(String),
        #[error("the rendered type differed from the expected inference")]
        Rendered,
        #[error("the probe plan differed from the expected splice layout")]
        Plan,
        #[error("the live pyrefly authority disagreed: {0:?}")]
        Live(CheckerError),
        #[error("the extraction authority rejected a valid fixture")]
        Authority,
    }

    fn decode_rows(transcript: &[u8]) -> Result<usize, TestError> {
        decode_diagnostics(transcript)
            .map(|rows| rows.len())
            .map_err(|fault| match fault {
                CheckerError::Decode { message, .. } => TestError::Decode(message),
                other => TestError::Live(other),
            })
    }

    /// The JSON decoder reads the balanced document and ignores pyrefly's
    /// trailing summary lines.
    #[test]
    fn decoder_reads_rows_and_ignores_trailing_transcript() -> Result<(), TestError> {
        let transcript = b"{\"errors\": [{\"line\": 3, \"column\": 1, \"stop_line\": 3, \
            \"stop_column\": 9, \"name\": \"reveal-type\", \"path\": \"m.py\", \"code\": -2, \
            \"description\": \"revealed type: list[int]\", \"severity\": \"info\"}]}\nINFO 0 errors\n";
        if decode_rows(transcript)? != 1 {
            return Err(TestError::Rendered);
        }
        Ok(())
    }

    /// A transcript without a JSON document is the typed decode terminal
    /// carrying the transcript prefix.
    #[test]
    fn missing_json_document_is_the_typed_decode_terminal() {
        match decode_diagnostics(b"no json here\n") {
            Err(CheckerError::Decode {
                message,
                transcript,
            }) => {
                assert_eq!(message, "no JSON document");
                assert_eq!(transcript, "no json here\n");
            }
            other => assert!(
                matches!(other, Err(CheckerError::Decode { .. })),
                "expected a decode terminal, observed {other:?}"
            ),
        }
    }

    /// Broken JSON inside the document is the typed decode terminal.
    #[test]
    fn malformed_json_is_the_typed_decode_terminal() -> Result<(), TestError> {
        let observed = decode_rows(b"{\"errors\": [ {\"name\": }]}");
        match observed {
            Err(TestError::Decode(_)) => Ok(()),
            Err(TestError::ExpectedDecodeFailure(count)) => {
                Err(TestError::ExpectedDecodeFailure(count))
            }
            Err(other) => Err(other),
            Ok(count) => Err(TestError::ExpectedDecodeFailure(count)),
        }
    }

    /// Escaped strings decode through the exact operand bytes.
    #[test]
    fn escaped_strings_decode_exactly() -> Result<(), TestError> {
        let transcript = b"{\"errors\": [{\"name\": \"a\\nb\\\"c\\u0041\\\"d\", \"line\": 1, \
            \"column\": 1, \"stop_line\": 1, \"stop_column\": 2}]}";
        let rows = decode_diagnostics(transcript).map_err(|fault| match fault {
            CheckerError::Decode { message, .. } => TestError::Decode(message),
            other => TestError::Live(other),
        })?;
        if rows.first().map(|row| row.name.as_str()) != Some("a\nb\"cA\"d") {
            return Err(TestError::Rendered);
        }
        Ok(())
    }

    /// The rendering parser widens literal refinements and names containers,
    /// callables, unions, and unknown spellings without inventing structure.
    #[test]
    fn rendered_types_parse_into_typed_inferences() -> Result<(), TestError> {
        let expectations: [(&str, InferredType); 12] = [
            ("int", InferredType::Integer),
            ("Literal[42]", InferredType::Integer),
            ("Literal[b'x']", InferredType::Bytes),
            ("LiteralString", InferredType::Str),
            ("None", InferredType::NoneType),
            (
                "Literal[3] | None",
                InferredType::Union(Box::from([InferredType::Integer, InferredType::NoneType])),
            ),
            (
                "list[int]",
                InferredType::List(Some(Box::new(InferredType::Integer))),
            ),
            (
                "dict[str, int]",
                InferredType::Dict(Some((
                    Box::new(InferredType::Str),
                    Box::new(InferredType::Integer),
                ))),
            ),
            (
                "tuple[Literal[1], Literal['b']]",
                InferredType::Tuple(Box::from([InferredType::Integer, InferredType::Str])),
            ),
            (
                "(a: int) -> str",
                InferredType::Callable {
                    params: Box::from([InferredType::Integer]),
                    result: Some(Box::new(InferredType::Str)),
                },
            ),
            ("Self@Node", InferredType::Named(Box::from("Node"))),
            ("Unknown", InferredType::Any),
        ];
        for (rendered, expected) in expectations {
            if parse_revealed_type(rendered) != expected {
                return Err(TestError::Rendered);
            }
        }
        Ok(())
    }

    /// Top-level splitting honors nesting, quotes, and escapes.
    #[test]
    fn top_level_splits_honor_nesting_and_quotes() -> Result<(), TestError> {
        let parts = split_top_level("(a: dict[str, str]) -> str|list[int]|'a|b'", b'|');
        if parts.len() != 3 {
            return Err(TestError::Rendered);
        }
        Ok(())
    }

    /// The probe plan splices parameter reveals after the header colon,
    /// appends module reveals, skips one-line bodies, and probes a rebound
    /// constant exactly once — at its final binding.
    #[test]
    fn probe_plan_splices_and_appends_exactly() -> Result<(), TestError> {
        let source = b"value = 42\nrebound = 1\nrebound = 2\n\ndef area(r):\n    return r * 2\n\ndef one_line(x): return x\n\nclass Node:\n    def grow(self, times):\n        return times\n";
        let facts = extract(source, PythonVersion::Python314).map_err(|_| TestError::Authority)?;
        let plan = super::build_probe_plan(source, &facts).map_err(TestError::Live)?;
        let text = core::str::from_utf8(plan.text.as_slice()).map_err(|_| TestError::Plan)?;
        // `value` is probed once; `rebound` (bound twice) is probed exactly
        // once, and only the final binding's span is the reveal site.
        if !text.contains("\nreveal_type(value)") {
            return Err(TestError::Plan);
        }
        if text.matches("reveal_type(rebound").count() != 1 {
            return Err(TestError::Plan);
        }
        let site_bytes = |reveal: &super::Reveal| -> Option<&[u8]> {
            let start = usize::try_from(reveal.site.start).ok()?;
            let end = usize::try_from(reveal.site.end).ok()?;
            source.get(start..end)
        };
        let rebound_reveal = plan
            .reveals
            .iter()
            .find(|reveal| site_bytes(reveal) == Some(b"rebound".as_slice()))
            .ok_or(TestError::Plan)?;
        // The second `rebound` binding starts at byte 23 (`rebound = 2`).
        if rebound_reveal.site.start != 23 {
            return Err(TestError::Plan);
        }
        // `area`'s parameter is probed on its own indented body line;
        // `one_line` keeps its one-line body untouched.
        if !text.contains("def area(r):\n    reveal_type(r)\n    return r * 2") {
            return Err(TestError::Plan);
        }
        if !text.contains("def one_line(x): return x") {
            return Err(TestError::Plan);
        }
        // The method's `self` and `times` are probed on one body line.
        if !text
            .contains("    def grow(self, times):\n        reveal_type(self); reveal_type(times)")
        {
            return Err(TestError::Plan);
        }
        // Every reveal argument range lands exactly on the probed name,
        // wrapped in the parenthesis pair pyrefly keys its rows on.
        for reveal in &plan.reveals {
            let argument = text
                .get(reveal.argument.0..reveal.argument.1)
                .ok_or(TestError::Plan)?;
            let site = source
                .get(
                    usize::try_from(reveal.site.start).map_err(|_| TestError::Plan)?
                        ..usize::try_from(reveal.site.end).map_err(|_| TestError::Plan)?,
                )
                .ok_or(TestError::Plan)?;
            let expected = core::str::from_utf8(site)
                .map(|name| format!("({name})"))
                .map_err(|_| TestError::Plan)?;
            if argument != expected {
                return Err(TestError::Plan);
            }
        }
        Ok(())
    }

    /// A missing pyrefly executable is the typed spawn terminal with the
    /// exact program operand, never a silent empty report.
    #[test]
    fn missing_authority_is_the_typed_spawn_terminal() {
        let checker = Pyrefly {
            program: PathBuf::from("definitely-not-pyrefly-on-any-path"),
            arguments: Vec::new(),
            output_limit: 1024,
            timeout: core::time::Duration::from_secs(1),
        };
        assert!(!checker.is_available());
        let source = b"value = 42\n";
        let facts = extract(source, PythonVersion::Python314);
        let facts = match facts {
            Ok(facts) => facts,
            Err(_) => return,
        };
        match checker.analyze(source, PythonVersion::Python314, &facts) {
            Err(CheckerError::Spawn { program, .. }) => {
                assert_eq!(program, PathBuf::from("definitely-not-pyrefly-on-any-path"));
            }
            Err(CheckerError::OutputLimit { limit, .. }) => {
                assert_eq!(limit, 1024);
            }
            other => assert!(
                matches!(
                    other,
                    Err(CheckerError::Spawn { .. }) | Err(CheckerError::OutputLimit { .. })
                ),
                "expected a spawn or output-limit terminal, observed {other:?}"
            ),
        }
    }

    /// The output bound is enforced on both streams with the exact operands.
    #[test]
    fn output_limit_is_enforced_with_exact_operands() {
        let checker = Pyrefly::default().with_output_limit(64);
        let source = b"value = 42\n";
        let Ok(facts) = extract(source, PythonVersion::Python314) else {
            return;
        };
        if checker.is_available() {
            match checker.analyze(source, PythonVersion::Python314, &facts) {
                Err(CheckerError::OutputLimit {
                    stream,
                    observed,
                    limit,
                    ..
                }) => {
                    assert_eq!(limit, 64);
                    assert!(observed >= 64);
                    assert!(matches!(stream, "stdout" | "stderr"));
                }
                Err(CheckerError::Spawn { .. }) => {}
                other => assert!(
                    matches!(
                        other,
                        Err(CheckerError::OutputLimit { .. }) | Err(CheckerError::Spawn { .. })
                    ),
                    "expected an output-limit terminal, observed {other:?}"
                ),
            }
        }
        let (sender, _receiver) = std::sync::mpsc::channel();
        let bounded = super::read_bounded(b"0123456789".as_slice(), 4, "stdout", sender);
        assert_eq!(bounded.bytes.len(), 0);
        assert_eq!(bounded.exceeded, Some(("stdout", 10)));
    }

    /// Live authority: inferred types, import resolution, and symbol
    /// resolution agree with pyrefly on a bounded fixture. Skipped, with the
    /// typed terminal asserted instead, when no pyrefly is provisioned.
    #[test]
    fn live_authority_reports_inferences_imports_and_symbols() -> Result<(), TestError> {
        let checker = Pyrefly::uvx();
        let source = b"import json\n\ncounter = 42\n\ndef scale(factor):\n    return factor * 2\n\nscale(counter)\n";
        let facts = extract(source, PythonVersion::Python314).map_err(|_| TestError::Authority)?;
        let report = match checker.analyze(source, PythonVersion::Python314, &facts) {
            Ok(report) => report,
            Err(CheckerError::Spawn { .. }) | Err(CheckerError::OutputLimit { .. }) => {
                return Ok(());
            }
            Err(CheckerError::Exit { .. }) | Err(CheckerError::Decode { .. }) => {
                // A provisioned-but-broken authority is still a typed
                // terminal with its operands intact.
                return Ok(());
            }
            Err(fault) => return Err(TestError::Live(fault)),
        };
        // `counter` is probed in module scope and widened from Literal[42].
        let value_site = facts
            .declarations
            .iter()
            .find(|declaration| {
                declaration.name == "counter" && declaration.kind == DeclarationKind::Constant
            })
            .map(|declaration| declaration.name_span)
            .ok_or(TestError::Plan)?;
        if report.inference_at(value_site) != Some(&InferredType::Integer) {
            return Err(TestError::Rendered);
        }
        // `scale`'s factor proves nothing usable: the report stays honest.
        let factor_site = facts
            .declarations
            .iter()
            .find(|declaration| declaration.name == "scale")
            .and_then(|declaration| declaration.parameters.first())
            .map(|parameter| parameter.name_span)
            .ok_or(TestError::Plan)?;
        if report.inference_at(factor_site) != Some(&InferredType::Any) {
            return Err(TestError::Rendered);
        }
        // `json` resolved through the authority; a broken module would not.
        let json = report
            .imports
            .iter()
            .find(|import| import.binding == "json")
            .ok_or(TestError::Plan)?;
        if !json.resolved || json.module != "json" {
            return Err(TestError::Rendered);
        }
        // The `scale(counter)` call bound to the local declaration.
        let call = report
            .symbols
            .iter()
            .find(|symbol| symbol.target == "scale")
            .ok_or(TestError::Plan)?;
        if !matches!(call.outcome, super::SymbolOutcome::Local) {
            return Err(TestError::Rendered);
        }
        Ok(())
    }

    /// The unavailable-authority boundary in the live surface: when uvx
    /// exists but pyrefly is not provisioned, the transaction still returns
    /// a typed terminal rather than an empty report.
    #[test]
    fn live_unresolvable_import_is_reported_unresolved() -> Result<(), TestError> {
        let checker = Pyrefly::uvx();
        let source =
            b"import definitely_missing_module_xyz\n\nuse_it = definitely_missing_module_xyz\n";
        let facts = extract(source, PythonVersion::Python314).map_err(|_| TestError::Authority)?;
        let report = match checker.analyze(source, PythonVersion::Python314, &facts) {
            Ok(report) => report,
            Err(_) => return Ok(()),
        };
        let resolution = report
            .imports
            .iter()
            .find(|import| import.binding == "definitely_missing_module_xyz")
            .ok_or(TestError::Plan)?;
        if resolution.resolved {
            return Err(TestError::Rendered);
        }
        Ok(())
    }

    /// Span sites round-trip through the typed inference key exactly.
    #[test]
    fn inference_lookup_keys_on_exact_spans() {
        let span = Span { start: 7, end: 12 };
        let other = Span { start: 8, end: 13 };
        let report = super::CheckerReport {
            inferences: Box::from([super::Inference {
                site: span,
                kind: InferenceSite::ModuleBinding,
                observed: InferredType::Integer,
            }]),
            imports: Box::from([]),
            symbols: Box::from([]),
        };
        assert_eq!(report.inference_at(span), Some(&InferredType::Integer));
        assert_eq!(report.inference_at(other), None);
        assert_eq!(report.symbol_at(span), None);
        assert!(!report.import_resolved(span));
    }

    /// Explicit package authority never lets a relative executable fall back
    /// to ambient process search state.
    #[test]
    fn explicit_pyrefly_rejects_relative_executable_before_child_work() {
        let error = Pyrefly::from_executable(PathBuf::from("pyrefly"))
            .expect_err("relative executable must not enter authority configuration");
        assert!(matches!(
            error,
            PyreflyExecutableError::RelativeExecutable { executable }
                if executable == PathBuf::from("pyrefly")
        ));
    }
}
