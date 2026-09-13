//! Exercises the `backend-flow` operation tests support native-tooling contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::atomic::{AtomicUsize, Ordering},
};

use compiler_driver::{NativeTool, ResolvedToolchain, ToolchainResolutionError};
use thiserror::Error;

static WORK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
const STABLE_TOOLCHAIN_ENV: &str = "COMPILER_STABLE_TOOLCHAIN";
const PYTHON_COMPILER_ENV: &str = "COMPILER_PYTHON_COMPILER";
const CLANG_COMPILER_ENV: &str = "COMPILER_CLANG_COMPILER";
const TYPESCRIPT_COMPILER_ENV: &str = "COMPILER_TYPESCRIPT_COMPILER";
const GO_COMPILER_ENV: &str = "COMPILER_GO_COMPILER";
const JAVA_COMPILER_ENV: &str = "COMPILER_JAVA_COMPILER";
const CSHARP_COMPILER_ENV: &str = "COMPILER_CSHARP_COMPILER";

#[derive(Debug, Error)]
pub(crate) enum NativeToolingError {
    #[error("PATH is unavailable while resolving host tool {tool:?}")]
    MissingPath { tool: NativeTool },
    #[error("configured host executable for {tool:?} is unavailable")]
    MissingTool { tool: NativeTool },
    #[error("could not canonicalize the {tool:?} compiler fixture")]
    Canonicalize {
        tool: NativeTool,
        #[source]
        source: std::io::Error,
    },
    #[error("could not probe the exact {tool:?} compiler version")]
    ProbeVersion {
        tool: NativeTool,
        #[source]
        source: std::io::Error,
    },
    #[error("the {tool:?} compiler version probe was rejected with {status}")]
    VersionRejected {
        tool: NativeTool,
        status: ExitStatus,
    },
    #[error("the {tool:?} compiler version probe returned no identity bytes")]
    EmptyVersion { tool: NativeTool },
    #[error("could not bind the resolved native compiler")]
    Resolve(#[from] ToolchainResolutionError),
    #[error("could not create caller-owned native work")]
    CreateWork(#[source] std::io::Error),
    #[error("could not inspect caller-owned native work")]
    InspectWork(#[source] std::io::Error),
    #[error("native work retained an unexpected artifact after compilation")]
    WorkNotEmpty,
}

pub(crate) struct HostTool {
    tool: NativeTool,
    executable: PathBuf,
}

impl HostTool {
    pub(crate) fn resolve(tool: NativeTool) -> Result<Self, NativeToolingError> {
        let variable = configured_variable(tool);
        let executable = match env::var_os(variable).map(PathBuf::from) {
            Some(configured) => {
                let executable = if tool == NativeTool::Rustc {
                    configured.join("bin/rustc")
                } else {
                    configured
                };
                if !executable.is_file() {
                    return Err(NativeToolingError::MissingTool { tool });
                }
                executable
            }
            None => path_executable(tool)?,
        };
        Ok(Self {
            tool,
            executable: executable
                .canonicalize()
                .map_err(|source| NativeToolingError::Canonicalize { tool, source })?,
        })
    }

    pub(crate) fn toolchain(&self) -> Result<ResolvedToolchain<'_>, NativeToolingError> {
        let version = Command::new(&self.executable)
            .arg(version_argument(self.tool))
            .output()
            .map_err(|source| NativeToolingError::ProbeVersion {
                tool: self.tool,
                source,
            })?;
        if !version.status.success() {
            return Err(NativeToolingError::VersionRejected {
                tool: self.tool,
                status: version.status,
            });
        }
        let identity_bytes = if version.stdout.is_empty() {
            version.stderr.as_slice()
        } else {
            version.stdout.as_slice()
        };
        if identity_bytes.is_empty() {
            return Err(NativeToolingError::EmptyVersion { tool: self.tool });
        }
        Ok(ResolvedToolchain::from_version(
            self.tool,
            &self.executable,
            identity_bytes,
        )?)
    }

    /// Returns the exact executable selected by the bounded host resolver.
    ///
    /// Corpus authority producers use this only to derive their own typed
    /// toolchain root.  They never perform an ambient PATH lookup after this
    /// boundary has been crossed.
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }
}

fn path_executable(tool: NativeTool) -> Result<PathBuf, NativeToolingError> {
    let paths = env::var_os("PATH").ok_or(NativeToolingError::MissingPath { tool })?;
    let name = executable_name(tool);
    for directory in env::split_paths(&paths) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|source| NativeToolingError::Canonicalize { tool, source });
        }
    }
    Err(NativeToolingError::MissingTool { tool })
}

const fn executable_name(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::Rustc => "rustc",
        NativeTool::Python => "python3",
        NativeTool::Clang => "clang",
        NativeTool::TypeScriptCompiler => "tsc",
        NativeTool::GoCompiler => "go",
        NativeTool::JavaCompiler => "javac",
        NativeTool::CSharpCompiler => "dotnet",
    }
}

const fn configured_variable(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::Rustc => STABLE_TOOLCHAIN_ENV,
        NativeTool::Python => PYTHON_COMPILER_ENV,
        NativeTool::Clang => CLANG_COMPILER_ENV,
        NativeTool::TypeScriptCompiler => TYPESCRIPT_COMPILER_ENV,
        NativeTool::GoCompiler => GO_COMPILER_ENV,
        NativeTool::JavaCompiler => JAVA_COMPILER_ENV,
        NativeTool::CSharpCompiler => CSHARP_COMPILER_ENV,
    }
}

const fn version_argument(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::GoCompiler => "version",
        NativeTool::Rustc
        | NativeTool::Python
        | NativeTool::Clang
        | NativeTool::TypeScriptCompiler
        | NativeTool::JavaCompiler
        | NativeTool::CSharpCompiler => "--version",
    }
}

pub(crate) struct NativeWork {
    path: PathBuf,
}

impl NativeWork {
    pub(crate) fn create() -> Result<Self, NativeToolingError> {
        let sequence = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "backend-flow-operation-compiler-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(NativeToolingError::CreateWork)?;
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn assert_empty(&self) -> Result<(), NativeToolingError> {
        let mut entries = fs::read_dir(&self.path).map_err(NativeToolingError::InspectWork)?;
        match entries.next() {
            Some(Ok(_entry)) => Err(NativeToolingError::WorkNotEmpty),
            Some(Err(source)) => Err(NativeToolingError::InspectWork(source)),
            None => Ok(()),
        }
    }
}

impl Drop for NativeWork {
    fn drop(&mut self) {
        let _removed = fs::remove_dir(&self.path);
    }
}
