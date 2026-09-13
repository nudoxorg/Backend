//! Exercises Go binary-authority admission through the public compiler request.
//! Proves configured image facts bypass retired native scanner dispatch.
//! Validates the compact artifact written from the same shared fact lane.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainResolutionError, ToolchainSelection,
    compile, compile_semantic,
};
use compiler_ir::EntityKind;
use compiler_vocabulary::{GoVersion, LanguageProfile, Stage};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Toolchain(#[from] ToolchainResolutionError),
    #[error(
        "configured Go authority image did not compile through the direct admission lane: {cause}"
    )]
    Compile { cause: String },
    #[error("validated entity atom coordinate could not fit this platform")]
    AtomCoordinate(#[source] std::num::TryFromIntError),
    #[error("validated entity referenced a missing compact atom")]
    MissingAtom,
    #[error("configured Go authority declaration was absent after compact admission")]
    Declaration,
    #[error("fused artifact source or recipe differed from its validated fragment")]
    Binding,
    #[error("fused owned image, capture, and compact census did not agree")]
    FusedCensus,
    #[error("fused fragment modified the caller output tail")]
    OutputTail,
    #[error("pre-entry cancellation did not retain the exact cancellation terminal")]
    Cancelled,
}

#[test]
fn configured_go_authority_image_admits_without_native_scanner_dispatch() -> Result<(), TestError> {
    let source = b"package demo\nfunc Brew() {}\n";
    let image = fixture(source);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/usr/bin/true"),
        b"configured-go-authority-image",
    )?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [];
    let mut output = [0; 65_536];
    let compiled = match compile(
        CompileRequest {
            profile: LanguageProfile::Go(GoVersion::Go125),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Go { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/private/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Ok(compiled) => compiled,
        Err(failure) => {
            return Err(TestError::Compile {
                cause: format!("{failure:?}"),
            });
        }
    };
    for entity in compiled.fragment.entities() {
        if entity.kind != EntityKind::Function {
            continue;
        }
        let atom = usize::try_from(entity.name.raw).map_err(TestError::AtomCoordinate)?;
        let Some(name) = compiled.fragment.atoms().nth(atom) else {
            return Err(TestError::MissingAtom);
        };
        if name.bytes == b"Brew" {
            return Ok(());
        }
    }
    Err(TestError::Declaration)
}

/// The fused entrypoint has one structural authority dispatch: this fixed
/// binary image has no counting hook, so the absence of a second transaction
/// is proved by the shared private transaction rather than global test state.
#[test]
fn fused_go_authority_result_binds_owned_and_compact_truth_once() -> Result<(), TestError> {
    let source = b"package demo\nfunc Brew() {}\n";
    let image = fixture(source);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/usr/bin/true"),
        b"configured-go-authority-image",
    )?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [];
    let mut output = [0xa5; 65_536];
    let compiled = compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Go(GoVersion::Go125),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Go { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/private/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    })?;

    if compiled.artifact.source != compiled.artifact.fragment.source
        || compiled.artifact.recipe != compiled.artifact.fragment.recipe
    {
        return Err(TestError::Binding);
    }
    match compiled.ir.image_provenance() {
        compiler_ir::ImageProvenance::Captured { source, recipe, .. }
            if source == compiled.artifact.source && recipe == compiled.artifact.recipe => {}
        _ => return Err(TestError::Binding),
    }
    let census = compiled.artifact.fragment.discover().census();
    if census.entities != u32::try_from(compiled.ir.items().len()).unwrap_or(u32::MAX)
        || census.entities != 1
        || census.canonical_entity_roots != 1
        || compiled.ir.entity_authority_columns().row_count() != compiled.ir.items().len()
        || compiled.ir.entity_authority_columns().semantic_type.first()
            != Some(&compiler_ir::FactAvailability::Captured)
        || !compiled.ir.items().any(|item| item.name() == b"Brew")
    {
        return Err(TestError::FusedCensus);
    }
    let length = compiled.artifact.fragment.as_ref().len();
    // The returned view borrows the entire output lease. Drop it before
    // inspecting the untouched suffix through the caller's original buffer.
    drop(compiled);
    if !output[length..].iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::OutputTail);
    }
    Ok(())
}

#[test]
fn fused_go_authority_preentry_cancellation_exposes_no_result_or_output() -> Result<(), TestError> {
    let source = b"package demo\nfunc Brew() {}\n";
    let image = fixture(source);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/usr/bin/true"),
        b"configured-go-authority-image",
    )?;
    let cancelled = AtomicBool::new(true);
    let mut diagnostic = [];
    let mut output = [0xa5; 65_536];
    let result = compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Go(GoVersion::Go125),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Go { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/private/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    );
    match result {
        Err(CompileFailure::Cancelled { diagnostic, .. })
            if diagnostic.bytes.is_empty() && diagnostic.observed == 0 && !diagnostic.truncated => {
        }
        _ => return Err(TestError::Cancelled),
    }
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::OutputTail);
    }
    Ok(())
}

/// Builds one minimal but fully valid format-v5 Go authority image: a single
/// `Brew` declaration, one package row naming it, and no other populated
/// plane. The body planes follow the frozen order — declarations first, then
/// packages right after the empty satisfaction plane; the module row is
/// absent (header count 0) because the fixture resolves no module. The
/// source digest binds the fixture bytes.
fn fixture(source: &[u8]) -> Vec<u8> {
    const HEADER: usize = 136;
    const DECL_AT: usize = HEADER;
    const PACKAGE_AT: usize = DECL_AT + 56;
    const ATOMS_AT: usize = PACKAGE_AT + 28;
    const BODY: usize = ATOMS_AT + 8 - HEADER;
    const DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v5\x00";
    const NAME: &[u8] = b"demoBrew";
    const NONE: u32 = u32::MAX;

    let mut image = vec![0_u8; HEADER + BODY];
    image[..4].copy_from_slice(b"NGAI");
    image[4..6].copy_from_slice(&5_u16.to_le_bytes());
    image[6..8].copy_from_slice(&(HEADER as u16).to_le_bytes());
    image[8..12].copy_from_slice(&1_u32.to_le_bytes()); // declarations
    image[12..16].copy_from_slice(&(NAME.len() as u32).to_le_bytes());
    image[16..20].copy_from_slice(&(BODY as u32).to_le_bytes());
    image[20..52].copy_from_slice(Sha256::digest(source).as_slice());
    image[116..120].copy_from_slice(&0_u32.to_le_bytes()); // no module row
    image[120..124].copy_from_slice(&1_u32.to_le_bytes()); // package row

    // Package row: import path "demo", clause "demo", no files.
    image[PACKAGE_AT..PACKAGE_AT + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[PACKAGE_AT + 4..PACKAGE_AT + 8].copy_from_slice(&4_u32.to_le_bytes());
    image[PACKAGE_AT + 8..PACKAGE_AT + 12].copy_from_slice(&0_u32.to_le_bytes());
    image[PACKAGE_AT + 12..PACKAGE_AT + 16].copy_from_slice(&4_u32.to_le_bytes());
    image[PACKAGE_AT + 24..PACKAGE_AT + 28].copy_from_slice(&0_u32.to_le_bytes());

    // Declaration row: exported func Brew in demo, no typed root.
    image[DECL_AT] = 3;
    image[DECL_AT + 1] = 1;
    image[DECL_AT + 4..DECL_AT + 8].copy_from_slice(&4_u32.to_le_bytes());
    image[DECL_AT + 8..DECL_AT + 12].copy_from_slice(&4_u32.to_le_bytes());
    image[DECL_AT + 16..DECL_AT + 20].copy_from_slice(&4_u32.to_le_bytes());
    image[DECL_AT + 20..DECL_AT + 24].copy_from_slice(&NONE.to_le_bytes());
    image[DECL_AT + 24..DECL_AT + 28].copy_from_slice(&NONE.to_le_bytes());
    image[DECL_AT + 28..DECL_AT + 32].copy_from_slice(&NONE.to_le_bytes());

    image[ATOMS_AT..].copy_from_slice(NAME);
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(&image[..52]);
    digest.update(&image[84..HEADER]);
    digest.update(&image[HEADER..]);
    image[52..84].copy_from_slice(digest.finalize().as_slice());
    image
}
