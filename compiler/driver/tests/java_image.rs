//! Exercises source-bound javac image admission through the public driver request.
//! Proves typed javac declarations feed the shared compact canonical lane.
//! Keeps Java interfaces mapped to the canonical trait vocabulary.

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
use compiler_vocabulary::{JavaRelease, LanguageProfile, Stage};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Toolchain(#[from] ToolchainResolutionError),
    #[error("configured javac authority image did not compile through direct admission")]
    Compile,
    #[error("validated entity atom coordinate could not fit this platform")]
    AtomCoordinate(#[source] std::num::TryFromIntError),
    #[error("validated entity referenced a missing compact atom")]
    MissingAtom,
    #[error("javac interface declaration was not retained as a canonical trait")]
    Interface,
}

#[test]
fn configured_javac_image_admits_source_bound_interface() -> Result<(), TestError> {
    let source = b"public interface Shape {}\n";
    let image = fixture(source);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/usr/bin/true"),
        b"configured-javac-authority-image",
    )?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [];
    let mut output = [0; 65_536];
    let compiled = match compile(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Java { image: &image },
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
    const INNER_HEADER: usize = 176;
    const OUTER_HEADER: usize = 80;
    const DIRECTORY: usize = 48;
    const DIRECTORY_ENTRY: usize = 16;
    const INNER_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v1\0";
    const OUTER_DOMAIN: &[u8] = b"nudox.java.bound.authority.image.sha256.v1\0";
    const NAME: &[u8] = b"Shape";
    let sections = [
        atom_directory(NAME.len()),
        NAME.to_vec(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        declaration_row(),
        Vec::new(),
    ];
    let mut inner = vec![0; INNER_HEADER];
    inner[..4].copy_from_slice(b"NJAI");
    inner[4..6].copy_from_slice(&1_u16.to_le_bytes());
    inner[6..8].copy_from_slice(&176_u16.to_le_bytes());
    inner[8..10].copy_from_slice(&21_u16.to_le_bytes());
    inner[10..12].copy_from_slice(&8_u16.to_le_bytes());
    inner[12..16].copy_from_slice(&(45_u32).to_le_bytes());
    let row_bytes = [8_u16, 1, 16, 4, 16, 4, 32, 20];
    let mut offset = INNER_HEADER;
    for (index, section) in sections.iter().enumerate() {
        let entry = DIRECTORY + index * DIRECTORY_ENTRY;
        inner[entry..entry + 2].copy_from_slice(&((index as u16 + 1).to_le_bytes()));
        inner[entry + 2..entry + 4].copy_from_slice(&row_bytes[index].to_le_bytes());
        let count = section.len() / usize::from(row_bytes[index]);
        inner[entry + 4..entry + 8].copy_from_slice(&(count as u32).to_le_bytes());
        inner[entry + 8..entry + 12].copy_from_slice(&(offset as u32).to_le_bytes());
        inner[entry + 12..entry + 16].copy_from_slice(&(section.len() as u32).to_le_bytes());
        offset += section.len();
    }
    for section in sections {
        inner.extend_from_slice(&section);
    }
    let mut inner_digest = Sha256::new();
    inner_digest.update(INNER_DOMAIN);
    inner_digest.update(&inner[..16]);
    inner_digest.update(&inner[48..INNER_HEADER]);
    inner_digest.update(&inner[INNER_HEADER..]);
    inner[16..48].copy_from_slice(inner_digest.finalize().as_slice());
    let mut outer = vec![0; OUTER_HEADER + inner.len()];
    outer[..4].copy_from_slice(b"NJAB");
    outer[4..6].copy_from_slice(&1_u16.to_le_bytes());
    outer[6..8].copy_from_slice(&80_u16.to_le_bytes());
    outer[8..12].copy_from_slice(&(inner.len() as u32).to_le_bytes());
    outer[12..44].copy_from_slice(Sha256::digest(source).as_slice());
    outer[OUTER_HEADER..].copy_from_slice(&inner);
    let mut outer_digest = Sha256::new();
    outer_digest.update(OUTER_DOMAIN);
    outer_digest.update(&outer[..44]);
    outer_digest.update(&outer[76..OUTER_HEADER]);
    outer_digest.update(&outer[OUTER_HEADER..]);
    outer[44..76].copy_from_slice(outer_digest.finalize().as_slice());
    outer
}

fn atom_directory(name_bytes: usize) -> Vec<u8> {
    let mut atoms = vec![0; 8];
    atoms[4..8].copy_from_slice(&(name_bytes as u32).to_le_bytes());
    atoms
}

fn declaration_row() -> Vec<u8> {
    let mut declaration = vec![0; 32];
    declaration[0] = 4;
    declaration[1] = 1;
    declaration[8..12].copy_from_slice(&0_u32.to_le_bytes());
    declaration[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    declaration[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    declaration[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    declaration[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
    declaration[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
    declaration
}
