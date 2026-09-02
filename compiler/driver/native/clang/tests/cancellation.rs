//! Exercises the direct Clang frontend's cancellation and deadline terminals through its observable boundary.
//! The cases prove typed rejection before any semantic result becomes visible.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use super::common::{Fixture, TestError, real_clang, require_linked_library};
use super::{ClangError, ClangSourceLanguage, MAX_ANALYSIS_SCRATCH_BYTES};

#[test]
fn pre_cancelled_direct_analysis_emits_no_facts() -> Result<(), TestError> {
    require_linked_library()?;
    let _tool = real_clang()?;
    let source = b"struct CancelledLinked { int value; };\n";
    let fixture = Fixture::new("c")?;
    let cancellation = AtomicBool::new(false);
    cancellation.store(true, Ordering::Release);
    let mut identity = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut facts = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut emitted = Vec::new();
    let error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        None,
        Some(&cancellation),
        None,
        super::AnalysisScratch {
            identity: &mut identity,
            facts: &mut facts,
        },
        |fact| emitted.push(fact),
    )
    .expect_err("a pre-cancelled analysis must be rejected");
    assert!(matches!(error, ClangError::Cancelled { .. }));
    assert!(emitted.is_empty());
    Ok(())
}

#[test]
fn expired_deadline_is_a_typed_terminal_before_native_work() -> Result<(), TestError> {
    require_linked_library()?;
    let _tool = real_clang()?;
    let source = b"struct Deadline { int value; };\n";
    let fixture = Fixture::new("c")?;
    let mut identity = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut facts = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
    let mut emitted = Vec::new();
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or_else(|| TestError("clock could not step back".to_owned()))?;
    let error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        None,
        None,
        Some(expired),
        super::AnalysisScratch {
            identity: &mut identity,
            facts: &mut facts,
        },
        |fact| emitted.push(fact),
    )
    .expect_err("an expired deadline must be rejected");
    assert!(matches!(error, ClangError::DeadlineExceeded { .. }));
    assert!(emitted.is_empty());
    Ok(())
}
