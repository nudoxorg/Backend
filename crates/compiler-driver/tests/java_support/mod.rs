use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_semantic::ir::Ir;
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, Stage};
use sha2::{Digest, Sha256};

const NONE: u32 = u32::MAX;

pub struct ImageBuilder {
    atoms: Vec<Vec<u8>>,
    types: Vec<[u8; 16]>,
    type_children: Vec<u32>,
    symbols: Vec<[u8; 16]>,
    symbol_parameters: Vec<u32>,
    declarations: Vec<[u8; 32]>,
}

impl ImageBuilder {
    pub fn new() -> Self {
        Self {
            atoms: Vec::new(),
            types: Vec::new(),
            type_children: Vec::new(),
            symbols: Vec::new(),
            symbol_parameters: Vec::new(),
            declarations: Vec::new(),
        }
    }
    pub fn atom(&mut self, text: &'static [u8]) -> u32 {
        let id = self.atoms.len() as u32;
        self.atoms.push(text.to_vec());
        id
    }
    pub fn ty(&mut self, kind: u8, spelling: Option<u32>, flags: u8, children: &[u32]) -> u32 {
        let start = self.type_children.len() as u32;
        self.type_children.extend_from_slice(children);
        let mut row = [0; 16];
        row[0] = kind;
        row[1] = flags;
        row[2..4].copy_from_slice(&(children.len() as u16).to_le_bytes());
        row[4..8].copy_from_slice(&spelling.unwrap_or(NONE).to_le_bytes());
        row[8..12].copy_from_slice(&start.to_le_bytes());
        let id = self.types.len() as u32;
        self.types.push(row);
        id
    }
    pub fn symbol(&mut self, owner: u32, name: u32, parameters: &[u32]) -> u32 {
        let start = self.symbol_parameters.len() as u32;
        self.symbol_parameters.extend_from_slice(parameters);
        let mut row = [0; 16];
        row[0..4].copy_from_slice(&owner.to_le_bytes());
        row[4..8].copy_from_slice(&name.to_le_bytes());
        row[8..12].copy_from_slice(&start.to_le_bytes());
        row[12..14].copy_from_slice(&(parameters.len() as u16).to_le_bytes());
        let id = self.symbols.len() as u32;
        self.symbols.push(row);
        id
    }
    pub fn declaration(
        &mut self,
        kind: u8,
        name: u32,
        owner: Option<u32>,
        ty: Option<u32>,
        symbol: Option<u32>,
        doc: Option<&'static [u8]>,
    ) {
        let mut row = [0; 32];
        row[0] = kind;
        row[1] = 1;
        row[2] = u8::from(doc.is_some());
        row[4..8].copy_from_slice(&0_u32.to_le_bytes());
        row[8..12].copy_from_slice(&name.to_le_bytes());
        row[12..16].copy_from_slice(&owner.unwrap_or(NONE).to_le_bytes());
        row[16..20].copy_from_slice(
            &doc.map_or(NONE, |text| {
                let id = self.atom(text);
                id
            })
            .to_le_bytes(),
        );
        row[20..24].copy_from_slice(&NONE.to_le_bytes());
        row[24..28].copy_from_slice(&ty.unwrap_or(NONE).to_le_bytes());
        row[28..32].copy_from_slice(&symbol.unwrap_or(NONE).to_le_bytes());
        self.declarations.push(row);
    }
    pub fn finish(self, source: &[u8]) -> Vec<u8> {
        let mut atoms = Vec::new();
        let mut atom_bytes = Vec::new();
        for atom in &self.atoms {
            atoms.extend_from_slice(&(atom_bytes.len() as u32).to_le_bytes());
            atoms.extend_from_slice(&(atom.len() as u32).to_le_bytes());
            atom_bytes.extend_from_slice(atom);
        }
        let sections = [
            atoms,
            atom_bytes,
            encode_rows(&self.types),
            encode_u32(&self.type_children),
            encode_rows(&self.symbols),
            encode_u32(&self.symbol_parameters),
            encode_rows(&self.declarations),
            Vec::new(),
        ];
        let widths = [8_u16, 1, 16, 4, 16, 4, 32, 20];
        let mut image = vec![0; 176];
        image[..4].copy_from_slice(b"NJAI");
        image[4..6].copy_from_slice(&1_u16.to_le_bytes());
        image[6..8].copy_from_slice(&176_u16.to_le_bytes());
        image[8..10].copy_from_slice(&21_u16.to_le_bytes());
        image[10..12].copy_from_slice(&8_u16.to_le_bytes());
        image[12..16]
            .copy_from_slice(&(sections.iter().map(Vec::len).sum::<usize>() as u32).to_le_bytes());
        let mut offset = 176;
        for (i, section) in sections.iter().enumerate() {
            let at = 48 + i * 16;
            image[at..at + 2].copy_from_slice(&((i + 1) as u16).to_le_bytes());
            image[at + 2..at + 4].copy_from_slice(&widths[i].to_le_bytes());
            image[at + 4..at + 8]
                .copy_from_slice(&((section.len() / usize::from(widths[i])) as u32).to_le_bytes());
            image[at + 8..at + 12].copy_from_slice(&(offset as u32).to_le_bytes());
            image[at + 12..at + 16].copy_from_slice(&(section.len() as u32).to_le_bytes());
            offset += section.len();
        }
        for section in sections {
            image.extend_from_slice(&section);
        }
        let mut hash = Sha256::new();
        hash.update(b"nudox.java.authority.image.sha256.v1\0");
        hash.update(&image[..16]);
        hash.update(&image[48..]);
        image[16..48].copy_from_slice(&hash.finalize());
        let mut bound = vec![0; 80 + image.len()];
        bound[..4].copy_from_slice(b"NJAB");
        bound[4..6].copy_from_slice(&1_u16.to_le_bytes());
        bound[6..8].copy_from_slice(&80_u16.to_le_bytes());
        bound[8..12].copy_from_slice(&(image.len() as u32).to_le_bytes());
        bound[12..44].copy_from_slice(&Sha256::digest(source));
        bound[80..].copy_from_slice(&image);
        let mut hash = Sha256::new();
        hash.update(b"nudox.java.bound.authority.image.sha256.v1\0");
        hash.update(&bound[..44]);
        hash.update(&bound[76..]);
        bound[44..76].copy_from_slice(&hash.finalize());
        bound
    }
}

fn encode_rows<const N: usize>(rows: &[[u8; N]]) -> Vec<u8> {
    rows.iter().flat_map(|row| row.iter().copied()).collect()
}
fn encode_u32(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
pub fn compile(source: &'static [u8], image: Vec<u8>) -> Ir {
    let tool = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/usr/bin/true"),
        b"fixture",
    )
    .unwrap();
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4096];
    let work = std::env::temp_dir();
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::Java { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
    )
    .unwrap()
    .ir
}
