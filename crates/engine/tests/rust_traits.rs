//! Falsifiers for Rust trait-implementation edges and semantic residuals.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_semantic::ir::{
    DecodedDocFact, DecodedOccurrence, DecodedTypeFact, EntityKind, FragmentView, SemanticTypeTag,
};
use backend_frontend_rust::legacy::{RustFeatureControl, RustProject, RustToolchain, SourceByteLimit};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const BODY: &str = r#"//! Fixture.

/// A node with a documented self-link [`Node`].
pub struct Node {
    /// Weight.
    pub weight: u8,
}

pub trait Visitor {
    fn visit(&self, node: &Node) -> u8;
}

impl Visitor for Node {
    fn visit(&self, node: &Node) -> u8 {
        node.weight
    }
}

/// Uses a dynamic visitor.
pub fn use_visitor(visitor: &dyn Visitor, node: &Node) -> u8 {
    visitor.visit(node)
}
"#;

struct Lane<'a> {
    entities: Vec<(&'a [u8], EntityKind)>,
    types: Vec<DecodedTypeFact<'a>>,
    occurrences: Vec<DecodedOccurrence<'a>>,
    docs: Vec<DecodedDocFact<'a>>,
}

fn root(body: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-traits-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"traits_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), body).map_err(|error| error.to_string())?;
    Ok(root)
}

fn rustc() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    std::env::var_os("PATH")
        .ok_or_else(|| "PATH has no rustc".to_owned())?
        .to_str()
        .ok_or_else(|| "PATH is not UTF-8".to_owned())?
        .split(':')
        .map(PathBuf::from)
        .map(|directory| directory.join("rustc"))
        .find_map(|path| path.is_file().then(|| path.canonicalize().ok()).flatten())
        .ok_or_else(|| "rustc absent".to_owned())
}

fn compile_fixture(body: &str) -> Result<Vec<u8>, String> {
    let root = root(body)?;
    let source = root.join("src/lib.rs");
    let tool = rustc()?;
    let toolchain =
        RustToolchain::discover(&tool).map_err(|error| format!("toolchain: {error:?}"))?;
    let project = RustProject::open_with_source(&root, &source, &toolchain, RustEdition::Rust2024)
        .map_err(|error| format!("project: {error:?}"))?;
    let resolved = ResolvedToolchain::from_version(
        backend_engine::driver::NativeTool::Rustc,
        &tool,
        b"rust-traits-fixture",
    )
    .map_err(|error| format!("tool: {error:?}"))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let mut output = vec![0_u8; 262_144];
    let result = compile(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: body.as_bytes(),
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Rust {
                project: &project,
                maximum_source_bytes: SourceByteLimit::from(65_536),
                features: RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &root,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|error| format!("compile: {error:?}"));
    let _ = fs::remove_dir_all(&root);
    let _compiled = result?;
    let length = output
        .get(8..12)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "header truncated".to_owned())? as usize;
    output
        .get(..length)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "declared length exceeds output".to_owned())
}

fn lane(bytes: &[u8]) -> Result<Lane<'_>, String> {
    let view = FragmentView::validate(bytes).map_err(|error| format!("validate: {error:?}"))?;
    let atoms: Vec<_> = view.atoms().map(|atom| atom.bytes).collect();
    let mut entities = Vec::new();
    for entity in view.entities() {
        let name = atoms
            .get(entity.name.raw as usize)
            .copied()
            .ok_or("entity atom")?;
        entities.push((name, entity.kind));
    }
    let types = view
        .type_facts()
        .map(|cursor| {
            cursor
                .map(|row| row.map_err(|error| format!("type: {error:?}")))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let occurrences = view
        .occurrences()
        .map(|cursor| {
            cursor
                .map(|row| row.map_err(|error| format!("occurrence: {error:?}")))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let docs = view
        .docs()
        .map(|cursor| {
            cursor
                .map(|row| row.map_err(|error| format!("doc: {error:?}")))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Lane {
        entities,
        types,
        occurrences,
        docs,
    })
}

fn ordinal(lane: &Lane<'_>, name: &[u8], kind: EntityKind) -> Result<usize, String> {
    lane.entities
        .iter()
        .position(|(known, known_kind)| *known == name && *known_kind == kind)
        .ok_or_else(|| format!("entity {name:?}/{kind:?} absent"))
}

#[test]
fn impl_trait_path_is_oracle_local_and_implementation_owned() -> Result<(), String> {
    let bytes = compile_fixture(BODY)?;
    let lane = lane(&bytes)?;
    let visitor = ordinal(&lane, b"Visitor", EntityKind::Trait)?;
    let implementation = ordinal(&lane, b"Node", EntityKind::Implementation)?;
    let occurrence = lane
        .occurrences
        .iter()
        .find(|row| {
            row.owner.raw as usize == implementation
                && row.occurrence.kind == backend_semantic::ir::ReferenceKind::TypeReference
        })
        .ok_or("impl trait occurrence absent")?;
    if occurrence.occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Oracle {
        return Err("impl trait occurrence is not oracle".to_owned());
    }
    match occurrence.occurrence.target {
        backend_semantic::ir::OccurrenceTarget::Local(target) if target.raw as usize == visitor => Ok(()),
        _ => Err("impl trait target is not local Visitor".to_owned()),
    }
}

#[test]
fn field_access_is_oracle_local_to_named_field() -> Result<(), String> {
    let bytes = compile_fixture(BODY)?;
    let lane = lane(&bytes)?;
    let weight = ordinal(&lane, b"weight", EntityKind::Field)?;
    let row = lane
        .occurrences
        .iter()
        .find(|row| row.occurrence.kind == backend_semantic::ir::ReferenceKind::FieldAccess)
        .ok_or("field occurrence absent")?;
    if row.occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Oracle {
        return Err("field occurrence is not oracle".to_owned());
    }
    match row.occurrence.target {
        backend_semantic::ir::OccurrenceTarget::Local(target) if target.raw as usize == weight => Ok(()),
        _ => Err("field target is not local weight".to_owned()),
    }
}

#[test]
fn dyn_trait_parameter_reaches_dyn_trait_type_fact() -> Result<(), String> {
    let bytes = compile_fixture(BODY)?;
    let lane = lane(&bytes)?;
    let parameter = ordinal(&lane, b"visitor", EntityKind::Parameter)?;
    if !lane
        .types
        .iter()
        .any(|fact| fact.owner.raw as usize == parameter)
    {
        return Err("visitor parameter has no type facts".to_owned());
    }
    lane.types
        .iter()
        .any(|fact| fact.record.tag == SemanticTypeTag::DynTrait)
        .then_some(())
        .ok_or_else(|| "visitor parameter's type graph lacks DynTrait".to_owned())
}

#[test]
fn intra_doc_node_link_resolves_local() -> Result<(), String> {
    let bytes = compile_fixture(BODY)?;
    let lane = lane(&bytes)?;
    let node = ordinal(&lane, b"Node", EntityKind::Record)?;
    let link = lane
        .docs
        .iter()
        .find_map(|fact| match fact.fragment {
            backend_semantic::ir::DocFragmentInput::Link { label, target } if label == b"Node" => {
                Some(target)
            }
            _ => None,
        })
        .ok_or("Node doc link absent")?;
    match link {
        backend_semantic::ir::DocLinkTarget::Local(target) if target.raw as usize == node => Ok(()),
        _ => Err("Node doc link is not local".to_owned()),
    }
}
