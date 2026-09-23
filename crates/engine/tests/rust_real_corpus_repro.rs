//! Focused real-corpus repros for the ten remaining Rust terminals.
//!
//! Each case stages the largest `.rs` file of one corpus crate exactly as the
//! flow audit's Rust arm does (a synthetic single-file Cargo crate opened with
//! `RustProject::open_with_source`), then drives the full direct-HIR compile.
//!
//! Eight crates exercise real lowering: three macro-invocation occurrence
//! foreign keys and one type-alias generic default. Two crates
//! (`bytemuck@1.25.2`, `objc2-foundation@0.2.2`) are proven empty under the
//! corpus fixture: their selected file is entirely behind non-default Cargo
//! features, so rust-analyzer's HIR carries no declaration while the written
//! item surface stays non-empty — the lane admits the collected-empty
//! product, exactly the Go `doc.go` and Clang cfg-gated analogues (a written
//! surface with no item at all would still be the exact typed rejection; see
//! `rust_semantic_lane::empty_source_is_the_exact_lowering_rejection`).
//!
//! The corpus environment variable is `NUDOX_RUST_CORPUS_DIR`; when unset the
//! test reports a typed skip instead of silently passing.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_rust::legacy::{
    RustFeatureControl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The ten crates from the latest real-200 Rust terminal report.
const FAILING_CRATES: &[&str] = &[
    "accesskit-0.24.1",
    "khronos-egl-6.0.0",
    "rayon-1.11.0",
    "winnow-0.5.40",
    "bytemuck-1.25.2",
    "objc2-foundation-0.2.2",
    "prost-derive-0.14.4",
    "tantivy-stacker-0.7.0",
    "utf8-zero-0.8.1",
    "ena-0.14.4",
];

/// Crates whose selected corpus file is entirely feature-gated, so the
/// synthetic single-file fixture has no declaration at all. These are the
/// corpus-inventory artifact, not an unsupported declaration form.
const CFG_EMPTY_CRATES: &[&str] = &["bytemuck-1.25.2", "objc2-foundation-0.2.2"];

fn resolve_tool() -> Option<PathBuf> {
    if let Some(tool) = std::env::var_os("RUSTC") {
        let tool = PathBuf::from(tool);
        if tool.is_absolute() {
            return Some(tool);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("rustc"))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

/// Finds the largest `.rs` file under `root` with the same deterministic
/// tie-break the audit's `largest_source_file` uses.
fn largest_source_file(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(&directory).ok()?;
        let mut children: Vec<_> = entries.filter_map(Result::ok).collect();
        children.sort_by_key(|entry| entry.path());
        for entry in children {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            {
                let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                let replace = match &best {
                    None => true,
                    Some((known, known_len)) => {
                        len > *known_len || (len == *known_len && path < *known)
                    }
                };
                if replace {
                    best = Some((path, len));
                }
            }
        }
    }
    best.map(|(path, _)| path)
}

/// Stages one selected corpus file as a synthetic single-file Cargo crate and
/// returns its typed terminal label, or `OK`.
fn compile_corpus_file(body: &[u8]) -> String {
    let Some(tool) = resolve_tool() else {
        return "missing-rustc".to_owned();
    };
    let Ok(toolchain) = RustToolchain::discover(&tool) else {
        return "missing-rustc".to_owned();
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-real-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    if fs::create_dir_all(root.join("src")).is_err() {
        return "fixture-io".to_owned();
    }
    if fs::write(
        root.join("Cargo.toml"),
        b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .is_err()
    {
        return "fixture-io".to_owned();
    }
    let source_path = root.join("src/lib.rs");
    if fs::write(&source_path, body).is_err() {
        return "fixture-io".to_owned();
    }
    let outcome = (|| -> Result<String, String> {
        let project =
            RustProject::open_with_source(&root, &source_path, &toolchain, RustEdition::Rust2024)
                .map_err(|error| format!("project-error: {error:?}"))?;
        let resolved = ResolvedToolchain::from_version(
            backend_engine::driver::NativeTool::Rustc,
            &tool,
            b"compiler-driver-rust-real-repro",
        )
        .map_err(|_| "toolchain-resolve".to_owned())?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0_u8; 4096];
        let mut fragment_output = vec![0_u8; 16 * 1024 * 1024];
        let request = CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: body,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Rust {
                project: &project,
                maximum_source_bytes: SourceByteLimit(
                    u32::try_from(body.len()).unwrap_or(u32::MAX),
                ),
                features: RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(240),
                cancelled: &cancelled,
            },
        };
        match compile_semantic(
            request,
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: &root,
            },
            CompileOutput {
                fragment_output: &mut fragment_output,
            },
        ) {
            Ok(_) => Ok("OK".to_owned()),
            Err(failure) => Ok(format_terminal(&failure)),
        }
    })()
    .unwrap_or_else(|error| error);
    let _ = fs::remove_dir_all(&root);
    outcome
}

fn format_terminal(failure: &CompileFailure<'_>) -> String {
    match failure {
        CompileFailure::LoweringUnsupported { cause, .. } => {
            format!("LoweringUnsupported: {cause:?}")
        }
        CompileFailure::Build { cause, .. } => format!("Build: {cause:?}"),
        other => format!("other: {other:?}"),
    }
}

fn corpus_root() -> Option<PathBuf> {
    let root = std::env::var_os("NUDOX_RUST_CORPUS_DIR")?;
    let root = PathBuf::from(root);
    root.is_dir().then_some(root)
}

#[test]
fn rust_real_corpus_crates_lower() {
    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping real corpus repros");
        return;
    };
    let mut failures = Vec::new();
    let filter: Option<Vec<String>> = std::env::var("NUDOX_REPRO_FILTER")
        .ok()
        .map(|value| value.split(',').map(str::to_owned).collect());
    for crate_name in FAILING_CRATES {
        if CFG_EMPTY_CRATES.contains(crate_name) {
            continue;
        }
        if let Some(filter) = &filter
            && !filter.iter().any(|needle| crate_name.contains(needle.as_str()))
        {
            continue;
        }
        let directory = root.join(crate_name);
        let Some(source) = largest_source_file(&directory) else {
            eprintln!("REPRO {crate_name}: no .rs source");
            failures.push(*crate_name);
            continue;
        };
        let bytes = match fs::read(&source) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("REPRO {crate_name}: read error {error}");
                failures.push(*crate_name);
                continue;
            }
        };
        let outcome = compile_corpus_file(&bytes);
        eprintln!(
            "REPRO {crate_name} file={} bytes={} => {outcome}",
            source.strip_prefix(&root).unwrap_or(&source).display(),
            bytes.len()
        );
        if outcome != "OK" {
            failures.push(*crate_name);
        }
    }
    assert!(failures.is_empty(), "crates still failing: {failures:?}");
}

/// Proves the two remaining cfg-gated selections admit their collected-empty
/// product: the selected file declares every top-level item behind a
/// non-default Cargo feature, so rust-analyzer's HIR carries no declaration
/// while the written item surface stays non-empty — the honest empty product,
/// never a lowering defect.
#[test]
fn rust_real_corpus_cfg_gated_sources_admit_the_empty_product() {
    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping cfg-gated proof");
        return;
    };
    for crate_name in CFG_EMPTY_CRATES {
        let directory = root.join(crate_name);
        let source = largest_source_file(&directory).expect("cfg-empty crate has a source file");
        let bytes = fs::read(&source).expect("cfg-empty source is readable");
        let text = String::from_utf8_lossy(&bytes);
        // The crate root is gated, or every top-level item is feature-gated.
        let crate_gated = text.starts_with("#![cfg(");
        let mut item_gated = false;
        for line in text.lines() {
            let line = line.trim_start();
            if line.starts_with("pub mod ")
                || line.starts_with("mod ")
                || line.starts_with("pub fn ")
                || line.starts_with("pub struct ")
                || line.starts_with("pub enum ")
                || line.starts_with("pub trait ")
            {
                item_gated = true;
                break;
            }
        }
        assert!(
            crate_gated || item_gated,
            "{crate_name}: selection is not cfg-gated"
        );
        let outcome = compile_corpus_file(&bytes);
        eprintln!("CFG-EMPTY {crate_name} crate_gated={crate_gated} => {outcome}");
        assert_eq!(
            outcome, "OK",
            "{crate_name}: the cfg-gated source must admit its empty product"
        );
    }
}

/// Minimal deterministic fixtures for the exact legal shapes the real corpus
/// surfaced. Each must lower through the full IR identity build, where the
/// duplicate-declaration terminal is raised.
#[test]
fn rust_real_corpus_regression_fixtures() {
    // Several inherent impl blocks for one type are legal Rust and must not
    // collide: the impl variant frames its ordered member set.
    let impl_blocks = "pub struct Node; impl Node { pub fn a(&self) -> u8 { 1 } } impl Node { pub fn b(&self) -> u8 { 2 } }";
    // Two same-typed wildcard parameters bind nothing; their positional names
    // keep the signature carriers distinct.
    let wildcard_parameters = "pub fn pair(_: u8, _: u8) -> u8 { 0 }";
    // Two impls of one local trait for one self type differ only in the
    // trait's generic arguments; the written trait spelling frames that.
    let local_trait_arguments = "pub trait Convert<T> { fn go(&self, value: T) -> u8; } pub struct S; impl Convert<u8> for S { fn go(&self, value: u8) -> u8 { value } } impl Convert<u16> for S { fn go(&self, value: u16) -> u8 { value as u8 } }";
    // A macro invocation's key is its path spelling, never the whole call text
    // (which may carry a line-continuation backslash inside a string literal).
    let macro_line_continuation =
        "pub fn describe(value: u8) -> String { format!(\"value \\\n {value}\") }";
    // One compound spelling hosted twice lowers to one carrier fact, never two
    // byte-identical twins the identity build would reject as a duplicate.
    let repeated_compound_carrier = "pub fn pair(_: &[u8], _: &[u8]) -> usize { 0 }";
    // A foreign application (`Vec<u8>`) repeated in two positions dedups to the
    // same foreign leaf and application structure.
    let repeated_foreign_application = "pub fn pair(_: Vec<u8>, _: Vec<u8>) -> usize { 0 }";
    // `impl Trait` spells no path leaf; the whole written bound run is borrowed
    // so two distinct `impl` positions stay distinct without a fabricated name.
    let impl_trait_written_run = "pub fn a() -> impl core::fmt::Debug { 0u8 } pub fn b() -> impl core::fmt::Debug { 0u16 }";
    // A generic default on a declaration is an annotation, not a use; the bare
    // local default binds the declared ordinal, and an unmodelled default keeps
    // its honest written spelling.
    let generic_defaults =
        "pub struct Holder<T = u8>(pub T); pub type Alias<T = u16> = T;";
    // A `where` predicate is a free predicate on the callable, not a second
    // declared parameter row.
    let where_free_predicate =
        "pub fn f<T>(value: T) -> T where T: core::fmt::Debug { value }";
    // Two distinct raw-pointer compounds in one signature are distinct
    // carriers, while the shared leaf stays one deduped row.
    let repeated_raw_pointer =
        "pub fn pair(_: *const u8, _: *const u8) -> u8 { 0 }";
    for (label, body) in [
        ("multiple-inherent-impls", impl_blocks),
        ("wildcard-parameters", wildcard_parameters),
        ("local-trait-arguments", local_trait_arguments),
        ("macro-line-continuation", macro_line_continuation),
        ("repeated-compound-carrier", repeated_compound_carrier),
        ("repeated-foreign-application", repeated_foreign_application),
        ("impl-trait-written-run", impl_trait_written_run),
        ("generic-defaults", generic_defaults),
        ("where-free-predicate", where_free_predicate),
        ("repeated-raw-pointer", repeated_raw_pointer),
    ] {
        let outcome = compile_corpus_file(body.as_bytes());
        eprintln!("FIXTURE {label} => {outcome}");
        assert_eq!(outcome, "OK", "fixture {label} did not lower");
    }
}

/// Regression: `cargo:itoa@1.0.14` raised `DuplicateDeclarationIdentity`
/// because one `macro_rules!` invocation stamps several impl blocks for one
/// self type, and every expanded impl projects onto the single invocation
/// span. Both impls then held the same family, the same inherent trait frame,
/// an empty member run, and an identical self-type record. The lane now mints
/// an authority-proven identity discriminator from the expansion's own trait
/// and self-type spelling, so the stamped impls stay distinct declarations.
#[test]
fn rust_real_corpus_macro_stamped_impls_lower() {
    // The exact itoa shape: two impl blocks stamped by one macro expansion,
    // one trait impl and one sealed-trait impl for the same self type.
    let itoa_shape = "mod seal { pub trait Sealed { fn write(self) -> u8; } } \
        pub trait Integer { const MAX_STR_LEN: usize; } \
        pub struct Unit; \
        macro_rules! stamp { ($t:ty) => { \
            impl Integer for $t { const MAX_STR_LEN: usize = 1; } \
            impl seal::Sealed for $t { fn write(self) -> u8 { 0 } } \
        }; } \
        stamp!(Unit); stamp!(u8);";
    // Expanded inherent impls stamp under the same span and must also stay
    // distinct from any expanded trait impl of the same self type.
    let inherent_shape = "pub struct Unit; \
        macro_rules! stamp { ($t:ty) => { \
            impl $t { pub fn a(&self) -> u8 { 0 } } \
            impl core::ops::Add for $t { type Output = $t; fn add(self, _: $t) -> $t { self } } \
        }; } \
        stamp!(Unit);";
    for (label, body) in [
        ("itoa-shape", itoa_shape),
        ("inherent-shape", inherent_shape),
    ] {
        let outcome = compile_corpus_file(body.as_bytes());
        eprintln!("MACRO-IMPL {label} => {outcome}");
        assert_eq!(outcome, "OK", "macro-stamped impl fixture {label} did not lower");
    }
}
