//! Byte-exact rendering over the committed C# Roslyn authority fixtures.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod csharp_support;

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{FragmentView, Ir, ItemKind};
use compiler_vocabulary::{CSharpVersion, LanguageProfile, Stage};
use std::{
    fs,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
const FIDELITY_SOURCE: &[u8] =
    include_bytes!("../../languages/csharp/tests/fixtures/producer/fidelity.cs");
const FIDELITY_IMAGE: &[u8] =
    include_bytes!("../../languages/csharp/tests/fixtures/producer/fidelity.ncaimg");
const UNICODE_SOURCE: &[u8] =
    include_bytes!("../../languages/csharp/tests/fixtures/producer/unicode.cs");
const UNICODE_IMAGE: &[u8] =
    include_bytes!("../../languages/csharp/tests/fixtures/producer/unicode.ncaimg");
const FIDELITY_GOLDEN: &str = include_str!("fixtures/csharp_render/fidelity.txt");
const UNICODE_GOLDEN: &str = include_str!("fixtures/csharp_render/unicode.txt");

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(#[from] csharp_support::Error),
    #[error("I/O failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("toolchain resolution failed: {source}")]
    Toolchain {
        #[source]
        source: compiler_driver::ToolchainResolutionError,
    },
    #[error("compile failed: {cause}")]
    Compile { cause: String },
    #[error("fragment validation failed: {source}")]
    Fragment {
        #[source]
        source: compiler_ir::FragmentError,
    },
    #[error("missing {kind:?} {name}")]
    Missing { name: &'static str, kind: ItemKind },
    #[error("{name} differs: expected {expected:?}, actual {actual:?}")]
    Mismatch {
        name: &'static str,
        expected: String,
        actual: String,
    },
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

fn toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let path = csharp_support::dotnet_executable()?;
    let path = Box::leak(path.canonicalize().map_err(io)?.into_boxed_path());
    let output = std::process::Command::new(&*path)
        .arg("--version")
        .output()
        .map_err(io)?;
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::CSharpCompiler, path, version)
        .map_err(|source| TestError::Toolchain { source })
}

fn compile_fixture(
    source: &'static [u8],
    image: &'static [u8],
    label: &str,
) -> Result<Ir, TestError> {
    let work = csharp_support::fresh_dir(label)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let result = compile_ir(
        CompileRequest {
            profile: PROFILE,
            stage: Stage::LowerIr,
            source,
            toolchain: ToolchainSelection::ResolvedNative(toolchain()?),
            authority: SemanticAuthorityInput::CSharp { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
    )
    .map(|compiled| compiled.ir)
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    });
    let cleanup = fs::remove_dir_all(work).map_err(io);
    cleanup?;
    result
}

fn compile_fragment(
    source: &'static [u8],
    image: &'static [u8],
    label: &str,
) -> Result<Vec<u8>, TestError> {
    let work = csharp_support::fresh_dir(label)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let result = compile(
        CompileRequest {
            profile: PROFILE,
            stage: Stage::LowerIr,
            source,
            toolchain: ToolchainSelection::ResolvedNative(toolchain()?),
            authority: SemanticAuthorityInput::CSharp { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map(|fragment| fragment.fragment.as_ref().to_vec())
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    });
    fs::remove_dir_all(work).map_err(io)?;
    result
}

fn render(
    ir: &Ir,
    id: compiler_ir::EntityId,
    name: &'static str,
    kind: ItemKind,
) -> Result<String, TestError> {
    let signature = ir.signature(id).ok_or(TestError::Missing { name, kind })?;
    let docs = ir
        .display_docs(id)
        .ok_or(TestError::Missing { name, kind })?;
    let mut text = format!("{}\n", signature);
    if !docs.to_string().is_empty() {
        text.push_str("docs: ");
        text.push_str(&docs.to_string());
        text.push('\n');
    }
    if let Some(ty) = ir.item(id).and_then(|item| item.semantic_type()) {
        text.push_str("type: ");
        text.push_str(
            &ir.display_type(ty)
                .ok_or(TestError::Missing { name, kind })?
                .to_string(),
        );
        text.push('\n');
    }
    Ok(text)
}

fn golden(ir: &Ir, expected: &str, name: &'static str) -> Result<(), TestError> {
    let mut actual = String::new();
    for item in ir.items() {
        actual.push_str(&render(ir, item.id(), name, item.kind())?);
    }
    if actual == expected {
        Ok(())
    } else {
        Err(TestError::Mismatch {
            name,
            expected: expected.to_owned(),
            actual,
        })
    }
}

#[test]
fn fidelity_render_is_byte_exact_deterministic_and_old_fragment_validates() -> Result<(), TestError>
{
    let first = compile_fixture(FIDELITY_SOURCE, FIDELITY_IMAGE, "render-fidelity")?;
    let second = compile_fixture(FIDELITY_SOURCE, FIDELITY_IMAGE, "render-fidelity-second")?;
    golden(&first, FIDELITY_GOLDEN, "fidelity")?;
    golden(&second, FIDELITY_GOLDEN, "fidelity-second")?;
    let old = compile_fragment(FIDELITY_SOURCE, FIDELITY_IMAGE, "render-fidelity-fragment")?;
    FragmentView::validate(&old).map_err(|source| TestError::Fragment { source })?;
    Ok(())
}

#[test]
fn unicode_render_is_byte_exact_and_old_fragment_validates() -> Result<(), TestError> {
    let ir = compile_fixture(UNICODE_SOURCE, UNICODE_IMAGE, "render-unicode")?;
    golden(&ir, UNICODE_GOLDEN, "unicode")?;
    let old = compile_fragment(UNICODE_SOURCE, UNICODE_IMAGE, "render-unicode-fragment")?;
    FragmentView::validate(&old).map_err(|source| TestError::Fragment { source })?;
    Ok(())
}
