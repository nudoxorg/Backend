//! Drives a checkout's native build description into one clang compilation database.
//!
//! The marker is only adapter selection: flags are copied from the build output without
//! interpretation.  `make -n` is split on shell quoting solely to transport its already
//! printed argv.  The command stream is product data and is bounded separately from the 64 KiB
//! retained stderr diagnostics. Non-compile lines (`mkdir`, `ar`, `echo`, and link commands) are
//! adapter selection, not translation units. Build and generated database
//! paths are always below the caller's scratch directory.  The existing native child module is
//! intentionally not reused: it couples stdin-fed frontend lowering to a compile request,
//! whereas these children need ordinary stdout capture and exit-status observation.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicBool, Ordering},
};

use thiserror::Error;

const CAPTURE_BYTES: usize = 64 * 1024;
/// Generous product bound for `make -n` stdout (the command stream).
const COMMAND_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSystem {
    CMake,
    Meson,
    Make,
    Buck,
}

impl BuildSystem {
    fn tool(self) -> &'static str {
        match self {
            Self::CMake => "cmake",
            Self::Meson => "meson",
            Self::Make => "make",
            Self::Buck => "buck2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, Error)]
pub enum BuildDriveFailure {
    #[error("build tool is absent: {tool}")]
    ToolAbsent { tool: &'static str },
    #[error("build tool failed: {tool}")]
    DriveFailed {
        tool: &'static str,
        captured: CapturedOutput,
    },
    #[error("build tool produced no translation units: {tool}")]
    NoTranslationUnits { tool: &'static str },
    #[error("build command needs {required} arguments, capacity is {capacity}")]
    CommandCapacity { required: usize, capacity: usize },
    #[error(
        "build tool command stream is too large: {tool} produced {bytes} bytes (limit {limit})"
    )]
    CommandStreamTooLarge {
        tool: &'static str,
        bytes: usize,
        limit: usize,
    },
    #[error("compile command has an unrecognized compiler: {line}")]
    UnrecognizedCompileCommand { line: String },
    #[error("build tool is present but its drive protocol is unsupported: {tool}")]
    ToolPresentUndrivable {
        tool: &'static str,
        evidence: String,
    },
    #[error("no build system marker under {root}")]
    NoBuildSystemDetected { root: PathBuf },
    #[error("build drive was cancelled")]
    Cancelled,
    #[error("build path operation failed")]
    Io(#[source] std::io::Error),
    #[error("generated compilation database was rejected")]
    Database(#[source] compiler_languages_clang::DatabaseError),
    #[error("compilation database JSON was rejected")]
    CompilationDatabase(#[source] CompdbError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrivenTranslationUnit {
    pub source: PathBuf,
    pub arguments: Vec<String>,
    pub directory: PathBuf,
}

#[derive(Debug)]
pub struct DrivenCompilation {
    pub build_system: BuildSystem,
    pub database_directory: PathBuf,
    pub translation_units: Vec<DrivenTranslationUnit>,
}

pub fn discover_and_drive(
    root: &Path,
    scratch: &Path,
    cancelled: &AtomicBool,
) -> Result<DrivenCompilation, BuildDriveFailure> {
    let system = detect(root)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(BuildDriveFailure::Cancelled);
    }
    let build = scratch.join("build-drive");
    fs::create_dir_all(&build).map_err(BuildDriveFailure::Io)?;
    let output = match system {
        BuildSystem::Make => run_make(root, cancelled)?,
        BuildSystem::CMake => run_one(system, root, &build, cancelled)?,
        BuildSystem::Meson => {
            let setup = run_one(system, root, &build, cancelled)?;
            if !setup.status.success() {
                return Err(BuildDriveFailure::DriveFailed {
                    tool: system.tool(),
                    captured: capture(&setup.stderr),
                });
            }
            run_ninja(&build, cancelled)?
        }
        BuildSystem::Buck => {
            if !available(system.tool()) {
                return Err(BuildDriveFailure::ToolAbsent {
                    tool: system.tool(),
                });
            }
            let query = Command::new(system.tool())
                .current_dir(root)
                .args(["uquery", "kind(\"compilation_database\", //...)"])
                .output()
                .map_err(BuildDriveFailure::Io)?;
            let evidence = String::from_utf8_lossy(&query.stdout).into_owned();
            return Err(BuildDriveFailure::ToolPresentUndrivable {
                tool: system.tool(),
                evidence,
            });
        }
    };
    if !output.status.success() {
        return Err(BuildDriveFailure::DriveFailed {
            tool: system.tool(),
            captured: capture(&output.stderr),
        });
    }
    let database_directory = build.clone();
    if output.stdout.len() > COMMAND_STREAM_BYTES {
        return Err(BuildDriveFailure::CommandStreamTooLarge {
            tool: system.tool(),
            bytes: output.stdout.len(),
            limit: COMMAND_STREAM_BYTES,
        });
    }
    if system == BuildSystem::Make {
        let commands = parse_make(&output.stdout, root)?;
        if commands.is_empty() {
            return Err(BuildDriveFailure::NoTranslationUnits {
                tool: system.tool(),
            });
        }
        return Ok(DrivenCompilation {
            build_system: system,
            database_directory,
            translation_units: commands,
        });
    }
    let database_bytes = if system == BuildSystem::CMake {
        fs::read(build.join("compile_commands.json")).map_err(BuildDriveFailure::Io)?
    } else {
        output.stdout
    };
    let translation_units =
        read_compdb(&database_bytes).map_err(BuildDriveFailure::CompilationDatabase)?;
    if translation_units.is_empty() {
        return Err(BuildDriveFailure::NoTranslationUnits {
            tool: system.tool(),
        });
    }
    Ok(DrivenCompilation {
        build_system: system,
        database_directory,
        translation_units,
    })
}

fn detect(root: &Path) -> Result<BuildSystem, BuildDriveFailure> {
    for (name, system) in [
        ("CMakeLists.txt", BuildSystem::CMake),
        ("meson.build", BuildSystem::Meson),
        ("Makefile", BuildSystem::Make),
        ("makefile", BuildSystem::Make),
        ("GNUmakefile", BuildSystem::Make),
        ("BUCK", BuildSystem::Buck),
        ("BUCK2", BuildSystem::Buck),
    ] {
        if root.join(name).is_file() {
            return Ok(system);
        }
    }
    Err(BuildDriveFailure::NoBuildSystemDetected {
        root: root.to_path_buf(),
    })
}

fn available(tool: &str) -> bool {
    child_path()
        .map(|path| env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
        .unwrap_or(false)
}

fn child_path() -> Option<std::ffi::OsString> {
    let current = env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/Users/mileswirht/.local/bin"),
    ];
    paths.extend(env::split_paths(&current));
    env::join_paths(paths).ok()
}

fn run_one(
    system: BuildSystem,
    root: &Path,
    build: &Path,
    cancelled: &AtomicBool,
) -> Result<Output, BuildDriveFailure> {
    if !available(system.tool()) {
        return Err(BuildDriveFailure::ToolAbsent {
            tool: system.tool(),
        });
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(BuildDriveFailure::Cancelled);
    }
    let mut command = Command::new(system.tool());
    if let Some(path) = child_path() {
        command.env("PATH", path);
    }
    if system == BuildSystem::CMake {
        command
            .arg("-S")
            .arg(root)
            .arg("-B")
            .arg(build)
            .arg("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON");
    } else {
        command.current_dir(root).arg("setup").arg(build);
    }
    command
        .output()
        .map_err(|cause| BuildDriveFailure::DriveFailed {
            tool: system.tool(),
            captured: capture(cause.to_string().as_bytes()),
        })
}

fn run_ninja(build: &Path, cancelled: &AtomicBool) -> Result<Output, BuildDriveFailure> {
    if !available("ninja") {
        return Err(BuildDriveFailure::ToolAbsent { tool: "ninja" });
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(BuildDriveFailure::Cancelled);
    }
    let mut command = Command::new("ninja");
    if let Some(path) = child_path() {
        command.env("PATH", path);
    }
    command
        .arg("-C")
        .arg(build)
        .args(["-t", "compdb", "c", "cxx"])
        .output()
        .map_err(|cause| BuildDriveFailure::DriveFailed {
            tool: "ninja",
            captured: capture(cause.to_string().as_bytes()),
        })
}

fn run_make(root: &Path, cancelled: &AtomicBool) -> Result<Output, BuildDriveFailure> {
    if !available("make") {
        return Err(BuildDriveFailure::ToolAbsent { tool: "make" });
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(BuildDriveFailure::Cancelled);
    }
    let mut command = Command::new("make");
    if let Some(path) = child_path() {
        command.env("PATH", path);
    }
    command
        .args(["-n", "-C", &root.to_string_lossy()])
        .output()
        .map_err(|cause| BuildDriveFailure::DriveFailed {
            tool: "make",
            captured: capture(cause.to_string().as_bytes()),
        })
}

fn capture(bytes: &[u8]) -> CapturedOutput {
    let retained = bytes.len().min(CAPTURE_BYTES);
    CapturedOutput {
        bytes: bytes[..retained].to_vec(),
        truncated: bytes.len() > retained,
    }
}

fn parse_make(bytes: &[u8], root: &Path) -> Result<Vec<DrivenTranslationUnit>, BuildDriveFailure> {
    let text = String::from_utf8_lossy(bytes);
    let mut result = Vec::new();
    for line in text.lines() {
        let argv = shell_words(line);
        if argv.len() > compiler_languages_clang::MAX_DATABASE_ARGUMENTS {
            return Err(BuildDriveFailure::CommandCapacity {
                required: argv.len(),
                capacity: compiler_languages_clang::MAX_DATABASE_ARGUMENTS,
            });
        }
        let has_compile = argv.iter().any(|arg| arg == "-c");
        let source = argv.iter().find(|arg| is_source(arg));
        if !has_compile {
            continue;
        }
        let Some(source) = source else {
            continue;
        };
        if !recognized_compiler(&argv) {
            return Err(BuildDriveFailure::UnrecognizedCompileCommand {
                line: line.to_owned(),
            });
        }
        result.push(DrivenTranslationUnit {
            source: PathBuf::from(source),
            arguments: argv,
            directory: root.to_path_buf(),
        });
    }
    Ok(result)
}

fn recognized_compiler(argv: &[String]) -> bool {
    let is_compiler = |value: &str| {
        let name = Path::new(value)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(value);
        name.ends_with("clang") || name.ends_with("clang++") || name == "cc" || name == "c++"
    };
    argv.first().is_some_and(|compiler| is_compiler(compiler))
        || (argv.first().is_some_and(|wrapper| {
            matches!(
                Path::new(wrapper)
                    .file_name()
                    .and_then(|name| name.to_str()),
                Some("ccache" | "sccache")
            )
        }) && argv.get(1).is_some_and(|compiler| is_compiler(compiler)))
}

fn is_source(value: &str) -> bool {
    [".c", ".cc", ".cpp", ".cxx"]
        .iter()
        .any(|suffix| value.ends_with(suffix))
}

fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

#[derive(Debug, Error)]
pub enum CompdbError {
    #[error("invalid compilation database JSON at byte {offset}")]
    Json { offset: usize },
    #[error("compilation database entry at byte {offset} is missing file")]
    MissingFile { offset: usize },
    #[error("compilation database entry at byte {offset} has neither arguments nor command")]
    MissingCommand { offset: usize },
    #[error("compilation database command has {required} arguments, capacity is {capacity}")]
    Capacity { required: usize, capacity: usize },
}

struct JsonReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> JsonReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn ws(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn take(&mut self, byte: u8) -> Result<(), CompdbError> {
        self.ws();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(CompdbError::Json { offset: self.at })
        }
    }
    fn string(&mut self) -> Result<String, CompdbError> {
        self.ws();
        if self.bytes.get(self.at) != Some(&b'"') {
            return Err(CompdbError::Json { offset: self.at });
        }
        self.at += 1;
        let mut out = String::new();
        while let Some(&byte) = self.bytes.get(self.at) {
            self.at += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escaped = *self
                        .bytes
                        .get(self.at)
                        .ok_or(CompdbError::Json { offset: self.at })?;
                    self.at += 1;
                    let value = match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        _ => {
                            return Err(CompdbError::Json {
                                offset: self.at - 1,
                            });
                        }
                    };
                    out.push(value);
                }
                byte if byte.is_ascii() && byte < 0x20 => {
                    return Err(CompdbError::Json {
                        offset: self.at - 1,
                    });
                }
                byte => out.push(byte as char),
            }
        }
        Err(CompdbError::Json { offset: self.at })
    }
}

fn read_compdb(bytes: &[u8]) -> Result<Vec<DrivenTranslationUnit>, CompdbError> {
    let mut reader = JsonReader::new(bytes);
    reader.take(b'[')?;
    let mut result = Vec::new();
    reader.ws();
    while reader.bytes.get(reader.at) != Some(&b']') {
        let entry_at = reader.at;
        reader.take(b'{')?;
        let mut directory = None;
        let mut file = None;
        let mut arguments = None;
        let mut command = None;
        reader.ws();
        while reader.bytes.get(reader.at) != Some(&b'}') {
            let key = reader.string()?;
            reader.take(b':')?;
            match key.as_str() {
                "directory" => directory = Some(reader.string()?),
                "file" => file = Some(reader.string()?),
                "arguments" => {
                    reader.take(b'[')?;
                    let mut values = Vec::new();
                    reader.ws();
                    while reader.bytes.get(reader.at) != Some(&b']') {
                        values.push(reader.string()?);
                        if values.len() > compiler_languages_clang::MAX_DATABASE_ARGUMENTS {
                            return Err(CompdbError::Capacity {
                                required: values.len(),
                                capacity: compiler_languages_clang::MAX_DATABASE_ARGUMENTS,
                            });
                        }
                        reader.ws();
                        if reader.bytes.get(reader.at) == Some(&b',') {
                            reader.at += 1;
                        } else {
                            break;
                        }
                    }
                    reader.take(b']')?;
                    arguments = Some(values);
                }
                "command" => command = Some(reader.string()?),
                "output" => {
                    let _ = reader.string()?;
                }
                _ => return Err(CompdbError::Json { offset: reader.at }),
            }
            reader.ws();
            if reader.bytes.get(reader.at) == Some(&b',') {
                reader.at += 1;
                reader.ws();
            } else {
                break;
            }
        }
        reader.take(b'}')?;
        let source = file.ok_or(CompdbError::MissingFile { offset: entry_at })?;
        let directory = directory.ok_or(CompdbError::Json { offset: entry_at })?;
        let arguments = match (arguments, command) {
            (Some(values), None) => values,
            (None, Some(value)) => shell_words(&value),
            _ => return Err(CompdbError::MissingCommand { offset: entry_at }),
        };
        if arguments.len() > compiler_languages_clang::MAX_DATABASE_ARGUMENTS {
            return Err(CompdbError::Capacity {
                required: arguments.len(),
                capacity: compiler_languages_clang::MAX_DATABASE_ARGUMENTS,
            });
        }
        result.push(DrivenTranslationUnit {
            source: PathBuf::from(source),
            arguments,
            directory: PathBuf::from(directory),
        });
        reader.ws();
        if reader.bytes.get(reader.at) == Some(&b',') {
            reader.at += 1;
            reader.ws();
        } else {
            break;
        }
    }
    reader.take(b']')?;
    reader.ws();
    if reader.at != bytes.len() {
        return Err(CompdbError::Json { offset: reader.at });
    }
    Ok(result)
}
