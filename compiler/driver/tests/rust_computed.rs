//! Falsifiers for Rust computed type rows and source-level re-export facts.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use compiler_ir::{EntityKind, FragmentView, SemanticTypeTag, TypeFactSegment};
use compiler_languages_rust::{RustFeatureControl, RustProject, RustToolchain, SourceByteLimit};
use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn compile_fixture(body: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-rust-computed-fixture-{sequence}"));
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"computed_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )?;
    fs::write(root.join("src/lib.rs"), body)?;
    let source_path = root.join("src/lib.rs");
    let tool = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("PATH")?
                .to_str()?
                .split(':')
                .map(PathBuf::from)
                .map(|dir| dir.join("rustc"))
                .find(|path| path.is_file())
        })
        .ok_or("RUSTC missing")?;
    let toolchain = RustToolchain::discover(&tool)?;
    let project =
        RustProject::open_with_source(&root, &source_path, &toolchain, RustEdition::Rust2024)?;
    let resolved =
        ResolvedToolchain::from_version(compiler_driver::NativeTool::Rustc, &tool, b"computed")?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [];
    let mut output = vec![0; 1_048_576];
    let _compiled = compile(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: body.as_bytes(),
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
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
    .map_err(|failure| std::io::Error::other(format!("{failure:?}")))?;
    let length = output
        .get(8..12)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .and_then(|length| usize::try_from(length).ok())
        .ok_or("fragment length")?;
    let bytes = output.get(..length).ok_or("fragment length")?.to_vec();
    fs::remove_dir_all(root)?;
    Ok(bytes)
}

#[test]
fn proven_let_and_call_types_are_computed_and_reexports_are_entities()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = compile_fixture(
        "pub use core::fmt::Debug as D; pub use core::{clone, marker}; pub fn run() { let n = 1u8; let pair = (n, 2u16); let _ = pair.clone(); }",
    )?;
    let view = FragmentView::validate(&bytes)?;
    let reexports = view
        .entities()
        .filter(|entity| entity.kind == EntityKind::Reexport)
        .count();
    assert!(reexports >= 3);
    let mut computed = 0;
    if let Some(mut facts) = view.type_facts() {
        for fact in &mut facts {
            if fact?.segment == TypeFactSegment::Computed {
                computed += 1;
            }
        }
    }
    assert!(computed >= 3);
    Ok(())
}

#[test]
fn computed_rows_retain_exact_primitive_lattice() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = compile_fixture("pub fn run() { let value = 1u8; }")?;
    let view = FragmentView::validate(&bytes)?;
    let fact = view
        .type_facts()
        .ok_or("missing types")?
        .find_map(|fact| {
            let fact = fact.ok()?;
            (fact.segment == TypeFactSegment::Computed).then_some(fact)
        })
        .ok_or("missing computed row")?;
    assert_eq!(fact.record.tag, SemanticTypeTag::Primitive);
    assert_eq!(fact.record.payload1, 8 << 1);
    Ok(())
}

#[test]
fn unresolved_let_has_no_computed_row() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = compile_fixture("pub fn run() { let value = unresolved_path::thing; }")?;
    let view = FragmentView::validate(&bytes)?;
    let count = view
        .type_facts()
        .into_iter()
        .flatten()
        .filter(|fact| {
            fact.as_ref()
                .is_ok_and(|fact| fact.segment == TypeFactSegment::Computed)
        })
        .count();
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn glob_use_does_not_fabricate_a_reexport() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = compile_fixture("pub use core::fmt::*; pub fn run() {}")?;
    let view = FragmentView::validate(&bytes)?;
    assert_eq!(
        view.entities()
            .filter(|entity| entity.kind == EntityKind::Reexport)
            .count(),
        0
    );
    Ok(())
}

#[test]
fn computed_capacity_is_a_typed_rejection() -> Result<(), Box<dyn std::error::Error>> {
    let mut body = String::from("pub fn run() {");
    for index in 0..1025 {
        body.push_str(&format!("let value_{index} = {index}u32;"));
    }
    body.push('}');
    assert!(compile_fixture(&body).is_err());
    Ok(())
}

#[test]
fn macro_inner_call_is_owned_by_the_function() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = compile_fixture(
        "macro_rules! make { ($value:expr) => { $value.clone() }; } pub fn run(value: String) { let copied = make!(value); }",
    )?;
    let view = FragmentView::validate(&bytes)?;
    assert!(
        view.type_facts()
            .into_iter()
            .flatten()
            .any(|fact| fact.is_ok_and(|fact| fact.segment == TypeFactSegment::Computed))
    );
    Ok(())
}
