//! Bounded native child proofs against the private native-process sidecar.
//!
//! Since `0f8f0120f`/`8ded4315e` the public `compile` entry admits semantic
//! authority first and never spawns a native syntax pass (see the driver
//! README), so these hostile-helper proofs can no longer reach the child
//! through `compile`. They drive [`parse_with_native_tool`] directly with the
//! same Rust recipe and the same hostile scripts, keeping every terminal and
//! invariant the public-boundary versions asserted.
//!
//! The helpers are `/bin/sh` scripts made executable through Unix mode bits,
//! so the module is Unix-only.

use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
};

use super::parse_with_native_tool;
use crate::driver::types::{
    CompileControl, CompileFailure, CompileScratch, NativeRecipe, NativeWorkError,
    NativeWorkPrimary, ResolvedToolchain, SourceIdentity, SourceLease,
};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn unique(prefix: &str) -> PathBuf {
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "{prefix}-{}-{sequence}",
        std::process::id()
    ))
}

struct Script(PathBuf);

impl Drop for Script {
    fn drop(&mut self) {
        let _removed = fs::remove_file(&self.0);
    }
}

fn script(body: &[u8]) -> Script {
    let path = unique("native-bounded-helper").with_extension("sh");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create hostile helper");
    file.write_all(body).expect("write hostile helper");
    file.set_permissions(fs::Permissions::from_mode(0o700))
        .expect("make hostile helper executable");
    Script(path)
}

struct Work(PathBuf);

impl Work {
    fn create() -> Self {
        let path = unique("native-bounded-work");
        fs::create_dir(&path).expect("create native work");
        Self(path)
    }

    fn assert_empty(&self) {
        assert!(
            fs::read_dir(&self.0)
                .expect("inspect native work")
                .next()
                .is_none(),
            "native work was not left empty"
        );
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

fn run<'diagnostic>(
    executable: &Path,
    version: &[u8],
    source: &[u8],
    cancelled: &AtomicBool,
    deadline: Instant,
    diagnostic_output: &'diagnostic mut [u8],
    native_work: &Path,
) -> Result<(), CompileFailure<'diagnostic>> {
    let toolchain = ResolvedToolchain::from_version(NativeTool::Rustc, executable, version)
        .expect("resolve hostile helper toolchain");
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    let identity: SourceIdentity = SourceLease::enter(source)
        .expect("source fits the lease")
        .identity();
    let recipe_fact = CompileRecipeFact::derive(
        profile,
        Stage::LowerIr,
        toolchain.tool,
        identity.identity,
        toolchain.identity,
    );
    parse_with_native_tool(
        NativeRecipe {
            profile,
            stage: Stage::LowerIr,
            source,
            toolchain,
        },
        identity,
        recipe_fact,
        CompileScratch {
            diagnostic_output,
            native_work,
        },
        CompileControl {
            deadline,
            cancelled,
        },
    )
}

#[test]
fn nonreading_never_exit_tool_is_killed_without_blocking_the_deadline_owner() {
    let helper = script(b"#!/bin/sh\nwhile :; do :; done\n");
    let work = Work::create();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 128];
    let result = run(
        &helper.0,
        b"nonreading-fixture",
        &[b'x'; 131_072],
        &cancelled,
        Instant::now() + Duration::from_millis(300),
        &mut diagnostic,
        &work.0,
    );
    assert!(
        matches!(result, Err(CompileFailure::DeadlineExceeded { .. })),
        "expected DeadlineExceeded, observed {result:?}"
    );
    work.assert_empty();
}

#[test]
fn stdin_closed_then_never_exit_retains_the_input_cause_and_reaps_the_child() {
    let helper = script(b"#!/bin/sh\nexec 0<&-\nwhile :; do :; done\n");
    let work = Work::create();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 128];
    let result = run(
        &helper.0,
        b"stdin-closed-fixture",
        &[b'x'; 131_072],
        &cancelled,
        Instant::now() + Duration::from_secs(1),
        &mut diagnostic,
        &work.0,
    );
    assert!(
        matches!(result, Err(CompileFailure::ToolInput { .. })),
        "expected ToolInput, observed {result:?}"
    );
    work.assert_empty();
}

#[test]
fn pre_cancelled_request_never_starts_the_marker_tool() {
    let helper = script(b"#!/bin/sh\nprintf x > started\nwhile :; do :; done\n");
    let work = Work::create();
    let cancelled = AtomicBool::new(true);
    let mut diagnostic = [0; 128];
    let result = run(
        &helper.0,
        b"pre-cancelled-fixture",
        b"pub const alpha: bool = true;",
        &cancelled,
        Instant::now() + Duration::from_secs(1),
        &mut diagnostic,
        &work.0,
    );
    assert!(
        matches!(
            &result,
            Err(CompileFailure::Cancelled { diagnostic, .. }) if diagnostic.bytes.is_empty()
        ),
        "expected Cancelled with an empty diagnostic, observed {result:?}"
    );
    // The marker file would exist had the helper started.
    work.assert_empty();
}

#[test]
fn cleanup_failure_retains_the_exact_native_rejection_terminal() {
    let helper = script(
        b"#!/bin/sh\nIFS= read -r ignored\nprintf x > foreign\nprintf rejected >&2\nexit 1\n",
    );
    let work = Work::create();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 128];
    let result = run(
        &helper.0,
        b"cleanup-failure-fixture",
        b"fixture\n",
        &cancelled,
        Instant::now() + Duration::from_secs(1),
        &mut diagnostic,
        &work.0,
    );
    assert!(
        matches!(
            &result,
            Err(CompileFailure::NativeWorkCleanup {
                primary: NativeWorkPrimary::NativeRejected { diagnostic, .. },
                cleanup: NativeWorkError::NotEmpty,
                ..
            }) if diagnostic.bytes == b"rejected"
        ),
        "expected NativeRejected compounded with a NotEmpty cleanup, observed {result:?}"
    );
    fs::remove_file(work.0.join("foreign")).expect("remove the exact hostile artifact");
    work.assert_empty();
}

#[test]
fn noisy_tool_exceeds_the_bounded_diagnostic_lease_before_a_compile_terminal() {
    let helper =
        script(b"#!/bin/sh\nwhile :; do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' >&2; done\n");
    let work = Work::create();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 32];
    let result = run(
        &helper.0,
        b"noisy-fixture",
        b"pub const alpha: bool = true;",
        &cancelled,
        Instant::now() + Duration::from_secs(1),
        &mut diagnostic,
        &work.0,
    );
    match result {
        Err(CompileFailure::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
            ..
        }) => {
            assert_eq!(limit, 32);
            assert!(observed > limit, "observed {observed} <= limit {limit}");
            assert_eq!(diagnostic.bytes.len(), limit);
            assert!(diagnostic.truncated);
        }
        other => panic!("expected DiagnosticLimit, observed {other:?}"),
    }
    work.assert_empty();
}
