use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::atomic::{AtomicUsize, Ordering},
};

use nudox_compile_driver::{NativeTool, ResolvedToolchain, ToolchainResolutionError};
use thiserror::Error;

static WORK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
const STABLE_TOOLCHAIN_ENV: &str = "NUDOX_STABLE_TOOLCHAIN";

#[derive(Debug, Error)]
pub(crate) enum NativeToolingError {
    #[error("PATH is unavailable while resolving {tool:?}")]
    MissingPath { tool: NativeTool },
    #[error("{tool:?} is unavailable to the public compiler journey")]
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
    pub(crate) fn resolve(
        executable_name: &str,
        tool: NativeTool,
    ) -> Result<Self, NativeToolingError> {
        if tool == NativeTool::Rustc
            && let Some(toolchain_root) = env::var_os(STABLE_TOOLCHAIN_ENV)
        {
            let candidate = PathBuf::from(toolchain_root).join("bin/rustc");
            if candidate.is_file() {
                return Ok(Self {
                    tool,
                    executable: candidate
                        .canonicalize()
                        .map_err(|source| NativeToolingError::Canonicalize { tool, source })?,
                });
            }
        }
        let paths = env::var_os("PATH").ok_or(NativeToolingError::MissingPath { tool })?;
        for directory in env::split_paths(&paths) {
            let candidate = directory.join(executable_name);
            if candidate.is_file() {
                return Ok(Self {
                    tool,
                    executable: candidate
                        .canonicalize()
                        .map_err(|source| NativeToolingError::Canonicalize { tool, source })?,
                });
            }
        }
        Err(NativeToolingError::MissingTool { tool })
    }

    pub(crate) fn toolchain(&self) -> Result<ResolvedToolchain<'_>, NativeToolingError> {
        let version = Command::new(&self.executable)
            .arg("--version")
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
}

pub(crate) struct NativeWork {
    path: PathBuf,
}

impl NativeWork {
    pub(crate) fn create() -> Result<Self, NativeToolingError> {
        let sequence = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "nudox-operation-compiler-{}-{sequence}",
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
