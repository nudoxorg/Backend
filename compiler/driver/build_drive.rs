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

use compiler_languages_clang::CompilationDatabase;
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
    ToolPresentUndrivable { tool: &'static str },
    #[error("no build system marker under {root}")]
    NoBuildSystemDetected { root: PathBuf },
    #[error("build drive was cancelled")]
    Cancelled,
    #[error("build path operation failed")]
    Io(#[source] std::io::Error),
    #[error("generated compilation database was rejected")]
    Database(#[source] compiler_languages_clang::DatabaseError),
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
            return Err(BuildDriveFailure::ToolPresentUndrivable {
                tool: system.tool(),
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
        write_database(&database_directory, &commands).map_err(BuildDriveFailure::Io)?;
    } else if system == BuildSystem::Meson {
        fs::write(
            database_directory.join("compile_commands.json"),
            &output.stdout,
        )
        .map_err(BuildDriveFailure::Io)?;
    }
    let database = CompilationDatabase::from_directory(&database_directory)
        .map_err(BuildDriveFailure::Database)?;
    if database.commands().is_empty() {
        return Err(BuildDriveFailure::NoTranslationUnits {
            tool: system.tool(),
        });
    }
    let translation_units = database
        .commands()
        .iter()
        .map(|command| DrivenTranslationUnit {
            source: PathBuf::from(command.file_name().to_string_lossy().into_owned()),
            arguments: command
                .arguments()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            directory: PathBuf::from(command.directory().to_string_lossy().into_owned()),
        })
        .collect();
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
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
        .unwrap_or(false)
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
    Command::new("ninja")
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
    Command::new("make")
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

fn json(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_database(
    directory: &Path,
    commands: &[DrivenTranslationUnit],
) -> Result<(), std::io::Error> {
    let mut text = String::from("[");
    for (index, command) in commands.iter().enumerate() {
        if index != 0 {
            text.push(',');
        }
        text.push_str("{\"directory\":");
        text.push_str(&json(&command.directory.to_string_lossy()));
        text.push_str(",\"file\":");
        text.push_str(&json(&command.source.to_string_lossy()));
        text.push_str(",\"arguments\":[");
        for (arg, value) in command.arguments.iter().enumerate() {
            if arg != 0 {
                text.push(',');
            }
            text.push_str(&json(value));
        }
        text.push_str("]}");
    }
    text.push(']');
    fs::write(directory.join("compile_commands.json"), text)
}
