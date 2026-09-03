//! Exercises Go binary-authority admission through the public compiler request.
//! Proves configured image facts bypass retired native scanner dispatch.
//! Validates the compact artifact written from the same shared fact lane.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainResolutionError, ToolchainSelection, compile,
};
use compiler_ir::EntityKind;
use compiler_vocabulary::{GoVersion, LanguageProfile, Stage};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Toolchain(#[from] ToolchainResolutionError),
    #[error("configured Go authority image did not compile through the direct admission lane")]
    Compile,
    #[error("validated entity atom coordinate could not fit this platform")]
    AtomCoordinate(#[source] std::num::TryFromIntError),
    #[error("validated entity referenced a missing compact atom")]
    MissingAtom,
    #[error("configured Go authority declaration was absent after compact admission")]
    Declaration,
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
        Err(_) => return Err(TestError::Compile),
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

/// Builds one minimal but fully valid format-v5 Go authority image: an
/// empty module row, one package row owning the single `Brew` declaration,
/// and no other populated plane. The source digest binds the fixture bytes.
fn fixture(source: &[u8]) -> Vec<u8> {
    const HEADER: usize = 132;
    const MODULE_AT: usize = HEADER;
    const PACKAGE_AT: usize = MODULE_AT + 32;
    const DECL_AT: usize = PACKAGE_AT + 36;
    const ATOMS_AT: usize = DECL_AT + 56;
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
    image[116..120].copy_from_slice(&1_u32.to_le_bytes()); // module row
    image[120..124].copy_from_slice(&1_u32.to_le_bytes()); // package row

    // Package row: import path "demo", clause "demo", no files, run 0..1.
    image[PACKAGE_AT..PACKAGE_AT + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[PACKAGE_AT + 4..PACKAGE_AT + 8].copy_from_slice(&4_u32.to_le_bytes());
    image[PACKAGE_AT + 12..PACKAGE_AT + 16].copy_from_slice(&4_u32.to_le_bytes());
    image[PACKAGE_AT + 28..PACKAGE_AT + 32].copy_from_slice(&0_u32.to_le_bytes());
    image[PACKAGE_AT + 32..PACKAGE_AT + 36].copy_from_slice(&1_u32.to_le_bytes());

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
