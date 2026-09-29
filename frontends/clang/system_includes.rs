//! Probe the system include paths for one explicitly selected Clang driver.
//!
//! The compiler authority passes a canonical driver path into this module.
//! Every query runs that exact executable with a cleared environment; this
//! module never consults `NUDOX_CLANG_DRIVER`, `PATH`, `cc`, or any other
//! ambient selector. The resulting resource directory, sysroot, and include
//! paths are retained by [`super::authority::ClangAuthorityEnvironment`] and
//! included in the arguments sent to libclang.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_PROBE_TIME: Duration = Duration::from_secs(5);
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Toolchain facts reported by the selected driver itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DriverProbe {
    /// Clang's default builtin headers for this driver.
    pub(crate) resource_dir: PathBuf,
    /// Effective target sysroot, absent when the driver reports none.
    pub(crate) sysroot: Option<PathBuf>,
    /// Ordered angle-bracket include search paths, excluding frameworks.
    pub(crate) system_include_dirs: Vec<PathBuf>,
}

/// Queries resource headers, sysroot, and default system includes from one
/// already canonicalized Clang driver.
pub(crate) fn probe(driver: &Path) -> Result<DriverProbe, ProbeError> {
    let (resource, _) = run(driver, "resource directory", ["-print-resource-dir"])?;
    let resource_dir = canonical_reported_path(driver, "resource directory", &resource)?;

    let (sysroot, _) = run(driver, "sysroot", ["-print-sysroot"])?;
    let sysroot = if sysroot.trim().is_empty() {
        None
    } else {
        Some(canonical_reported_path(driver, "sysroot", &sysroot)?)
    };

    let (_, stderr) = run(
        driver,
        "system include search list",
        ["-v", "-E", "-x", "c++", "-"],
    )?;
    let parsed = parse_search_list(&stderr);
    if parsed.is_empty() {
        return Err(ProbeError::MalformedOutput {
            driver: driver.to_path_buf(),
            operation: "system include search list",
        });
    }
    let mut system_include_dirs = Vec::with_capacity(parsed.len());
    for path in parsed {
        let canonical = path.canonicalize().map_err(|source| ProbeError::PathIo {
            driver: driver.to_path_buf(),
            operation: "system include search list",
            path: path.clone(),
            source,
        })?;
        if !canonical.is_dir() {
            return Err(ProbeError::InvalidPath {
                driver: driver.to_path_buf(),
                operation: "system include search list",
                path: canonical,
            });
        }
        system_include_dirs.push(canonical);
    }

    Ok(DriverProbe {
        resource_dir,
        sysroot,
        system_include_dirs,
    })
}

/// Runs an exact driver command with no inherited environment.
fn run<const N: usize>(
    driver: &Path,
    operation: &'static str,
    arguments: [&str; N],
) -> Result<(String, String), ProbeError> {
    let mut command = Command::new(driver);
    command
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| ProbeError::Spawn {
        driver: driver.to_path_buf(),
        operation,
        source,
    })?;
    let stdout = child.stdout.take().ok_or_else(|| ProbeError::Spawn {
        driver: driver.to_path_buf(),
        operation,
        source: std::io::Error::other("driver stdout pipe unavailable"),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| ProbeError::Spawn {
        driver: driver.to_path_buf(),
        operation,
        source: std::io::Error::other("driver stderr pipe unavailable"),
    })?;
    let deadline = Instant::now() + MAX_PROBE_TIME;
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = stdout_tx.send(read_bounded(stdout));
    });
    thread::spawn(move || {
        let _ = stderr_tx.send(read_bounded(stderr));
    });
    let status = wait_bounded(&mut child, driver, operation, deadline)?;
    let stdout = receive_output(stdout_rx, driver, operation, deadline)?;
    let stderr = receive_output(stderr_rx, driver, operation, deadline)?;
    if stdout.1 || stderr.1 {
        return Err(ProbeError::OutputTooLarge {
            driver: driver.to_path_buf(),
            operation,
            limit: MAX_PROBE_OUTPUT_BYTES,
        });
    }
    if !status.success() {
        return Err(ProbeError::Failed {
            driver: driver.to_path_buf(),
            operation,
            status: status.to_string(),
        });
    }
    Ok((
        String::from_utf8_lossy(&stdout.0).trim().to_owned(),
        String::from_utf8_lossy(&stderr.0).trim().to_owned(),
    ))
}

fn read_bounded(mut reader: impl std::io::Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let room = MAX_PROBE_OUTPUT_BYTES.saturating_sub(output.len());
        let retained = read.min(room);
        output.extend_from_slice(&buffer[..retained]);
        overflow |= retained < read;
    }
    Ok((output, overflow))
}

fn receive_output(
    receiver: mpsc::Receiver<std::io::Result<(Vec<u8>, bool)>>,
    driver: &Path,
    operation: &'static str,
    deadline: Instant,
) -> Result<(Vec<u8>, bool), ProbeError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let output = receiver
        .recv_timeout(remaining)
        .map_err(|_| ProbeError::Timeout {
            driver: driver.to_path_buf(),
            operation,
            timeout: MAX_PROBE_TIME,
        })?;
    output.map_err(|source| ProbeError::ReadOutput { operation, source })
}

fn wait_bounded(
    child: &mut Child,
    driver: &Path,
    operation: &'static str,
    deadline: Instant,
) -> Result<ExitStatus, ProbeError> {
    loop {
        match child.try_wait().map_err(|source| ProbeError::Spawn {
            driver: driver.to_path_buf(),
            operation,
            source,
        })? {
            Some(status) => return Ok(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProbeError::Timeout {
                    driver: driver.to_path_buf(),
                    operation,
                    timeout: MAX_PROBE_TIME,
                });
            }
            None => thread::sleep(CHILD_POLL_INTERVAL),
        }
    }
}

fn canonical_reported_path(
    driver: &Path,
    operation: &'static str,
    output: &str,
) -> Result<PathBuf, ProbeError> {
    let path = PathBuf::from(output.trim());
    if !path.is_absolute() {
        return Err(ProbeError::MalformedOutput {
            driver: driver.to_path_buf(),
            operation,
        });
    }
    let canonical = path.canonicalize().map_err(|source| ProbeError::PathIo {
        driver: driver.to_path_buf(),
        operation,
        path: path.clone(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(ProbeError::InvalidPath {
            driver: driver.to_path_buf(),
            operation,
            path: canonical,
        });
    }
    Ok(canonical)
}

/// One typed failure while probing the selected driver.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProbeError {
    /// The executable could not be started or its probe input could not be written.
    #[error("could not run Clang driver {driver} for {operation}: {source}", driver = driver.display())]
    Spawn {
        driver: PathBuf,
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The selected driver rejected a probe command.
    #[error("Clang driver {driver} failed {operation} probe with {status}", driver = driver.display())]
    Failed {
        driver: PathBuf,
        operation: &'static str,
        status: String,
    },
    /// The exact driver exceeded the bounded probe window.
    #[error("Clang driver {driver} exceeded the {timeout:?} limit during {operation}", driver = driver.display())]
    Timeout {
        driver: PathBuf,
        operation: &'static str,
        timeout: Duration,
    },
    /// A driver emitted more output than the authority probe retains.
    #[error("Clang driver {driver} exceeded the {limit}-byte output limit during {operation}", driver = driver.display())]
    OutputTooLarge {
        driver: PathBuf,
        operation: &'static str,
        limit: usize,
    },
    /// A probe output stream could not be read.
    #[error("could not read Clang driver output during {operation}: {source}")]
    ReadOutput {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The selected driver emitted an invalid path or no parseable include block.
    #[error("Clang driver {driver} emitted malformed {operation} output", driver = driver.display())]
    MalformedOutput {
        driver: PathBuf,
        operation: &'static str,
    },
    /// A reported path could not be inspected.
    #[error("could not inspect {operation} path {path} from Clang driver {driver}: {source}", driver = driver.display(), path = path.display())]
    PathIo {
        driver: PathBuf,
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A reported toolchain directory was not a directory.
    #[error("Clang driver {driver} reported a non-directory {operation} path: {path}", driver = driver.display(), path = path.display())]
    InvalidPath {
        driver: PathBuf,
        operation: &'static str,
        path: PathBuf,
    },
}

/// The directory list from a `clang -v` (or `gcc -v`) stderr dump.
/// Framework directories are skipped because their lookup semantics require
/// `-F`, not `-isystem`.
fn parse_search_list(stderr: &str) -> Vec<PathBuf> {
    let mut in_list = false;
    let mut dirs = Vec::new();
    for line in stderr.lines() {
        if line.contains("#include <...> search starts here:") {
            in_list = true;
            continue;
        }
        if line.trim() == "End of search list." {
            break;
        }
        if !in_list {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.ends_with("(framework directory)") {
            continue;
        }
        dirs.push(PathBuf::from(trimmed));
    }
    dirs
}

/// Explicitly selected system include arguments in driver search order.
pub(crate) fn args(dirs: &[PathBuf]) -> Vec<String> {
    dirs.iter()
        .flat_map(|dir| ["-isystem".to_owned(), dir.to_string_lossy().into_owned()])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_representative_clang_verbose_dump() {
        let stderr = "\
clang version 21.1.8
Target: arm64-apple-darwin
#include \"...\" search starts here:
#include <...> search starts here:
 /nix/store/aaa-libcxx/include
 /nix/store/aaa-libcxx/include/c++/v1
 /nix/store/aaa-sdk/System/Library/Frameworks (framework directory)
End of search list.
";
        assert_eq!(
            parse_search_list(stderr),
            vec![
                PathBuf::from("/nix/store/aaa-libcxx/include"),
                PathBuf::from("/nix/store/aaa-libcxx/include/c++/v1"),
            ]
        );
    }

    #[test]
    fn missing_search_list_yields_empty() {
        assert!(parse_search_list("no useful output here").is_empty());
    }
}
