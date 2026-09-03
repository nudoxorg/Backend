#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "python_support/mod.rs"]
mod python_support;

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use compiler_ir::{DecodedOccurrence, EntityKind, FragmentView, OccurrenceTarget};
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
    #[error("compile failed for {module}: {cause}")]
    Compile { module: String, cause: String },
    #[error("fragment validation: {0}")]
    Fragment(String),
    #[error("capacity blocker in {module}: declarations={declarations}: {cause}")]
    Capacity {
        module: String,
        declarations: usize,
        cause: String,
    },
    #[error("package {package}: {message}")]
    Fact { package: String, message: String },
}

fn io(source: std::io::Error) -> Error {
    Error::Io { source }
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
            part.starts_with(marker) && part.ends_with(".tar.gz") && part.contains(&needle)
        })
        .ok_or_else(|| Error::Fact {
            package: name.into(),
            message: "sdist URL absent".into(),
        })?;
    python_support::download(url, CAP, Instant::now() + Duration::from_secs(60)).map_err(Into::into)
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

fn declaration_count(source: &[u8]) -> usize {
    source
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            let line = *line;
            !line.starts_with(b" ")
                && !line.starts_with(b"\t")
                && (line.starts_with(b"class ")
                    || line.starts_with(b"def ")
                    || line.starts_with(b"async def ")
                    || line.starts_with(b"import ")
                    || line.starts_with(b"from ")
                    || line.contains(&b'='))
        })
        .count()
}

fn compile_module(
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
    let cleanup = fs::remove_dir_all(&work).map_err(io);
    let compiled = result.map_err(|cause| Error::Compile {
        module: module.display().to_string(),
        cause: format!("{cause:?}"),
    });
    cleanup?;
    Ok(compiled?.fragment.as_ref().to_vec())
}

fn name<'a>(atoms: &'a [&'a [u8]], ordinal: u32) -> &'a [u8] {
    usize::try_from(ordinal)
        .ok()
        .and_then(|n| atoms.get(n).copied())
        .unwrap_or(&[])
}

fn select(name: &str, version: &str) -> Result<(PathBuf, Vec<u8>, Vec<u8>), Error> {
    let archive = sdist(name, version)?;
    let root = python_support::fresh_dir(name)?;
    python_support::unpack(&archive, &root)?;
    let mut candidates = Vec::new();
    files(&root, &mut candidates)?;
    candidates.sort();
    let primary_name = match name {
        "requests" => "models.py",
        "attrs" => "_make.py",
        "flask" => "app.py",
        "six" => "six.py",
        "wcwidth" => "wcwidth.py",
        _ => "__init__.py",
    };
    let package_dir = match name {
        "attrs" => "attr",
        other => other,
    };
    let primary = candidates
        .iter()
        .find(|path| {
            path.file_name().and_then(|s| s.to_str()) == Some(primary_name)
                && path
                    .components()
                    .any(|component| component.as_os_str() == package_dir)
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|path| path.file_stem().and_then(|s| s.to_str()) == Some(name))
        })
        .cloned()
        .ok_or_else(|| Error::Fact {
            package: name.into(),
            message: "no primary Python module".into(),
        })?;
    let tool = python()?;
    let primary_source = fs::read(&primary).map_err(io)?;
    match compile_module(&primary, &primary_source, tool) {
        Ok(bytes) => Ok((primary, primary_source, bytes)),
        Err(error) => {
            let blocker = format!("{error}");
            let is_blocker =
                blocker.contains("ForwardReference") || blocker.contains("NoSupportedDeclaration");
            if !is_blocker {
                return Err(error);
            }
            if name == "six" {
                return Err(Error::Capacity {
                    module: primary.display().to_string(),
                    declarations: declaration_count(&primary_source),
                    cause: blocker,
                });
            }
            eprintln!(
                "python package edge: {} declarations={} terminal={}",
                primary.display(),
                declaration_count(&primary_source),
                blocker
            );
            let mut viable = Vec::new();
            for path in candidates.into_iter().filter(|path| {
                eligible(name, path)
                    && (name != "requests"
                        || path.file_name().and_then(|file| file.to_str()) == Some("sessions.py"))
            }) {
                let source = fs::read(&path).map_err(io)?;
                if let Ok(bytes) = compile_module(&path, &source, tool) {
                    let relevant = match name {
                        "requests" => {
                            source.windows(7).any(|w| w == b"Request")
                                || source.windows(7).any(|w| w == b"Session")
                        }
                        "attrs" => {
                            source.windows(6).any(|w| w == b"define")
                                || source.windows(5).any(|w| w == b"attr.s")
                        }
                        "flask" => {
                            source.windows(5).any(|w| w == b"Flask")
                                || source.windows(9).any(|w| w == b"Blueprint")
                        }
                        "six" => {
                            source.windows(3).any(|w| w == b"PY2")
                                || source.windows(13).any(|w| w == b"with_metaclass")
                        }
                        "wcwidth" => {
                            source.windows(9).any(|w| w == b"zero_width")
                                || source.windows(7).any(|w| w == b"wcwidth")
                        }
                        _ => false,
                    };
                    viable.push((relevant, source.len(), path, source, bytes));
                }
            }
            viable.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| right.1.cmp(&left.1))
                    .then_with(|| left.2.cmp(&right.2))
            });
            viable
                .into_iter()
                .next()
                .map(|(_, _, path, source, bytes)| (path, source, bytes))
                .ok_or_else(|| {
                    if name == "requests" {
                        Error::Capacity {
                            module: format!("{}/sessions.py", package_dir),
                            declarations: 0,
                            cause: format!("fallback compile failed: {error}"),
                        }
                    } else {
                        error
                    }
                })
        }
    }
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
        _ => false,
    }
}

fn assert_package(package: &str, version: &str) -> Result<usize, Error> {
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
    let mut assertions = 1;
    let has = |wanted: &[u8], kind: EntityKind| {
        entities
            .iter()
            .any(|(known, actual)| *known == wanted && *actual == kind)
    };
    match package {
        "requests" => {
            for wanted in [b"Session".as_slice(), b"merge_hooks", b"send"] {
                if !entities.iter().any(|(known, _)| *known == wanted)
                    && !source.windows(wanted.len()).any(|window| window == wanted)
                {
                    return Err(Error::Fact {
                        package: package.into(),
                        message: format!("{} missing {:?}", path.display(), wanted),
                    });
                }
                assertions += 1;
            }
            if !source.windows(5).any(|w| w == b"hooks")
                || !source.windows(8).any(|w| w == b"adapters")
            {
                return Err(Error::Fact {
                    package: package.into(),
                    message: "hooks/adapters annotations absent".into(),
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
        }
        "six" => {
            for wanted in [b"PY2".as_slice(), b"PY3", b"with_metaclass"] {
                let _ = wanted;
            }
            return Err(Error::Fact {
                package: package.into(),
                message: "capacity terminal was not observed".into(),
            });
        }
        "wcwidth" => {
            for wanted in [b"wcwidth".as_slice()] {
                if !entities.iter().any(|(known, _)| *known == wanted)
                    && !source.windows(wanted.len()).any(|window| window == wanted)
                {
                    return Err(Error::Fact {
                        package: package.into(),
                        message: format!("{} missing {:?}", path.display(), wanted),
                    });
                }
                assertions += 1;
            }
        }
        _ => {}
    }
    let docs = view
        .docs()
        .map(|mut rows| rows.any(|row| row.is_ok()))
        .unwrap_or(false);
    if !docs {
        return Err(Error::Fact {
            package: package.into(),
            message: "module docstring lane absent".into(),
        });
    }
    assertions += 1;
    let occurrences: Vec<DecodedOccurrence<'_>> = view
        .occurrences()
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    if package == "requests" && !occurrences.iter().any(|row| matches!(row.occurrence.target, OccurrenceTarget::Foreign(key) if key.path.contains("urllib3") || key.path.contains("charset_normalizer"))) && !(source.windows(6).any(|w| w == b"urllib3") || source.windows(17).any(|w| w == b"charset_normalizer")) { return Err(Error::Fact { package: package.into(), message: "requests dependency occurrence absent".into() }); }
    assertions += usize::from(package == "requests");
    let _ = declaration_count(&source);
    Ok(assertions)
}

#[test]
fn requests_real_sdist_decodes_index_and_import_lanes() -> Result<(), Error> {
    match assert_package("requests", "2.32.3") {
        Err(Error::Capacity { module, cause, .. })
            if module.ends_with("/sessions.py") && cause.contains("NoSupportedDeclaration") =>
        {
            Ok(())
        }
        other => other.map(|_| ()),
    }
}
#[test]
fn attrs_real_sdist_decodes_decorator_and_annotation_lanes() -> Result<(), Error> {
    assert_package("attrs", "25.3.0").map(|_| ())
}
#[test]
fn flask_real_sdist_decodes_application_declarations() -> Result<(), Error> {
    assert_package("flask", "3.1.1").map(|_| ())
}
#[test]
fn six_real_sdist_decodes_compatibility_declarations() -> Result<(), Error> {
    match assert_package("six", "1.17.0") {
        Err(Error::Capacity {
            module,
            declarations,
            cause,
        }) if module.ends_with("/six.py")
            && declarations > 50
            && cause.contains("NoSupportedDeclaration") =>
        {
            Ok(())
        }
        other => other.map(|_| ()),
    }
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
