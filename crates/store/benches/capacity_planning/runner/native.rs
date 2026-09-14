//! Measures `backend-store` benches capacity-planning runner native work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Native compiler-to-compact-IR phase over the bounded corpus fixture.

use std::{sync::atomic::AtomicBool, time::Duration};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    ToolchainSelection, compile,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage as CompileStage};

use crate::{
    BenchmarkError,
    runner::{
        failure::compile_failure_fact,
        fixture::{Corpus, Fixture, FragmentSlots},
    },
};

const DIAGNOSTIC_BYTES: usize = 8_192;

pub(crate) fn compile_corpus(
    toolchain: &ResolvedToolchain<'_>,
    corpus: &Corpus,
    fixture: &Fixture,
    output: &mut FragmentSlots,
) -> Result<u64, BenchmarkError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; DIAGNOSTIC_BYTES];
    let mut written = 0_u64;
    for index in 0..corpus.len {
        let fragment_output = output
            .slots
            .get_mut(index)
            .ok_or(BenchmarkError::CorpusSlot {
                index,
                len: corpus.len,
            })?;
        let compiled = compile(
            CompileRequest {
                profile: LanguageProfile::Rust(RustEdition::Rust2024),
                stage: CompileStage::LowerIr,
                source: corpus.source(index)?,
                declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
                toolchain: ToolchainSelection::ResolvedNative(*toolchain),
                authority: backend_engine::driver::SemanticAuthorityInput::None,
                control: CompileControl {
                    deadline: std::time::Instant::now() + Duration::from_secs(30),
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: &fixture.native_work,
            },
            CompileOutput {
                fragment_output: &mut fragment_output.bytes,
            },
        )
        .map_err(|cause| BenchmarkError::Compile {
            slot: index,
            cause: Box::new(compile_failure_fact(cause)),
        })?;
        fragment_output.len = compiled.fragment.as_ref().len();
        written = written
            .checked_add(u64::try_from(fragment_output.len).map_err(BenchmarkError::ByteCount)?)
            .ok_or(BenchmarkError::ByteCountOverflow)?;
    }
    Ok(written)
}
