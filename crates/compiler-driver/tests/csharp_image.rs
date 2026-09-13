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
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
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
    // One version-3 authority image: the interface declaration `Shape` with
    // its recursive nominal type row, canonical empty sections elsewhere.
    const HEADER: usize = 256;
    const DIRECTORY: usize = 48;
    const ENTRY: usize = 16;
    const DIGEST_AT: usize = 224;
    const DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v3\0";
    const ABSENT: u32 = u32::MAX;
    let name: &[u8] = b"Shape";
    let qualified: &[u8] = b"demo.Shape";
    let (at, _) = source
        .windows(name.len())
        .position(|window| window == name)
        .map_or((0_usize, 0_usize), |at| (at, at + name.len()));
    let (name_start, name_end) = (
        u32::try_from(at).unwrap_or(u32::MAX),
        u32::try_from(at + name.len()).unwrap_or(u32::MAX),
    );

    let mut atoms = Vec::new();
    let mut atom_bytes = Vec::new();
    for text in [&name[..], qualified, qualified] {
        atoms.extend_from_slice(
            &u32::try_from(atom_bytes.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        atoms.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
        atom_bytes.extend_from_slice(text);
    }
    let name_atom = 0_u32;
    let qualified_atom = 1_u32;
    let spelling_atom = 2_u32;

    // Declarations: one interface row (kind 3) typed by its own type row.
    let mut declarations = Vec::new();
    declarations.push(3_u8);
    declarations.push(0_u8);
    declarations.push(0_u8);
    declarations.push(0_u8);
    declarations.extend_from_slice(&name_atom.to_le_bytes());
    declarations.extend_from_slice(&qualified_atom.to_le_bytes());
    declarations.extend_from_slice(&ABSENT.to_le_bytes());
    declarations.extend_from_slice(&0_u32.to_le_bytes());
    declarations.extend_from_slice(&name_start.to_le_bytes());
    declarations.extend_from_slice(&name_start.to_le_bytes());
    declarations.extend_from_slice(&name_end.to_le_bytes());
    declarations.extend_from_slice(&0_u32.to_le_bytes());
    declarations.extend_from_slice(&0_u16.to_le_bytes());
    declarations.extend_from_slice(&0_u32.to_le_bytes());
    declarations.extend_from_slice(&0_u16.to_le_bytes());
    declarations.extend_from_slice(&ABSENT.to_le_bytes());

    // Types: one named row over the recursive spelling, no children.
    let mut types = Vec::new();
    types.push(1_u8);
    types.push(2_u8);
    types.push(0_u8);
    types.push(0_u8);
    types.extend_from_slice(&spelling_atom.to_le_bytes());
    types.extend_from_slice(&0_u32.to_le_bytes());
    types.extend_from_slice(&0_u32.to_le_bytes());

    let sections = [atoms, atom_bytes, declarations, types];
    // Canonical body order: atoms, atom bytes, declarations, parameters,
    // type parameters, type constraints, types, type children, attributes,
    // docs, references. Only four sections carry rows here.
    let populated = [0_usize, 1, 2, 6];
    let all_row_bytes = [8_u16, 1, 48, 24, 12, 4, 16, 8, 8, 20, 28];
    let mut image = vec![0_u8; HEADER];
    image[..4].copy_from_slice(b"NCAI");
    image[4..6].copy_from_slice(&3_u16.to_le_bytes());
    image[6..8].copy_from_slice(&u16::try_from(HEADER).unwrap_or(u16::MAX).to_le_bytes());
    let body: usize = sections.iter().map(Vec::len).sum();
    image[8..12].copy_from_slice(&u32::try_from(body).unwrap_or(u32::MAX).to_le_bytes());
    image[12..44].copy_from_slice(Sha256::digest(source).as_slice());
    image[44..46].copy_from_slice(&11_u16.to_le_bytes());
    let mut offset = HEADER;
    for (index, row_width) in all_row_bytes.iter().enumerate() {
        let entry = DIRECTORY + index * ENTRY;
        image[entry..entry + 2]
            .copy_from_slice(&u16::try_from(index + 1).unwrap_or(u16::MAX).to_le_bytes());
        image[entry + 2..entry + 4].copy_from_slice(row_width.to_le_bytes().as_slice());
        match populated.iter().position(|at| *at == index) {
            Some(at) => {
                let section = &sections[at];
                let count = section.len() / usize::from(*row_width);
                image[entry + 4..entry + 8]
                    .copy_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
                image[entry + 12..entry + 16].copy_from_slice(
                    &u32::try_from(section.len())
                        .unwrap_or(u32::MAX)
                        .to_le_bytes(),
                );
            }
            None => {
                image[entry + 4..entry + 8].copy_from_slice(&0_u32.to_le_bytes());
                image[entry + 12..entry + 16].copy_from_slice(&0_u32.to_le_bytes());
            }
        }
        image[entry + 8..entry + 12]
            .copy_from_slice(&u32::try_from(offset).unwrap_or(u32::MAX).to_le_bytes());
        if let Some(at) = populated.iter().position(|candidate| *candidate == index) {
            offset += sections[at].len();
        }
    }
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(&image[..DIGEST_AT]);
    for section in &sections {
        digest.update(section);
    }
    image[DIGEST_AT..HEADER].copy_from_slice(&digest.finalize().as_slice());
    let mut complete = image;
    for section in &sections {
        complete.extend_from_slice(section);
    }
    complete
}
