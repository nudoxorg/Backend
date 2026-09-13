#![forbid(unsafe_code)]

//! Work-law falsifiers for the single-pass Rust authority collections.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_semantic::ir::{FragmentView, TypeFactSegment};
use compiler_languages_rust::{RustFeatureControl, RustProject, RustToolchain, SourceByteLimit};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

fn fixture(source: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-shortcuts-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"shortcuts\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )?;
    let path = root.join("src/lib.rs");
    fs::write(&path, source)?;
    let rustc = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("rustc"))
                    .find(|candidate| candidate.is_file())
            })
        })
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "rustc"))?;
    let toolchain = RustToolchain::discover(&rustc)?;
    let project = RustProject::open_with_source(&root, &path, &toolchain, RustEdition::Rust2024)?;
    let resolved =
        ResolvedToolchain::from_version(NativeTool::Rustc, &rustc, b"rust-shortcuts-falsifier")?;
    let cancelled = AtomicBool::new(false);
    let mut output = vec![0; 4 * 1024 * 1024];
    let compiled = compile(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: source.as_bytes(),
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Rust {
                project: &project,
                maximum_source_bytes: SourceByteLimit::from(4 * 1024 * 1024),
                features: RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut [],
            native_work: &root,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )?;
    let bytes = compiled.fragment.as_ref().to_vec();
    fs::remove_dir_all(root)?;
    Ok(bytes)
}

fn facts(bytes: &[u8]) -> Result<(usize, usize, usize, usize), Box<dyn std::error::Error>> {
    let view = FragmentView::validate(bytes)?;
    let entities = view.entities().count();
    let (mut declared, mut computed) = (0, 0);
    if let Some(cursor) = view.type_facts() {
        for fact in cursor {
            match fact?.segment {
                TypeFactSegment::Declared => declared += 1,
                TypeFactSegment::Computed => computed += 1,
            }
        }
    }
    let occurrences = view.occurrences().map_or(0, |rows| rows.count());
    let docs = view.docs().map_or(0, |rows| rows.count());
    Ok((entities, declared + computed, occurrences, docs))
}

/// F2 (work): the registry fixture whose walk dominated corpus runs —
/// log@0.4.34, 66,290 bytes with 16 `macro_rules!` definitions — must finish
/// the full compile inside the corpus deadline budget. The pre-shortcut walk
/// alone was observed at 33-195 s depending on machine load (Terra,
/// 2026-09-03/04); the redundant-collection collapse targets the loaded end.
#[test]
fn registry_log_compiles_within_the_corpus_deadline() -> Result<(), Box<dyn std::error::Error>> {
    let rustc = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("rustc"))
                    .find(|candidate| candidate.is_file())
            })
        })
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "rustc"))?;
    let toolchain = RustToolchain::discover(&rustc)?;
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let registry = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or_else(|| std::io::Error::other("no cargo home"))?
        .join("registry/src");
    let cancelled = AtomicBool::new(false);
    let located = compiler_languages_rust::RustPackageUrl::parse("cargo:log@0.4.34")?.locate(
        &workspace,
        &toolchain,
        Some(&registry),
        &cancelled,
    )?;
    let source = fs::read(&located.project().source_path)?;
    let resolved =
        ResolvedToolchain::from_version(NativeTool::Rustc, &rustc, b"rust-shortcuts-log")?;
    let started = Instant::now();
    let mut output = vec![0; 16 * 1024 * 1024];
    let compiled = compile(
        CompileRequest {
            profile: LanguageProfile::Rust(located.project().edition),
            stage: Stage::LowerIr,
            source: &source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Rust {
                project: located.project(),
                maximum_source_bytes: SourceByteLimit::from(4 * 1024 * 1024),
                features: RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(150),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut [],
            native_work: &workspace,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )?;
    let elapsed = started.elapsed();
    let view = FragmentView::validate(compiled.fragment.as_ref())?;
    assert!(view.entities().count() > 0);
    assert!(
        elapsed < Duration::from_secs(150),
        "log compile took {elapsed:?}"
    );
    Ok(())
}

/// F3: computed rows remain attached to the exact enclosing function.
#[test]
fn computed_rows_split_by_owner() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fixture(
        "pub fn first() { let a = 1u8; let _ = a.clone(); }\npub fn second() { let b = 2u8; let _ = b.clone(); }\n",
    )?;
    let view = FragmentView::validate(&bytes)?;
    let owners: Vec<u32> = view
        .type_facts()
        .into_iter()
        .flat_map(|rows| rows.filter_map(Result::ok))
        .filter(|fact| fact.segment == TypeFactSegment::Computed)
        .map(|fact| fact.owner.raw)
        .collect();
    assert!(owners.len() >= 4);
    assert_ne!(owners[0], owners[owners.len() - 1]);
    Ok(())
}

/// F4: a doc line duplicated in code still borrows the doc line's source bytes.
#[test]
fn doc_locator_prefers_exact_content_span() -> Result<(), Box<dyn std::error::Error>> {
    let source =
        "fn body() { let _ = \"Repeated line.\"; }\n/// Repeated line.\npub fn documented() {}\n";
    let bytes = fixture(source)?;
    let view = FragmentView::validate(&bytes)?;
    let text = view
        .docs()
        .into_iter()
        .flat_map(|rows| rows.filter_map(Result::ok))
        .find_map(|fact| match fact.fragment {
            backend_semantic::ir::DocFragmentInput::Text(bytes) => Some(bytes),
            _ => None,
        })
        .ok_or("doc text absent")?;
    assert_eq!(text, b"Repeated line.");
    let _ = facts(&bytes)?;
    Ok(())
}

/// F1: the identity oracle's compact plane counts are fixed by the pre-change
/// harness (commit 2eb5ad834; 2026-09-04; machine load may affect elapsed time).
#[test]
fn identity_counts_match_prechange_oracle() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = [
        "macro_rules! m { ($x:expr) => { $x }; }\npub fn run() { let n = 1u8; m!(n.clone()); }\n",
        "pub use core::fmt::Debug as D;\npub fn run() { let n = 1u8; let pair = (n, 2u16); let _ = pair.clone(); }\n",
        "pub fn run() { let n = 1u8; let _ = n.clone(); }\n",
    ];
    let observed: Vec<_> = fixtures
        .into_iter()
        .map(|source| fixture(source).and_then(|bytes| facts(&bytes)))
        .collect::<Result<_, _>>()?;
    assert_eq!(observed, [(2, 4, 3, 0), (3, 13, 4, 0), (2, 5, 2, 0)]);
    Ok(())
}
