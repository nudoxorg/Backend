//! Chief-owned public compiler and publication falsifiers.

#[path = "support/native_tooling.rs"]
mod native_tooling;

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use nudox_compile_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledFragment, NativeTool,
    ResolvedToolchain, ToolchainSelection, compile,
};
use nudox_compile_vocab::{Language, Stage};
use nudox_ir_format::{EntityKind, PrimitiveType, TypeNode};
use thiserror::Error;

use native_tooling::{HostTool, NativeToolingError, NativeWork};

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Tooling(#[from] NativeToolingError),
    #[error("the public Rust compilation unexpectedly failed")]
    Compile,
}

#[test]
fn distinct_equal_shape_declarations_produce_distinct_semantic_ir() -> Result<(), TestFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1    ;";
    assert_eq!(alpha_source.len(), bravo_source.len());

    let host = HostTool::resolve("rustc", NativeTool::Rustc)?;
    let toolchain = host.toolchain()?;
    let work = NativeWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut alpha_diagnostic = [0; 4_096];
    let mut bravo_diagnostic = [0; 4_096];
    let mut alpha_output = [0; 512];
    let mut bravo_output = [0; 512];
    let alpha = compile(
        request(alpha_source, toolchain, &cancelled),
        CompileScratch {
            diagnostic_output: &mut alpha_diagnostic,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut alpha_output,
        },
    )
    .map_err(|_source| TestFailure::Compile)?;
    work.assert_empty()?;
    let bravo = compile(
        request(bravo_source, toolchain, &cancelled),
        CompileScratch {
            diagnostic_output: &mut bravo_diagnostic,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut bravo_output,
        },
    )
    .map_err(|_source| TestFailure::Compile)?;
    work.assert_empty()?;

    assert_ne!(alpha.source.identity, bravo.source.identity);
    assert_ne!(alpha.fragment.as_ref(), bravo.fragment.as_ref());
    assert_fragment(&alpha, b"alpha", PrimitiveType::Bool);
    assert_fragment(&bravo, b"bravo", PrimitiveType::I32);
    Ok(())
}

fn assert_fragment(
    compiled: &CompiledFragment<'_>,
    expected_name: &[u8],
    expected_type: PrimitiveType,
) {
    assert!(
        compiled
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(EntityKind::Constant, 0)])
    );
    assert!(
        compiled
            .fragment
            .atoms()
            .map(|atom| atom.bytes)
            .eq([expected_name])
    );
    assert!(
        compiled
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(expected_type)])
    );
}

fn request<'source, 'toolchain, 'cancel>(
    source: &'source [u8],
    toolchain: ResolvedToolchain<'toolchain>,
    cancelled: &'cancel AtomicBool,
) -> CompileRequest<'source, 'toolchain, 'cancel> {
    CompileRequest {
        language: Language::Rust,
        stage: Stage::LowerIr,
        source,
        toolchain: ToolchainSelection::ResolvedNative(toolchain),
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled,
        },
    }
}
