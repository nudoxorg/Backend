#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "python_support/mod.rs"]
mod python_support;

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, FactFault,
    FactRejection, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use compiler_ir::{
    DecodedOccurrence, EntityKind, ForeignOrigin, FragmentView, LanguageExtensionWireFact,
    OccurrenceTarget, PythonFacts, SECTION_NONE,
};
use compiler_vocabulary::{LanguageProfile, NativeTool, PythonVersion, Stage};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const CAP: usize = 8 * 1024 * 1024;

/// The exact typed beyond-geometry terminal one primary may pin: the closed
/// compile-failure label, the exact rejected fact, and the module that hit it.
/// The rejected name's byte length stays at the terminal's evidence line; the
/// pinned law is the lane bound and its fault.
struct ExpectedTerminal {
    label: &'static str,
    fact: usize,
    cause: FactFault,
    module_suffix: &'static str,
}

/// The emission fact lane bound the beyond-geometry primaries exceed.
const LANE_FULL_FACT: usize = 1024;

/// The exact terminal pyparsing 3.2.3's core.py hits: the emission fact lane
/// is full, so fact 1024 (the lane bound) is the first rejected declaration.
const fn lane_full_terminal(module_suffix: &'static str) -> ExpectedTerminal {
    ExpectedTerminal {
        label: "fact-rejected",
        fact: LANE_FULL_FACT,
        cause: FactFault::Capacity,
        module_suffix,
    }
}

/// The exact terminal a primary whose one declaration carries more than the
/// per-fact child geometry hits: the child lane is full at that fact ordinal.
const fn child_lane_full_terminal(fact: usize, module_suffix: &'static str) -> ExpectedTerminal {
    ExpectedTerminal {
        label: "fact-rejected",
        fact,
        cause: FactFault::ChildCapacity,
        module_suffix,
    }
}

impl ExpectedTerminal {
    fn matches(&self, terminal: &Error) -> bool {
        let Error::Terminal {
            package: _,
            module,
            label,
            rejection,
        } = terminal
        else {
            return false;
        };
        *label == self.label
            && module.ends_with(self.module_suffix)
            && rejection
                .is_some_and(|rejected| rejected.fact == self.fact && rejected.cause == self.cause)
    }
}

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Support(#[from] python_support::Error),
    #[error("I/O: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("toolchain: {0}")]
    Toolchain(String),
    #[error("fragment validation: {0}")]
    Fragment(String),
    #[error("package {package}: {message}")]
    Fact { package: String, message: String },
    #[error("typed terminal in {package} at {module}: {label} rejected {rejection:?}")]
    Terminal {
        package: &'static str,
        module: String,
        label: &'static str,
        rejection: Option<FactRejection>,
    },
}

fn io(source: std::io::Error) -> Error {
    Error::Io { source }
}

/// The closed label of one compile terminal; every variant is named so a
/// future terminal cannot silently fall into a catch-all bucket.
fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::SourceLength { .. } => "source-length",
        CompileFailure::UnsupportedStage { .. } => "unsupported-stage",
        CompileFailure::ToolchainSelectionMismatch { .. } => "toolchain-selection-mismatch",
        CompileFailure::ToolchainMismatch { .. } => "toolchain-mismatch",
        CompileFailure::NativeWork { .. } => "native-work",
        CompileFailure::NativeWorkCleanup { .. } => "native-work-cleanup",
        CompileFailure::ToolingUnavailable { .. } => "tooling-unavailable",
        CompileFailure::ToolStart { .. } => "tool-start",
        CompileFailure::MissingToolInput { .. } => "missing-tool-input",
        CompileFailure::MissingToolInputCleanup { .. } => "missing-tool-input-cleanup",
        CompileFailure::MissingToolDiagnostic { .. } => "missing-tool-diagnostic",
        CompileFailure::MissingToolDiagnosticCleanup { .. } => "missing-tool-diagnostic-cleanup",
        CompileFailure::ToolInput { .. } => "tool-input",
        CompileFailure::ToolInputCleanup { .. } => "tool-input-cleanup",
        CompileFailure::ToolTerminate { .. } => "tool-terminate",
        CompileFailure::ToolWait { .. } => "tool-wait",
        CompileFailure::ToolWaitCleanup { .. } => "tool-wait-cleanup",
        CompileFailure::ToolDiagnosticRead { .. } => "tool-diagnostic-read",
        CompileFailure::ToolDiagnosticReadCleanup { .. } => "tool-diagnostic-read-cleanup",
        CompileFailure::NativeWorkerPanic { .. } => "native-worker-panic",
        CompileFailure::Cancelled { .. } => "cancelled",
        CompileFailure::DeadlineExceeded { .. } => "deadline-exceeded",
        CompileFailure::DiagnosticLimit { .. } => "diagnostic-limit",
        CompileFailure::NativeRejected { .. } => "native-rejected",
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::FactRejected { .. } => "fact-rejected",
        CompileFailure::CSharpProjection { .. } => "csharp-projection",
        CompileFailure::Build { .. } => "build",
        CompileFailure::Prepare { .. } => "prepare",
        CompileFailure::Write { .. } => "write",
        CompileFailure::Validate { .. } => "validate",
    }
}

fn python() -> Result<ResolvedToolchain<'static>, Error> {
    let path = std::env::var_os("COMPILER_PYTHON_COMPILER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path)
                        .map(|d| d.join("python3"))
                        .find(|p| p.is_file())
                })
                .flatten()
        })
        .ok_or_else(|| Error::Toolchain("python3 unavailable".into()))?;
    let path = Box::leak(path.canonicalize().map_err(io)?.into_boxed_path());
    let output = Command::new(&*path).arg("--version").output().map_err(io)?;
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::Python, path, version)
        .map_err(|cause| Error::Toolchain(cause.to_string()))
}

fn metadata_url(name: &str, version: &str) -> String {
    format!("https://pypi.org/pypi/{name}/{version}/json")
}

fn sdist(name: &str, version: &str) -> Result<Vec<u8>, Error> {
    let metadata = python_support::download(
        &metadata_url(name, version),
        CAP,
        Instant::now() + Duration::from_secs(60),
    )?;
    let text = std::str::from_utf8(&metadata).map_err(|_| Error::Fact {
        package: name.into(),
        message: "metadata was not UTF-8".into(),
    })?;
    let marker = "https://files.pythonhosted.org/packages/";
    let needle = format!("{name}-{version}");
    let url = text
        .split('"')
        .find(|part| {
            part.starts_with(marker)
                && part.ends_with(".tar.gz")
                && part
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
        })
        .ok_or_else(|| Error::Fact {
            package: name.into(),
            message: "sdist URL absent".into(),
        })?;
    let url_end = text.find(url).ok_or_else(|| Error::Fact {
        package: name.into(),
        message: "sdist URL disappeared from metadata".into(),
    })?;
    let digest_marker = text[..url_end]
        .rfind("\"sha256\"")
        .ok_or_else(|| Error::Fact {
            package: name.into(),
            message: "sdist sha256 absent".into(),
        })?;
    let after = &text[digest_marker + "\"sha256\"".len()..url_end];
    let quote = after.find('"').ok_or_else(|| Error::Fact {
        package: name.into(),
        message: "malformed sdist sha256".into(),
    })?;
    let expected = &after[quote + 1..quote + 65];
    let archive = python_support::download(url, CAP, Instant::now() + Duration::from_secs(60))?;
    let actual = python_support::sha256(&archive);
    let actual = actual
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        return Err(Error::Fact {
            package: name.into(),
            message: format!("sdist sha256 mismatch: expected {expected}, got {actual}"),
        });
    }
    Ok(archive)
}

fn files(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(path).map_err(io)? {
        let entry = entry.map_err(io)?;
        let child = entry.path();
        if child.is_dir() {
            files(&child, out)?;
        } else if child.extension().and_then(|x| x.to_str()) == Some("py") {
            out.push(child);
        }
    }
    Ok(())
}

fn compile_module(
    package: &'static str,
    module: &Path,
    source: &[u8],
    tool: ResolvedToolchain<'static>,
) -> Result<Vec<u8>, Error> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; CAP];
    let label = module
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("module");
    let work = python_support::fresh_dir(&format!("packages-{label}"))?;
    let result = compile(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source,
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    );
    let module_display = module.display().to_string();
    let outcome = match result {
        Ok(compiled) => Ok(compiled.fragment.as_ref().to_vec()),
        Err(failure) => {
            let label = failure_label(&failure);
            let rejection = match &failure {
                CompileFailure::FactRejected { rejected, .. } => Some(*rejected),
                _ => None,
            };
            Err(Error::Terminal {
                package,
                module: module_display,
                label,
                rejection,
            })
        }
    };
    let cleanup = fs::remove_dir_all(&work).map_err(io);
    match (outcome, cleanup) {
        (Ok(bytes), Ok(())) => Ok(bytes),
        (Ok(bytes), Err(cleanup)) => {
            eprintln!("python package: scratch cleanup failed after a clean compile: {cleanup}");
            Ok(bytes)
        }
        (Err(terminal), Err(cleanup)) => {
            eprintln!("python package: scratch cleanup also failed at the terminal: {cleanup}");
            Err(terminal)
        }
        (Err(terminal), Ok(())) => Err(terminal),
    }
}

fn name<'a>(atoms: &'a [&'a [u8]], ordinal: u32) -> &'a [u8] {
    usize::try_from(ordinal)
        .ok()
        .and_then(|n| atoms.get(n).copied())
        .unwrap_or(&[])
}

/// Locates and compiles exactly the pinned primary module: a typed terminal
/// or a hard error, never an alternative file.
fn select(package: &'static str, version: &str) -> Result<(PathBuf, Vec<u8>, Vec<u8>), Error> {
    let archive = sdist(package, version)?;
    let root = python_support::fresh_dir(package)?;
    python_support::unpack(&archive, &root)?;
    let mut candidates = Vec::new();
    files(&root, &mut candidates)?;
    candidates.sort();
    let (primary_name, container) = match package {
        "requests" => ("models.py", "requests"),
        "attrs" => ("_make.py", "attr"),
        "flask" => ("app.py", "flask"),
        "six" => ("six.py", ""),
        "wcwidth" => ("wcwidth.py", "wcwidth"),
        "idna" => ("core.py", "idna"),
        "certifi" => ("core.py", "certifi"),
        "packaging" => ("__init__.py", "packaging"),
        "pyparsing" => ("core.py", "pyparsing"),
        "iniconfig" => ("__init__.py", "iniconfig"),
        "pluggy" => ("_hooks.py", "pluggy"),
        "click" => ("core.py", "click"),
        "itsdangerous" => ("serializer.py", "itsdangerous"),
        "jinja2" => ("environment.py", "jinja2"),
        "markupsafe" => ("__init__.py", "markupsafe"),
        "werkzeug" => ("request.py", "wrappers"),
        "colorama" => ("ansitowin32.py", "colorama"),
        "PyYAML" => ("__init__.py", "yaml"),
        "tomli" => ("_parser.py", "tomli"),
        "webencodings" => ("__init__.py", "webencodings"),
        _ => ("__init__.py", package),
    };
    let primary = candidates
        .iter()
        .filter(|path| eligible(package, path))
        .find(|path| {
            path.file_name().and_then(|s| s.to_str()) == Some(primary_name)
                && (container.is_empty()
                    || path
                        .components()
                        .any(|component| component.as_os_str() == container))
        })
        .cloned()
        .ok_or_else(|| Error::Fact {
            package: package.into(),
            message: format!("no primary Python module {primary_name} under {container}/"),
        })?;
    let tool = python()?;
    let primary_source = fs::read(&primary).map_err(io)?;
    let bytes = compile_module(package, &primary, &primary_source, tool)?;
    Ok((primary, primary_source, bytes))
}

fn eligible(package: &str, path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.contains("/tests/")
        || text.contains("/docs/")
        || text.ends_with("/setup.py")
        || text.ends_with("/conf.py")
        || text.ends_with("/conftest.py")
    {
        return false;
    }
    match package {
        "requests" => text.contains("/src/requests/"),
        "attrs" => text.contains("/src/attr/"),
        "flask" => text.contains("/src/flask/"),
        "wcwidth" => text.contains("/wcwidth/"),
        "six" => text.contains("/six-1.17.0/") && !text.contains("/documentation/"),
        "PyYAML" => text.contains("/yaml/"),
        _ => {
            text.contains(&format!("/{package}/"))
                || (text.contains("/src/") && text.ends_with(".py"))
        }
    }
}

/// One pinned corpus row. Symbols, lane conditions, and decorator spellings
/// are pinned from the source truth of the exact sdist version; `expect`
/// pins the one typed beyond-geometry terminal the primary may hit.
struct PackageFacts {
    package: &'static str,
    version: &'static str,
    symbols: [(&'static str, EntityKind); 4],
    annotated: bool,
    docs: bool,
    foreign_pypi: bool,
    decorators: &'static [&'static [u8]],
    expect: Option<ExpectedTerminal>,
}

const ADDITIONS: [PackageFacts; 15] = [
    PackageFacts {
        package: "idna",
        version: "3.10",
        symbols: [
            ("IDNAError", EntityKind::Record),
            ("encode", EntityKind::Function),
            ("decode", EntityKind::Function),
            ("uts46_remap", EntityKind::Function),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[],
        expect: None,
    },
    PackageFacts {
        package: "certifi",
        version: "2025.7.14",
        symbols: [
            ("where", EntityKind::Function),
            ("contents", EntityKind::Function),
            ("exit_cacert_ctx", EntityKind::Function),
            ("_CACERT_PATH", EntityKind::Static),
        ],
        annotated: true,
        docs: false,
        foreign_pypi: true,
        decorators: &[],
        expect: None,
    },
    PackageFacts {
        package: "packaging",
        version: "25.0",
        symbols: [
            ("__title__", EntityKind::Static),
            ("__summary__", EntityKind::Static),
            ("__version__", EntityKind::Static),
            ("__license__", EntityKind::Static),
        ],
        annotated: false,
        docs: false,
        foreign_pypi: false,
        decorators: &[],
        expect: None,
    },
    PackageFacts {
        package: "pyparsing",
        version: "3.2.3",
        symbols: [
            ("__compat__", EntityKind::Record),
            ("__diag__", EntityKind::Record),
            ("enable_diag", EntityKind::Function),
            ("ParserElement", EntityKind::Record),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[b"staticmethod", b"property", b"classmethod"],
        expect: Some(lane_full_terminal("pyparsing/core.py")),
    },
    PackageFacts {
        package: "iniconfig",
        version: "2.1.0",
        symbols: [
            ("__all__", EntityKind::Static),
            ("SectionWrapper", EntityKind::Record),
            ("IniConfig", EntityKind::Record),
            ("lineof", EntityKind::Function),
        ],
        annotated: true,
        docs: false,
        foreign_pypi: true,
        decorators: &[b"overload"],
        expect: None,
    },
    PackageFacts {
        package: "pluggy",
        version: "1.6.0",
        symbols: [
            ("normalize_hookimpl_opts", EntityKind::Function),
            ("varnames", EntityKind::Function),
            ("HookCaller", EntityKind::Record),
            ("HookImpl", EntityKind::Record),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: false,
        decorators: &[b"final", b"property", b"overload"],
        expect: None,
    },
    PackageFacts {
        package: "click",
        version: "8.2.1",
        symbols: [
            ("Command", EntityKind::Record),
            ("Group", EntityKind::Record),
            ("Context", EntityKind::Record),
            ("Parameter", EntityKind::Record),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[
            b"property",
            b"t.overload",
            b"contextmanager",
            b"click.pass_context",
        ],
        expect: Some(child_lane_full_terminal(105, "click/core.py")),
    },
    PackageFacts {
        package: "itsdangerous",
        version: "2.2.0",
        symbols: [
            ("Serializer", EntityKind::Record),
            ("is_text_serializer", EntityKind::Function),
            ("dumps", EntityKind::Function),
            ("loads", EntityKind::Function),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[b"t.overload", b"property"],
        expect: None,
    },
    PackageFacts {
        package: "jinja2",
        version: "3.1.6",
        symbols: [
            ("Environment", EntityKind::Record),
            ("Template", EntityKind::Record),
            ("TemplateStream", EntityKind::Record),
            ("create_cache", EntityKind::Function),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[
            b"internalcode",
            b"property",
            b"classmethod",
            b"typing.overload",
        ],
        expect: Some(child_lane_full_terminal(107, "jinja2/environment.py")),
    },
    PackageFacts {
        package: "markupsafe",
        version: "3.0.2",
        symbols: [
            ("Markup", EntityKind::Record),
            ("escape", EntityKind::Function),
            ("soft_str", EntityKind::Function),
            ("escape_silent", EntityKind::Function),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[b"classmethod"],
        expect: None,
    },
    PackageFacts {
        package: "werkzeug",
        version: "3.1.3",
        symbols: [
            ("Request", EntityKind::Record),
            ("get_data", EntityKind::Function),
            ("_load_form_data", EntityKind::Function),
            ("application", EntityKind::Function),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[
            b"cached_property",
            b"t.overload",
            b"property",
            b"classmethod",
        ],
        expect: None,
    },
    PackageFacts {
        package: "colorama",
        version: "0.4.6",
        symbols: [
            ("AnsiToWin32", EntityKind::Record),
            ("StreamWrapper", EntityKind::Record),
            ("write", EntityKind::Function),
            ("isatty", EntityKind::Function),
        ],
        annotated: false,
        docs: false,
        foreign_pypi: true,
        decorators: &[b"property"],
        expect: None,
    },
    PackageFacts {
        package: "PyYAML",
        version: "6.0.2",
        symbols: [
            ("load", EntityKind::Function),
            ("dump", EntityKind::Function),
            ("safe_load", EntityKind::Function),
            ("scan", EntityKind::Function),
        ],
        annotated: false,
        docs: true,
        foreign_pypi: false,
        decorators: &[b"classmethod"],
        expect: None,
    },
    PackageFacts {
        package: "tomli",
        version: "2.2.1",
        symbols: [
            ("TOMLDecodeError", EntityKind::Record),
            ("load", EntityKind::Function),
            ("loads", EntityKind::Function),
            ("Flags", EntityKind::Record),
        ],
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[],
        expect: None,
    },
    PackageFacts {
        package: "webencodings",
        version: "0.5.1",
        symbols: [
            ("Encoding", EntityKind::Record),
            ("decode", EntityKind::Function),
            ("lookup", EntityKind::Function),
            ("ascii_lower", EntityKind::Function),
        ],
        annotated: false,
        docs: true,
        foreign_pypi: false,
        decorators: &[],
        expect: None,
    },
];

/// The five primary packages carry bespoke per-package laws beside the shared
/// full-fidelity lane assertions. The version stays in the test invocations.
struct PrimaryFacts {
    package: &'static str,
    annotated: bool,
    docs: bool,
    foreign_pypi: bool,
    decorators: &'static [&'static [u8]],
}

const PRIMARIES: [PrimaryFacts; 5] = [
    PrimaryFacts {
        package: "requests",
        annotated: false,
        docs: true,
        foreign_pypi: true,
        decorators: &[b"property", b"staticmethod"],
    },
    PrimaryFacts {
        package: "attrs",
        annotated: true,
        docs: true,
        foreign_pypi: false,
        decorators: &[b"staticmethod", b"classmethod"],
    },
    PrimaryFacts {
        package: "flask",
        annotated: true,
        docs: true,
        foreign_pypi: true,
        decorators: &[],
    },
    PrimaryFacts {
        package: "six",
        annotated: false,
        docs: true,
        foreign_pypi: true,
        decorators: &[b"classmethod"],
    },
    PrimaryFacts {
        package: "wcwidth",
        annotated: false,
        docs: true,
        foreign_pypi: true,
        decorators: &[],
    },
];

/// One little-endian u32 word of a validated section payload.
fn word(payload: &[u8], at: usize) -> Option<u32> {
    payload
        .get(at..at.checked_add(4)?)?
        .first_chunk::<4>()
        .map(|bytes| u32::from_le_bytes(*bytes))
}

/// Decodes the raw Python extension row of one entity ordinal: a 16-byte
/// section header, seven 20-byte directory entries (Python is the fifth),
/// then the row table and the fixed-width fact pool.
fn python_extension_row(
    package: &str,
    payload: &[u8],
    ordinal: usize,
) -> Result<Option<PythonFacts>, Error> {
    const PYTHON_DIRECTORY: usize = 16 + 4 * 20;
    let fact = |message: &'static str| Error::Fact {
        package: package.into(),
        message: message.into(),
    };
    let rows =
        usize::try_from(word(payload, PYTHON_DIRECTORY + 4).ok_or_else(|| fact("extension rows"))?)
            .map_err(|_| fact("extension rows"))?;
    let pool = word(payload, PYTHON_DIRECTORY + 8).ok_or_else(|| fact("extension pool"))?;
    let offset = usize::try_from(
        word(payload, PYTHON_DIRECTORY + 12).ok_or_else(|| fact("extension offset"))?,
    )
    .map_err(|_| fact("extension offset"))?;
    if pool == 0 {
        return Err(fact("empty python extension plane"));
    }
    let row_ordinal = word(payload, offset + ordinal * 4).ok_or_else(|| fact("extension row"))?;
    if row_ordinal == SECTION_NONE {
        return Ok(None);
    }
    let at = offset
        .checked_add(rows * 4)
        .and_then(|base| {
            usize::try_from(row_ordinal)
                .ok()
                .and_then(|ordinal| ordinal.checked_mul(PythonFacts::WIDTH))
                .and_then(|width| base.checked_add(width))
        })
        .ok_or_else(|| fact("extension fact out of range"))?;
    PythonFacts::decode(payload, at)
        .map(Some)
        .ok_or_else(|| fact("python extension fact decode"))
}

/// Reads one pooled atom list from the extension-pool payload: the
/// type-parameter segment first, then the three ref-list lanes.
fn pooled_atom_list(package: &str, pool: &[u8], index: u32) -> Result<Vec<u32>, Error> {
    let fact = |message: &'static str| Error::Fact {
        package: package.into(),
        message: message.into(),
    };
    let mut cursor = 4usize;
    let parameter_count = word(pool, 0).ok_or_else(|| fact("pool parameter count"))?;
    for _ in 0..parameter_count {
        let name_len = word(pool, cursor + 1).ok_or_else(|| fact("pool parameter name"))?;
        cursor = cursor
            .checked_add(5)
            .and_then(|at| {
                usize::try_from(name_len)
                    .ok()
                    .and_then(|width| at.checked_add(width))
            })
            .ok_or_else(|| fact("pool parameter name"))?;
        for _ in 0..2 {
            let present = pool.get(cursor).copied().ok_or_else(|| fact("pool cell"))?;
            cursor = cursor.checked_add(1).ok_or_else(|| fact("pool cell"))?;
            if present != 0 {
                word(pool, cursor).ok_or_else(|| fact("pool reference"))?;
                cursor = cursor
                    .checked_add(4)
                    .ok_or_else(|| fact("pool reference"))?;
            }
        }
    }
    let list_count = word(pool, cursor).ok_or_else(|| fact("pool list count"))?;
    cursor = cursor
        .checked_add(4)
        .ok_or_else(|| fact("pool list count"))?;
    for list in 0..list_count {
        let length = word(pool, cursor).ok_or_else(|| fact("pool list length"))?;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| fact("pool list length"))?;
        let length = usize::try_from(length).map_err(|_| fact("pool list length"))?;
        if list == index {
            let mut elements = Vec::new();
            for position in 0..length {
                elements
                    .push(word(pool, cursor + position * 4).ok_or_else(|| fact("pool element"))?);
            }
            return Ok(elements);
        }
        cursor = cursor
            .checked_add(
                length
                    .checked_mul(4)
                    .ok_or_else(|| fact("pool list length"))?,
            )
            .ok_or_else(|| fact("pool list length"))?;
    }
    Err(fact("atom list absent"))
}

fn source_declares(source_text: &str, symbol: &str) -> bool {
    source_text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(&format!("class {symbol}"))
            || line.starts_with(&format!("def {symbol}"))
            || line.starts_with(&format!("async def {symbol}"))
            || line.starts_with(&format!("{symbol} ="))
            || line.starts_with(&format!("{symbol}:"))
    })
}

/// Decodes every lane the fragment committed, without swallowing a single
/// row error, and asserts the source-truth conditions the card pins:
/// type rows exist for annotated primaries, occurrences exist for modules
/// with imports, import bindings resolve as `pypi` foreign keys where the
/// package's imports are absolute, member-docstring-bearing modules commit
/// a decodable docs lane, and decorator spellings reach the pooled
/// extension atoms exactly as written minus `@`.
fn assert_lanes(
    package: &str,
    source_text: &str,
    view: &FragmentView<'_>,
    annotated: bool,
    docs: bool,
    foreign_pypi: bool,
    decorators: &'static [&'static [u8]],
) -> Result<usize, Error> {
    let fact = |message: &str| Error::Fact {
        package: package.into(),
        message: message.into(),
    };
    let mut assertions = 0;
    let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(&[u8], EntityKind)> = view
        .entities()
        .map(|row| (name(&atoms, row.name.raw), row.kind))
        .collect();

    let mut type_rows = 0_usize;
    if let Some(mut cursor) = view.type_facts() {
        for row in cursor.by_ref() {
            row.map_err(|fault| fact(&format!("type fact row failed to decode: {fault}")))?;
            type_rows += 1;
        }
    }
    if annotated && type_rows == 0 {
        return Err(fact("annotated declarations decoded an empty type lane"));
    }
    assertions += 1;

    let mut occurrences: Vec<DecodedOccurrence<'_>> = Vec::new();
    if let Some(mut cursor) = view.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(
                row.map_err(|fault| fact(&format!("occurrence row failed to decode: {fault}")))?,
            );
        }
    }
    let imports_present = source_text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("import ") || line.starts_with("from ")
    });
    if imports_present && occurrences.is_empty() {
        return Err(fact("import facts absent from the occurrence lane"));
    }
    if foreign_pypi {
        let pypi_foreign = occurrences.iter().any(|row| {
            matches!(
                &row.occurrence.target,
                OccurrenceTarget::Foreign(key)
                    if matches!(&key.origin, ForeignOrigin::Package(lineage) if lineage.ecosystem == "pypi")
            )
        });
        if !pypi_foreign {
            return Err(fact("no import binding decoded to a pypi foreign key"));
        }
    }
    assertions += 1;

    let mut docs_decoded = false;
    if let Some(mut cursor) = view.docs() {
        for row in cursor.by_ref() {
            row.map_err(|fault| fact(&format!("docs row failed to decode: {fault}")))?;
            docs_decoded = true;
        }
    }
    if docs && !docs_decoded {
        return Err(fact("member docstring facts absent from the docs lane"));
    }
    assertions += 1;

    if !decorators.is_empty() {
        let payload = view
            .language_extension_payload()
            .ok_or_else(|| fact("extension section absent"))?;
        let pool = view
            .extension_pool_payload()
            .ok_or_else(|| fact("extension pool absent"))?;
        let mut spelled: Vec<&[u8]> = Vec::new();
        for ordinal in 0..entities.len() {
            if let Some(facts) = python_extension_row(package, payload, ordinal)? {
                for element in pooled_atom_list(package, pool, facts.decorators.raw)? {
                    let atom = usize::try_from(element)
                        .ok()
                        .and_then(|at| atoms.get(at).copied())
                        .ok_or_else(|| fact("decorator atom out of range"))?;
                    spelled.push(atom);
                }
            }
        }
        for wanted in decorators {
            if !spelled.iter().any(|spelling| *spelling == *wanted) {
                return Err(fact(&format!(
                    "decorator spelling {} absent from the extension atoms",
                    String::from_utf8_lossy(wanted)
                )));
            }
        }
        assertions += 1;
    }
    Ok(assertions)
}

fn assert_addition(spec: &PackageFacts) -> Result<usize, Error> {
    let (path, source, bytes) = select(spec.package, spec.version)?;
    let view =
        FragmentView::validate(&bytes).map_err(|cause| Error::Fragment(cause.to_string()))?;
    let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(&[u8], EntityKind)> = view
        .entities()
        .map(|row| (name(&atoms, row.name.raw), row.kind))
        .collect();
    let source_text = std::str::from_utf8(&source).map_err(|_| Error::Fact {
        package: spec.package.into(),
        message: "module source was not UTF-8".into(),
    })?;
    eprintln!(
        "python package selected: {} entities={}",
        path.display(),
        entities.len()
    );
    let mut assertions = 0;
    for (symbol, kind) in spec.symbols {
        if !source_declares(source_text, symbol)
            || !entities.iter().any(|(known, known_kind)| {
                String::from_utf8_lossy(known) == *symbol && *known_kind == kind
            })
        {
            return Err(Error::Fact {
                package: spec.package.into(),
                message: format!("pinned symbol {symbol}/{kind:?} absent from source or entities"),
            });
        }
        assertions += 1;
    }
    assertions += assert_lanes(
        spec.package,
        source_text,
        &view,
        spec.annotated,
        spec.docs,
        spec.foreign_pypi,
        spec.decorators,
    )?;
    Ok(assertions)
}

#[test]
fn fifteen_additional_real_sdists_preserve_source_facts() -> Result<(), Error> {
    for spec in ADDITIONS {
        match assert_addition(&spec) {
            Ok(count) => eprintln!(
                "python package facts: {}@{} assertions={count}",
                spec.package, spec.version
            ),
            Err(Error::Support(python_support::Error::Network { .. })) => {
                eprintln!(
                    "python package typed skip: {} network unavailable",
                    spec.package
                );
            }
            Err(Error::Toolchain(message)) if message == "python3 unavailable" => {
                eprintln!("python package typed skip: python3 unavailable");
                break;
            }
            Err(error) if spec.expect.as_ref().is_some_and(|e| e.matches(&error)) => {
                let Error::Terminal {
                    module, rejection, ..
                } = &error
                else {
                    continue;
                };
                eprintln!(
                    "python package typed terminal: {}@{} at {module} pinned {rejection:?}",
                    spec.package, spec.version
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn assert_package(package: &'static str, version: &'static str) -> Result<usize, Error> {
    let facts = PRIMARIES
        .iter()
        .find(|facts| facts.package == package)
        .ok_or_else(|| Error::Fact {
            package: package.into(),
            message: "no primary spec".into(),
        })?;
    let (path, source, bytes) = select(package, version)?;
    let view =
        FragmentView::validate(&bytes).map_err(|cause| Error::Fragment(cause.to_string()))?;
    let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(&[u8], EntityKind)> = view
        .entities()
        .map(|row| (name(&atoms, row.name.raw), row.kind))
        .collect();
    eprintln!(
        "python package selected: {} entities={}",
        path.display(),
        entities.len()
    );
    let source_text = std::str::from_utf8(&source).map_err(|_| Error::Fact {
        package: package.into(),
        message: "module source was not UTF-8".into(),
    })?;
    let mut assertions = 1;
    let has = |wanted: &[u8], kind: EntityKind| {
        entities
            .iter()
            .any(|(known, actual)| *known == wanted && *actual == kind)
    };
    let dual = |wanted: &[u8], kind: EntityKind| {
        has(wanted, kind) && source_declares(source_text, std::str::from_utf8(wanted).unwrap_or(""))
    };
    match package {
        "requests" => {
            for (wanted, kind) in [
                (b"Request".as_slice(), EntityKind::Record),
                (b"PreparedRequest", EntityKind::Record),
                (b"Response", EntityKind::Record),
            ] {
                if !dual(wanted, kind) {
                    return Err(Error::Fact {
                        package: package.into(),
                        message: format!(
                            "{} missing {}/{kind:?}",
                            path.display(),
                            String::from_utf8_lossy(wanted)
                        ),
                    });
                }
                assertions += 1;
            }
            if !source.windows(5).any(|w| w == b"hooks")
                || !source.windows(7).any(|w| w == b"urllib3")
            {
                return Err(Error::Fact {
                    package: package.into(),
                    message: "hooks/urllib3 import surface absent".into(),
                });
            }
            assertions += 1;
        }
        "attrs" => {
            if !(source.windows(6).any(|w| w == b"define")
                || source.windows(6).any(|w| w == b"attr.s"))
            {
                return Err(Error::Fact {
                    package: package.into(),
                    message: "decorator source absent".into(),
                });
            }
            assertions += 1;
            for (wanted, kind) in [
                (b"attrib".as_slice(), EntityKind::Function),
                (b"_ClassBuilder", EntityKind::Record),
                (b"evolve", EntityKind::Function),
            ] {
                if !dual(wanted, kind) {
                    return Err(Error::Fact {
                        package: package.into(),
                        message: format!(
                            "{} missing {}/{kind:?}",
                            path.display(),
                            String::from_utf8_lossy(wanted)
                        ),
                    });
                }
                assertions += 1;
            }
        }
        "flask" => {
            for wanted in [b"AppContext".as_slice(), b"RequestContext"] {
                if !has(wanted, EntityKind::Record)
                    && !source.windows(wanted.len()).any(|window| window == wanted)
                {
                    return Err(Error::Fact {
                        package: package.into(),
                        message: format!("{} missing {:?}", path.display(), wanted),
                    });
                }
                assertions += 1;
            }
            if !dual(b"Flask", EntityKind::Record) {
                return Err(Error::Fact {
                    package: package.into(),
                    message: format!("{} missing Flask/Record", path.display()),
                });
            }
            assertions += 1;
        }
        "six" => {
            for (wanted, kind) in [
                (b"PY2".as_slice(), EntityKind::Static),
                (b"PY3", EntityKind::Static),
                (b"add_metaclass", EntityKind::Function),
                (b"with_metaclass", EntityKind::Function),
                (b"b", EntityKind::Function),
                (b"u", EntityKind::Function),
            ] {
                if !dual(wanted, kind) {
                    return Err(Error::Fact {
                        package: package.into(),
                        message: format!(
                            "{} missing {}/{kind:?}",
                            path.display(),
                            String::from_utf8_lossy(wanted)
                        ),
                    });
                }
                assertions += 1;
            }
        }
        "wcwidth" => {
            if !dual(b"wcwidth", EntityKind::Function) {
                return Err(Error::Fact {
                    package: package.into(),
                    message: format!("{} missing wcwidth/Function", path.display()),
                });
            }
            assertions += 1;
        }
        _ => {}
    }
    assertions += assert_lanes(
        package,
        source_text,
        &view,
        facts.annotated,
        facts.docs,
        facts.foreign_pypi,
        facts.decorators,
    )?;
    Ok(assertions)
}

#[test]
fn requests_real_sdist_decodes_index_and_import_lanes() -> Result<(), Error> {
    assert_package("requests", "2.32.3").map(|_| ())
}
#[test]
fn attrs_real_sdist_decodes_decorator_and_annotation_lanes() -> Result<(), Error> {
    match assert_package("attrs", "25.3.0") {
        // attrs 25.3.0's _make.py stops at the exact typed terminal: the
        // `_ClassBuilder` fact at ordinal 258 carries more members than the
        // per-fact child geometry admits, so admission rejects it with
        // ChildCapacity. Every earlier fact, lane, and symbol still decoded.
        Err(error) if child_lane_full_terminal(258, "src/attr/_make.py").matches(&error) => Ok(()),
        other => other.map(|_| ()),
    }
}
#[test]
fn flask_real_sdist_decodes_application_declarations() -> Result<(), Error> {
    assert_package("flask", "3.1.1").map(|_| ())
}
#[test]
fn six_real_sdist_decodes_compatibility_declarations() -> Result<(), Error> {
    assert_package("six", "1.17.0").map(|_| ())
}
#[test]
fn wcwidth_real_sdist_decodes_tables_and_signature() -> Result<(), Error> {
    assert_package("wcwidth", "0.2.13").map(|_| ())
}

#[test]
fn oracle_assertions_are_conditional_on_pyrefly() -> Result<(), Error> {
    if !compiler_languages_python::Pyrefly::from_env().is_available() {
        return Ok(());
    }
    assert_package("six", "1.17.0").map(|_| ())
}
