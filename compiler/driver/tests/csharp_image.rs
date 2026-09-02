//! Exercises source-bound Roslyn image admission through the public driver request.
//! Proves a configured C# authority image reaches the shared compact fact lane.
//! Keeps the declared interface kind distinct from a concrete record.

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
use compiler_vocabulary::{CSharpVersion, LanguageProfile, Stage};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Toolchain(#[from] ToolchainResolutionError),
    #[error("configured Roslyn authority image did not compile through direct admission")]
    Compile,
    #[error("validated entity atom coordinate could not fit this platform")]
    AtomCoordinate(#[source] std::num::TryFromIntError),
    #[error("validated entity referenced a missing compact atom")]
    MissingAtom,
    #[error("Roslyn interface declaration was not retained as a canonical trait")]
    Interface,
}

#[test]
fn configured_roslyn_image_admits_source_bound_interface() -> Result<(), TestError> {
    let source = b"public interface Shape {}\n";
    let image = fixture(source);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::CSharpCompiler,
        Path::new("/usr/bin/true"),
        b"configured-roslyn-authority-image",
    )?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [];
    let mut output = [0; 65_536];
    let compiled = match compile(
        CompileRequest {
            profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
            stage: Stage::LowerIr,
            source,
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::CSharp { image: &image },
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
        if entity.kind != EntityKind::Trait {
            continue;
        }
        let atom = usize::try_from(entity.name.raw).map_err(TestError::AtomCoordinate)?;
        let Some(name) = compiled.fragment.atoms().nth(atom) else {
            return Err(TestError::MissingAtom);
        };
        if name.bytes == b"Shape" {
            return Ok(());
        }
    }
    Err(TestError::Interface)
}

fn fixture(source: &[u8]) -> Vec<u8> {
    const HEADER: usize = 88;
    const ROW: usize = 20;
    const NAME: &[u8] = b"Shape";
    const DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v1\0";
    let mut image = vec![0; HEADER + ROW + NAME.len()];
    image[..4].copy_from_slice(b"NCAI");
    image[4..6].copy_from_slice(&1_u16.to_le_bytes());
    image[6..8].copy_from_slice(&88_u16.to_le_bytes());
    image[8..12].copy_from_slice(&1_u32.to_le_bytes());
    image[12..16].copy_from_slice(&5_u32.to_le_bytes());
    image[16..20].copy_from_slice(&25_u32.to_le_bytes());
    image[20..52].copy_from_slice(Sha256::digest(source).as_slice());
    image[HEADER] = 3;
    image[HEADER + 8..HEADER + 12].copy_from_slice(&5_u32.to_le_bytes());
    image[HEADER + 12..HEADER + 16].copy_from_slice(&17_u32.to_le_bytes());
    image[HEADER + 16..HEADER + 20].copy_from_slice(&22_u32.to_le_bytes());
    image[HEADER + ROW..].copy_from_slice(NAME);
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(&image[..52]);
    digest.update(&image[84..HEADER]);
    digest.update(&image[HEADER..]);
    image[52..84].copy_from_slice(digest.finalize().as_slice());
    image
}
