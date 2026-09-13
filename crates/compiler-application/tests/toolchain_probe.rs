//! Ordinary falsifiers for bounded native toolchain identity admission.

use std::{num::NonZeroUsize, path::PathBuf, time::Duration};

#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

use compiler_application::{
    LocalRuntimeToolchain, ToolchainProbeError, ToolchainProbeLimits, ToolchainProbePrimary,
};
use backend_semantic::vocabulary::{NativeTool, NativeWorker};

#[cfg(unix)]
static NEXT_PROGRAM: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
struct ProbeProgram {
    root: PathBuf,
    executable: PathBuf,
}

#[cfg(unix)]
impl ProbeProgram {
    fn new(body: &str) -> Self {
        let sequence = NEXT_PROGRAM.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "nudox-toolchain-probe-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("each probe fixture owns a fresh directory");
        let executable = root.join("probe.sh");
        fs::write(&executable, format!("#!/bin/sh\n{body}\n"))
            .expect("the probe fixture source is writable");
        let mut permissions = fs::metadata(&executable)
            .expect("the probe fixture has metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions)
            .expect("the probe fixture is executable by its owner");
        Self { root, executable }
    }
}

#[cfg(unix)]
impl Drop for ProbeProgram {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("the owned probe fixture is removable");
    }
}

fn limits() -> ToolchainProbeLimits {
    ToolchainProbeLimits::new(
        Duration::from_secs(5),
        NonZeroUsize::new(16 * 1024).expect("test stream limit is nonzero"),
    )
    .expect("test timeout is nonzero")
}

#[test]
fn explicit_test_executable_yields_a_nonempty_identity() {
    let executable = std::env::current_exe().expect("test executable has an absolute path");
    let toolchain = LocalRuntimeToolchain::probe(NativeTool::GoCompiler, executable, limits())
        .expect("libtest accepts Go's positional version probe as a zero-match filter");
    assert_eq!(toolchain.tool, NativeTool::GoCompiler);
    assert!(toolchain.identity.is_some());
}

#[test]
fn relative_executable_is_rejected_before_process_entry() {
    let error = LocalRuntimeToolchain::probe(NativeTool::GoCompiler, PathBuf::from("go"), limits())
        .expect_err("relative executable would consult ambient PATH");
    assert!(matches!(
        error,
        ToolchainProbeError::RelativeExecutable {
            tool: NativeTool::GoCompiler,
            executable,
        } if executable == PathBuf::from("go")
    ));
}

#[cfg(unix)]
#[test]
fn output_crossing_the_stream_cap_is_terminated_and_retains_exact_facts() {
    let program = ProbeProgram::new("while :; do printf '0123456789abcdef'; done");
    let limits = ToolchainProbeLimits::new(
        Duration::from_secs(2),
        NonZeroUsize::new(64).expect("test stream limit is nonzero"),
    )
    .expect("test timeout is nonzero");
    let error = LocalRuntimeToolchain::probe(
        NativeTool::TypeScriptCompiler,
        program.executable.clone(),
        limits,
    )
    .expect_err("unbounded fixture output must cross the admitted cap");
    assert!(matches!(
        error,
        ToolchainProbeError::Bounded {
            tool: NativeTool::TypeScriptCompiler,
            primary: ToolchainProbePrimary::OutputLimit {
                worker: NativeWorker::StandardOutputReader,
                observed,
                maximum: 64,
            },
        } if observed > 64
    ));
}

#[cfg(unix)]
#[test]
fn exited_probe_cannot_publish_a_truncated_stream_as_its_identity() {
    let program = ProbeProgram::new("printf '0123456789abcdef0123456789abcdef'");
    let limits = ToolchainProbeLimits::new(
        Duration::from_secs(2),
        NonZeroUsize::new(8).expect("test stream limit is nonzero"),
    )
    .expect("test timeout is nonzero");
    let error = LocalRuntimeToolchain::probe(
        NativeTool::TypeScriptCompiler,
        program.executable.clone(),
        limits,
    )
    .expect_err("a fast successful exit must not hide its output-cap crossing");
    assert!(matches!(
        error,
        ToolchainProbeError::Bounded {
            tool: NativeTool::TypeScriptCompiler,
            primary: ToolchainProbePrimary::OutputLimit {
                worker: NativeWorker::StandardOutputReader,
                observed: 32,
                maximum: 8,
            },
        }
    ));
}

#[cfg(unix)]
#[test]
fn live_probe_crossing_the_deadline_is_terminated_and_retains_the_interval() {
    let program = ProbeProgram::new("while :; do :; done");
    let timeout = Duration::from_millis(25);
    let limits = ToolchainProbeLimits::new(
        timeout,
        NonZeroUsize::new(64).expect("test stream limit is nonzero"),
    )
    .expect("test timeout is nonzero");
    let error =
        LocalRuntimeToolchain::probe(NativeTool::Python, program.executable.clone(), limits)
            .expect_err("live fixture must cross the admitted deadline");
    assert!(matches!(
        error,
        ToolchainProbeError::Bounded {
            tool: NativeTool::Python,
            primary: ToolchainProbePrimary::Deadline { timeout: observed },
        } if observed == timeout
    ));
}
