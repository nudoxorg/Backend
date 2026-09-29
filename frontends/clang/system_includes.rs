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

    // The verbose preprocessing dump comes before the sysroot query: it is the
    // include search list's source, and it is also where a driver that cannot
    // answer `-print-sysroot` names the SDK it will use.
    let (_, stderr) = run(
        driver,
        "system include search list",
        ["-v", "-E", "-x", "c++", "-"],
    )?;
    let sysroot = probe_sysroot(driver, &stderr)?;
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

/// The effective sysroot of the selected driver, when it has one.
///
/// GCC-style `-print-sysroot` is the query. Apple's Clang and the pinned Nix
/// wrapper reject it as an unknown argument, yet still pass their SDK to the
/// compiler proper as `-isysroot`, which the verbose preprocessing dump
/// (`verbose`, the stderr of `-v -E -x c++ -`) prints. A driver that rejects
/// the query and names no `-isysroot` has no sysroot. Any other failure of the
/// query stays a failure: only the documented rejection selects the fallback.
fn probe_sysroot(driver: &Path, verbose: &str) -> Result<Option<PathBuf>, ProbeError> {
    let queried = run_capture(driver, "sysroot", ["-print-sysroot"])?;
    let reported = if queried.status.success() {
        queried.stdout
    } else if rejects_sysroot_query(&queried.stderr) {
        effective_isysroot(verbose).unwrap_or_default()
    } else {
        return Err(ProbeError::Failed {
            driver: driver.to_path_buf(),
            operation: "sysroot",
            status: queried.status.to_string(),
        });
    };
    if reported.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(canonical_reported_path(driver, "sysroot", &reported)?))
}

/// Whether a failed `-print-sysroot` is the driver saying it does not have
/// that query, as opposed to failing to answer it.
fn rejects_sysroot_query(stderr: &str) -> bool {
    stderr.contains("unknown argument: '-print-sysroot'")
}

/// The last `-isysroot` value in a `clang -v` dump, as the compiler proper
/// received it. The driver prints each `-cc1` argument as one token, wrapping
/// (and backslash-escaping inside) double quotes only where the text needs it,
/// so the value is read as a token rather than cut at the next space.
fn effective_isysroot(verbose: &str) -> Option<String> {
    let mut found = None;
    for line in verbose.lines() {
        let mut tokens = command_line_tokens(line).into_iter();
        while let Some(token) = tokens.next() {
            if token == "-isysroot"
                && let Some(value) = tokens.next()
            {
                found = Some(value);
            }
        }
    }
    found
}

/// One command-line's tokens: whitespace-separated, with double quotes
/// grouping and a backslash escaping the next character inside them.
fn command_line_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                if let Some(escaped) = characters.next() {
                    current.push(escaped);
                }
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
}

/// Runs an exact driver command with no inherited environment and requires it
/// to succeed.
fn run<const N: usize>(
    driver: &Path,
    operation: &'static str,
    arguments: [&str; N],
) -> Result<(String, String), ProbeError> {
    let captured = run_capture(driver, operation, arguments)?;
    if !captured.status.success() {
        return Err(ProbeError::Failed {
            driver: driver.to_path_buf(),
            operation,
            status: captured.status.to_string(),
        });
    }
    Ok((captured.stdout, captured.stderr))
}

/// What one bounded driver run left behind, whatever its exit status.
struct Captured {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// Runs an exact driver command with no inherited environment.
fn run_capture<const N: usize>(
    driver: &Path,
    operation: &'static str,
    arguments: [&str; N],
) -> Result<Captured, ProbeError> {
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
    Ok(Captured {
        status,
        stdout: String::from_utf8_lossy(&stdout.0).trim().to_owned(),
        stderr: String::from_utf8_lossy(&stderr.0).trim().to_owned(),
    })
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

    #[test]
    fn the_effective_isysroot_is_read_as_a_token_from_the_cc1_line() {
        let dump = "\
clang version 21.1.8
 \"/nix/store/aaa-clang/bin/clang-21\" -cc1 -triple arm64-apple-macosx14.0.0 -fmacro-prefix-map=/x/-isysroot=/y -isysroot /nix/store/bbb-sdk/MacOSX.sdk -internal-isystem /nix/store/ccc/include -x c++ -
";
        assert_eq!(
            effective_isysroot(dump).as_deref(),
            Some("/nix/store/bbb-sdk/MacOSX.sdk")
        );
        // Quoted where the text needs it, including the option itself.
        assert_eq!(
            effective_isysroot(
                " \"-cc1\" \"-isysroot\" \"/Applications/My Xcode/SDK\\\"s\" \"-x\""
            )
            .as_deref(),
            Some("/Applications/My Xcode/SDK\"s"),
        );
        // The compiler proper honours the last one.
        assert_eq!(
            effective_isysroot("-cc1 -isysroot /a -isysroot /b -x c++ -").as_deref(),
            Some("/b")
        );
        assert_eq!(effective_isysroot("-cc1 -x c++ -"), None);
        assert_eq!(effective_isysroot("-cc1 -isysroot"), None);
    }

    #[cfg(unix)]
    mod driver {
        use super::*;
        use std::{fs, os::unix::fs::PermissionsExt as _};

        /// What a fixture driver answers. `-v` always prints the include
        /// search list of `include`, plus whatever `verbose_extra` adds before it.
        struct Driver<'a> {
            /// The `-print-sysroot` clause of the script's `case`.
            print_sysroot: &'a str,
            /// Extra stderr the verbose dump prints (the `-cc1` line).
            verbose_extra: String,
        }

        fn shell_quote(path: &Path) -> String {
            format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
        }

        /// A script standing in for a Clang driver under `root`, with real
        /// resource and include directories beside it.
        fn write_driver(root: &Path, driver: &Driver<'_>) -> (PathBuf, PathBuf) {
            let resource = root.join("resource");
            let include = root.join("include");
            fs::create_dir_all(&resource).expect("resource dir");
            fs::create_dir_all(&include).expect("include dir");
            let script = format!(
                "#!/bin/sh\ncase \"$1\" in\n  -print-resource-dir) printf '%s\\n' {resource};;\n  -print-sysroot) {sysroot};;\n  -v) printf '%s\\n' {extra} >&2; printf '#include <...> search starts here:\\n %s\\nEnd of search list.\\n' {include} >&2;;\n  *) exit 2;;\nesac\n",
                resource = shell_quote(&resource),
                sysroot = driver.print_sysroot,
                extra = shell_quote(Path::new(&driver.verbose_extra)),
                include = shell_quote(&include),
            );
            let path = root.join("driver");
            fs::write(&path, script).expect("driver script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("executable driver");
            (path, include)
        }

        /// The rejection the Nix wrapper and Apple's Clang give, as the
        /// `-print-sysroot` clause of a script.
        const REJECTS: &str =
            "echo \"clang: error: unknown argument: '-print-sysroot'\" >&2; exit 1";

        #[test]
        fn a_driver_that_rejects_print_sysroot_reports_the_isysroot_its_verbose_dump_names() {
            let temp = tempfile::tempdir().expect("temporary fixture");
            let sdk = temp.path().join("sdk");
            fs::create_dir_all(&sdk).expect("sdk dir");
            let (driver, include) = write_driver(
                temp.path(),
                &Driver {
                    print_sysroot: REJECTS,
                    verbose_extra: format!(
                        " \"/x/clang-21\" -cc1 -triple arm64-apple-macosx14.0.0 -isysroot {} -x c++ -",
                        sdk.display()
                    ),
                },
            );

            let facts = probe(&driver)
                .expect("a rejected -print-sysroot is recovered from the verbose dump");

            assert_eq!(
                facts.sysroot,
                Some(sdk.canonicalize().expect("sdk")),
                "the sysroot is the SDK the verbose dump names"
            );
            assert_eq!(
                facts.system_include_dirs,
                vec![include.canonicalize().expect("include")]
            );
        }

        #[test]
        fn a_quoted_isysroot_with_a_space_survives_the_recovery() {
            let temp = tempfile::tempdir().expect("temporary fixture");
            let sdk = temp.path().join("My SDK");
            fs::create_dir_all(&sdk).expect("sdk dir");
            let (driver, _) = write_driver(
                temp.path(),
                &Driver {
                    print_sysroot: REJECTS,
                    verbose_extra: format!(
                        " \"/x/clang-21\" -cc1 -isysroot \"{}\" -x c++ -",
                        sdk.display()
                    ),
                },
            );

            let facts = probe(&driver).expect("the quoted value is one token");

            assert_eq!(facts.sysroot, Some(sdk.canonicalize().expect("sdk")));
        }

        #[test]
        fn a_driver_that_rejects_print_sysroot_and_names_no_isysroot_has_no_sysroot() {
            let temp = tempfile::tempdir().expect("temporary fixture");
            let (driver, _) = write_driver(
                temp.path(),
                &Driver {
                    print_sysroot: REJECTS,
                    verbose_extra: " \"/x/clang-21\" -cc1 -x c++ -".to_owned(),
                },
            );

            let facts = probe(&driver).expect("no sysroot is a valid answer");

            assert_eq!(facts.sysroot, None);
        }

        #[test]
        fn a_print_sysroot_answer_wins_over_the_verbose_dump() {
            let temp = tempfile::tempdir().expect("temporary fixture");
            let answered = temp.path().join("answered");
            let named = temp.path().join("named");
            fs::create_dir_all(&answered).expect("answered dir");
            fs::create_dir_all(&named).expect("named dir");
            let (driver, _) = write_driver(
                temp.path(),
                &Driver {
                    print_sysroot: &format!("printf '%s\\n' {}", shell_quote(&answered)),
                    verbose_extra: format!(" -cc1 -isysroot {} -x c++ -", named.display()),
                },
            );

            let facts = probe(&driver).expect("a driver that answers is asked, not second-guessed");

            assert_eq!(
                facts.sysroot,
                Some(answered.canonicalize().expect("answered"))
            );
        }

        #[test]
        fn a_print_sysroot_that_fails_for_another_reason_is_still_a_failure() {
            let temp = tempfile::tempdir().expect("temporary fixture");
            let sdk = temp.path().join("sdk");
            fs::create_dir_all(&sdk).expect("sdk dir");
            let (driver, _) = write_driver(
                temp.path(),
                &Driver {
                    print_sysroot: "echo 'clang: error: unable to execute command: Segmentation fault' >&2; exit 3",
                    verbose_extra: format!(" -cc1 -isysroot {} -x c++ -", sdk.display()),
                },
            );

            let error = probe(&driver)
                .expect_err("only the unknown-argument rejection selects the fallback");

            assert!(
                matches!(
                    &error,
                    ProbeError::Failed {
                        operation: "sysroot",
                        ..
                    }
                ),
                "the failed query is reported as itself: {error:?}"
            );
        }
    }
}
