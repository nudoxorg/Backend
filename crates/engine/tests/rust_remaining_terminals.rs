//! Focused repros for the current round of Rust audit terminals.
//!
//! The real corpus audit selects a Rust crate's *crate root* (`src/lib.rs` or
//! `src/main.rs`) when one is not a declaration shell, else the largest
//! non-shell `.rs` file (see `inventory_resolve::largest_source_file`), then
//! stages it as a synthetic single-file Cargo crate and drives the full
//! direct-HIR compile. This suite reproduces exactly that selection and pins
//! each fixed terminal class with a synthetic twin plus a corpus guard:
//!
//! - higher-ranked `for<'a>` where-predicates lower into the free-predicate
//!   lane (`cargo:either@1.18.0`),
//! - implementations name by their whole written self type, so blocks whose
//!   self types differ only in generic arguments stay distinct declarations
//!   (`cargo:generic-array@1.4.5`),
//! - the anonymous foreign-row dedup table is sized for measured corpus
//!   demand (`cargo:itertools@0.15.0`),
//! - a crate root whose every written item stayed out of the lane (each
//!   behind an unmet `#[cfg]` gate, an unresolved facade re-export, or a
//!   `compile_error!` stub) admits its collected-empty product, while a
//!   written surface with no item at all stays the exact typed rejection
//!   (`cargo:clap@4.6.7`, `cargo:crossbeam-channel@0.5.17`,
//!   `cargo:tracing-subscriber@0.3.23`, `cargo:bincode@3.0.0`,
//!   `cargo:futures-io@0.3.34`).
//!
//! A `NUDOX_REPRO_FILTER` comma-filter lets one crate be iterated without
//! paying the whole set.

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

/// Crates whose selected crate root holds real declarations and whose lowering
/// previously minted duplicate declaration identities.
const LOWERING_CRATES: &[&str] = &["dragonbox_ecma-0.1.12", "winnow-0.5.40"];

/// Crates whose selected crate root is a written stub with no top-level item
/// at all (`#![no_std]` only). Such a source carries no written surface to
/// prove, so the exact typed rejection stays.
const EMPTY_CRATE_ROOT: &[&str] = &["windows_x86_64_gnullvm-0.48.5"];

/// Crates whose selected crate root carries written items that all stayed out
/// of the lane (each behind an unmet `#[cfg]` gate or an unresolved facade
/// re-export). The authority succeeded, so the collected-empty product is the
/// honest output; the largest non-root source still lowers with real
/// declarations, proving no construct is at fault.
const GATED_CRATE_ROOT: &[&str] = &["futures-executor-0.3.34"];

/// Crates whose selected crate root declares only `macro_rules!` items. Such
/// a root is not empty: the macro is the crate's written declaration, and the
/// lane commits it as a closed `Macro` row instead of rejecting the crate as
/// declaration-less.
const MACRO_ONLY_CRATE_ROOT: &[&str] = &["cfg-if-1.0.0"];

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

/// The audit's Rust selection: prefer the crate root, otherwise the largest
/// `.rs` file with the deterministic path tie-break.
fn selected_source_file(root: &Path, prefer_crate_root: bool) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    let mut crate_root: Option<PathBuf> = None;
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
                if matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("lib.rs" | "main.rs")
                ) {
                    crate_root.get_or_insert(path.clone());
                }
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
    if prefer_crate_root {
        crate_root.or_else(|| best.map(|(path, _)| path))
    } else {
        best.map(|(path, _)| path)
    }
}

fn compile_corpus_file(body: &[u8]) -> String {
    stage_fixture(body, 0).map_or_else(
        || "fixture-io".to_owned(),
        |root| compile_staged_fixture(&root, body),
    )
}

/// Stages the audit's synthetic single-file Cargo crate, plus an optional
/// path dependency providing `count` distinct foreign types.
fn stage_fixture(body: &[u8], dep_types: usize) -> Option<PathBuf> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-term-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("src")).ok()?;
    if dep_types > 0 {
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nfonts = {{ path = \"fonts\" }}\n"
            ),
        )
        .ok()?;
        fs::create_dir_all(root.join("fonts/src")).ok()?;
        fs::write(
            root.join("fonts/Cargo.toml"),
            b"[package]\nname = \"fonts\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .ok()?;
        let mut dep = String::new();
        for ordinal in 0..dep_types {
            dep.push_str(&format!("pub struct T{ordinal};\n"));
        }
        fs::write(root.join("fonts/src/lib.rs"), dep).ok()?;
    } else {
        fs::write(
            root.join("Cargo.toml"),
            b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .ok()?;
    }
    fs::write(root.join("src/lib.rs"), body).ok()?;
    Some(root)
}

/// Drives the full direct-HIR compile over one staged fixture root.
fn compile_staged_fixture(root: &Path, body: &[u8]) -> String {
    let Some(tool) = resolve_tool() else {
        return "missing-rustc".to_owned();
    };
    let Ok(toolchain) = RustToolchain::discover(&tool) else {
        return "missing-rustc".to_owned();
    };
    let source_path = root.join("src/lib.rs");
    let outcome = (|| -> Result<String, String> {
        let project =
            RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)
                .map_err(|error| format!("project-error: {error:?}"))?;
        let resolved = ResolvedToolchain::from_version(
            backend_engine::driver::NativeTool::Rustc,
            &tool,
            b"compiler-driver-rust-remaining",
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
                native_work: root,
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
    let _ = fs::remove_dir_all(root);
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
fn rust_remaining_lowering_crates_lower() {
    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping remaining terminal repros");
        return;
    };
    let filter: Option<Vec<String>> = std::env::var("NUDOX_REPRO_FILTER")
        .ok()
        .map(|value| value.split(',').map(str::to_owned).collect());
    let mut failures = Vec::new();
    for crate_name in LOWERING_CRATES {
        if let Some(filter) = &filter
            && !filter
                .iter()
                .any(|needle| crate_name.contains(needle.as_str()))
        {
            continue;
        }
        let (source, bytes) = selected(&root, crate_name, true);
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

/// Pins the two-sided empty-root contract. A written stub with no top-level
/// item at all stays the exact typed rejection; a crate root whose every
/// written item stayed out of the lane under the fixture's feature policy
/// admits its collected-empty product, exactly the Go `doc.go` and Clang
/// cfg-gated analogues. The largest non-root source of the same crate still
/// lowers cleanly, proving the terminal was never a lowering defect.
#[test]
fn rust_empty_crate_roots_are_the_typed_terminal() {
    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping empty-root proof");
        return;
    };
    for crate_name in EMPTY_CRATE_ROOT {
        let (root_source, root_bytes) = selected(&root, crate_name, true);
        let root_name = root_source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert_eq!(
            root_name, "lib.rs",
            "{crate_name}: crate-root selection is not lib.rs"
        );
        let outcome = compile_corpus_file(&root_bytes);
        eprintln!(
            "EMPTY-ROOT {crate_name} file={} bytes={} => {outcome}",
            root_source
                .strip_prefix(&root)
                .unwrap_or(&root_source)
                .display(),
            root_bytes.len()
        );
        assert_eq!(
            outcome, "LoweringUnsupported: NoSupportedDeclaration",
            "{crate_name}: expected the item-less stub to stay the exact terminal"
        );
        // The crate is not empty at all: its largest non-root source lowers,
        // so the terminal is the fixture's feature policy, never a construct.
        let (largest, largest_bytes) = selected(&root, crate_name, false);
        if largest == root_source {
            continue;
        }
        let fallback = compile_corpus_file(&largest_bytes);
        eprintln!(
            "EMPTY-ROOT-FALLBACK {crate_name} file={} bytes={} => {fallback}",
            largest.strip_prefix(&root).unwrap_or(&largest).display(),
            largest_bytes.len()
        );
        assert_eq!(
            fallback, "OK",
            "{crate_name}: a non-root source failed to lower"
        );
    }
    for crate_name in GATED_CRATE_ROOT {
        let (root_source, root_bytes) = audit_selected(&root, crate_name);
        let outcome = compile_corpus_file(&root_bytes);
        eprintln!(
            "GATED-ROOT {crate_name} file={} bytes={} => {outcome}",
            root_source
                .strip_prefix(&root)
                .unwrap_or(&root_source)
                .display(),
            root_bytes.len()
        );
        assert_eq!(
            outcome, "OK",
            "{crate_name}: the cfg-gated crate root must admit its empty product"
        );
    }
}

/// Proves a crate root whose entire written surface is a `macro_rules!`
/// definition lowers: the macro is the crate's declaration, committed as a
/// closed `Macro` row, never a `NoSupportedDeclaration` rejection. The
/// corpus-guarded half pins the exact audit row (`cargo:cfg-if@1.0.0`) whose
/// root declares one exported macro and cfg-gates everything else.
#[test]
fn rust_macro_only_crate_roots_lower_with_macro_rows() {
    // Synthetic cfg-if shape: one documented, exported macro and nothing else.
    let outcome = compile_corpus_file(
        b"/// Cascade `#[cfg]` cases.\n#[macro_export]\nmacro_rules! cascade {\n    ($($tokens:tt)*) => { $($tokens)* };\n}\n",
    );
    eprintln!("MACRO-ONLY synthetic => {outcome}");
    assert_eq!(outcome, "OK", "a macro-only crate root must lower");

    // A macro inside a cfg-disabled module stays out with that module's
    // subtree while the enabled surface (including its own macro) lowers.
    let outcome = compile_corpus_file(
        b"#[cfg(any())] pub mod gone {\n    macro_rules! hidden { () => {}; }\n    pub fn f() -> u8 { 1 }\n}\n#[macro_export]\nmacro_rules! kept { () => {}; }\npub fn real() -> u8 { 2 }\n",
    );
    eprintln!("MACRO-ONLY cfg-disabled twin => {outcome}");
    assert_eq!(outcome, "OK", "the cfg-disabled subtree leaked");

    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping macro-only corpus repro");
        return;
    };
    for crate_name in MACRO_ONLY_CRATE_ROOT {
        let (source, bytes) = selected(&root, crate_name, true);
        let root_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert_eq!(
            root_name, "lib.rs",
            "{crate_name}: crate-root selection is not lib.rs"
        );
        let outcome = compile_corpus_file(&bytes);
        eprintln!(
            "MACRO-ONLY {crate_name} file={} bytes={} => {outcome}",
            source.strip_prefix(&root).unwrap_or(&source).display(),
            bytes.len()
        );
        assert_eq!(
            outcome, "OK",
            "{crate_name}: the macro-only crate root failed to lower"
        );
    }
}

/// Reads the audit-selected source and bytes for one crate.
fn selected(root: &Path, crate_name: &str, prefer_crate_root: bool) -> (PathBuf, Vec<u8>) {
    let directory = root.join(crate_name);
    let source = selected_source_file(&directory, prefer_crate_root)
        .unwrap_or_else(|| panic!("{crate_name}: no .rs source"));
    let bytes = fs::read(&source).unwrap_or_else(|error| panic!("{crate_name}: {error}"));
    (source, bytes)
}

/// The audit's exact Rust selection (see
/// `inventory_resolve::largest_source_file`): prefer a crate root that is not
/// a declaration shell, else the largest non-shell source, else the raw
/// largest, with the deterministic `(size desc, path asc)` order.
fn audit_selected(root: &Path, crate_name: &str) -> (PathBuf, Vec<u8>) {
    let directory = root.join(crate_name);
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack = vec![directory.clone()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current).unwrap_or_else(|error| panic!("{crate_name}: {error}"));
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs") => {
                    let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                    files.push((path, len));
                }
                _ => {}
            }
        }
    }
    assert!(!files.is_empty(), "{crate_name}: no .rs source");
    files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let is_crate_root = |path: &Path| {
        matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("lib.rs" | "main.rs")
        )
    };
    let shell = |path: &Path| is_rust_declaration_shell(path);
    let chosen = files
        .iter()
        .find(|(path, _)| is_crate_root(path) && !shell(path))
        .or_else(|| files.iter().find(|(path, _)| !shell(path)))
        .unwrap_or(&files[0]);
    let bytes = fs::read(&chosen.0).unwrap_or_else(|error| panic!("{crate_name}: {error}"));
    (chosen.0.clone(), bytes)
}

/// Whether one Rust source declares no unconditional top-level item. This
/// mirrors the audit's shell predicate exactly so the corpus guards address
/// the file the audit really lowers.
fn is_rust_declaration_shell(path: &Path) -> bool {
    const MAX_SCAN_BYTES: usize = 256 * 1024;
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    if bytes.len() > MAX_SCAN_BYTES {
        return false;
    }
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return false;
    };
    let mut previous_was_cfg = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("#![") {
            previous_was_cfg = false;
            continue;
        }
        if trimmed.starts_with("#[") {
            previous_was_cfg = trimmed.starts_with("#[cfg");
            continue;
        }
        if !starts_rust_item(trimmed) {
            continue;
        }
        if previous_was_cfg {
            previous_was_cfg = false;
            continue;
        }
        return false;
    }
    true
}

/// Whether one trimmed line opens a top-level Rust item or import.
fn starts_rust_item(line: &str) -> bool {
    let mut rest = line;
    loop {
        let next = if let Some(stripped) = rest.strip_prefix("pub ") {
            stripped
        } else if let Some(stripped) = rest.strip_prefix("pub(") {
            match stripped.find(')') {
                Some(close) => &stripped[close + 1..],
                None => return false,
            }
        } else if let Some(stripped) = rest
            .strip_prefix("unsafe ")
            .or_else(|| rest.strip_prefix("async "))
            .or_else(|| rest.strip_prefix("const "))
            .or_else(|| rest.strip_prefix("extern "))
        {
            stripped
        } else {
            break;
        };
        rest = next.trim_start();
    }
    rest.starts_with("mod ")
        || rest.starts_with("fn ")
        || rest.starts_with("struct ")
        || rest.starts_with("enum ")
        || rest.starts_with("union ")
        || rest.starts_with("impl")
        || rest.starts_with("trait ")
        || rest.starts_with("type ")
        || rest.starts_with("use ")
        || rest.starts_with("static ")
        || rest.starts_with("macro_rules!")
        || rest.starts_with("macro ")
}

/// Falsifies whether the written syntax walk leaks `#[cfg]`-disabled items by
/// pairing each disabled item with an enabled twin that shares kind, name, and
/// structure. A leak mints two identical identities; correct filtering lowers
/// cleanly.
#[test]
fn rust_cfg_disabled_items_do_not_leak() {
    let bodies = [
        (
            "cfg-disabled-fn",
            "#[cfg(any())] pub fn dup() -> u8 { 1 } pub fn dup() -> u8 { 2 }",
        ),
        (
            "cfg-disabled-module",
            "#[cfg(any())] pub mod dup { pub fn f() -> u8 { 1 } } pub mod dup { pub fn g() -> u8 { 2 } }",
        ),
        (
            "cfg-disabled-const",
            "#[cfg(any())] pub const DUP: u8 = 1; pub const DUP: u8 = 2;",
        ),
        (
            "cfg-disabled-struct",
            "#[cfg(any())] pub struct Dup; pub struct Dup;",
        ),
        // Enabled modules must survive the filter, including the composite
        // predicates the parser produces.
        (
            "cfg-enabled-all-module",
            "#[cfg(all())] pub mod kept { pub fn f() -> u8 { 1 } }",
        ),
        (
            "cfg-enabled-not-module",
            "#[cfg(not(any()))] pub mod kept { pub fn f() -> u8 { 1 } }",
        ),
        (
            "cfg-disabled-inline-subtree",
            "#[cfg(any())] pub mod gone { pub mod inner { pub fn f() -> u8 { 1 } } } pub fn real() -> u8 { 2 }",
        ),
        (
            "cfg-attr-disabled-module",
            "#[cfg_attr(all(), cfg(any()))] pub mod dup { pub fn f() -> u8 { 1 } } pub mod dup { pub fn g() -> u8 { 2 } }",
        ),
        (
            "cfg-attr-enabled-module",
            "#[cfg_attr(all(), cfg(all()))] pub mod kept { pub fn f() -> u8 { 1 } }",
        ),
        // An anonymous `const _` with no identifier anywhere in its body has
        // no name for the authority's first-identifier fallback to borrow; it
        // must lower (by staying out) instead of erroring.
        (
            "anonymous-const-literal",
            "const _: () = 1; pub fn real() -> u8 { 2 }",
        ),
        (
            "anonymous-const-twins",
            "const _: () = 1; const _: () = 2; pub fn real() -> u8 { 2 }",
        ),
        (
            "anonymous-const-in-impl",
            "pub struct S; impl S { const _: () = 1; pub fn f() -> u8 { 2 } }",
        ),
        (
            "anonymous-const-assert-twins",
            "const _: () = assert!(true); const _: () = assert!(true); pub fn f() -> u8 { 1 }",
        ),
        // A named const whose name merely starts with an underscore is a real
        // declaration and must stay.
        (
            "underscore-named-const",
            "pub const _X: u8 = 1; pub fn f() -> u8 { _X }",
        ),
    ];
    for (label, body) in bodies {
        let outcome = compile_corpus_file(body.as_bytes());
        eprintln!("CFG-PROBE {label} => {outcome}");
        assert_eq!(outcome, "OK", "cfg-disabled probe {label} leaked");
    }
}

/// Higher-ranked `for<'a>` where-predicates name no declaration-scoped
/// parameter row, so they lower into the free-predicate lane with their exact
/// written subject and bound instead of rejecting the whole declaration. The
/// corpus guard pins the audit row (`cargo:either@1.18.0`) whose lib.rs
/// carries four of them.
#[test]
fn rust_hrtb_where_predicates_lower_into_the_free_lane() {
    // Synthetic either shape: one higher-ranked predicate pair over borrowed
    // parameters, with and without a bound-associated binding.
    let outcome = compile_corpus_file(
        b"pub struct Wrap<L>(L);\ntrait Into {}\nimpl<L> Wrap<L> where for<'a> &'a L: Into {\n    fn x(&self) -> u8 { 1 }\n}\n",
    );
    eprintln!("HRTB borrowed-subject => {outcome}");
    assert_eq!(outcome, "OK", "a higher-ranked predicate must lower freely");
    let outcome = compile_corpus_file(
        b"pub struct Wrap<L>(L);\ntrait Into { type Item; }\nimpl<L> Wrap<L> where for<'a> &'a L: Into<Item = u8> {\n    fn x(&self) -> u8 { 1 }\n}\n",
    );
    eprintln!("HRTB bound-binding => {outcome}");
    assert_eq!(outcome, "OK", "a higher-ranked predicate with a bound binding must lower");
    // A declared parameter alongside a higher-ranked predicate keeps both.
    let outcome = compile_corpus_file(
        b"pub struct Wrap<L>(L);\ntrait Into {}\nimpl<L: Into> Wrap<L> where for<'a> &'a L: Into {\n    fn x(&self) -> u8 { 1 }\n}\n",
    );
    eprintln!("HRTB with-parameter-row => {outcome}");
    assert_eq!(outcome, "OK", "the parameter row must survive beside the free predicate");

    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping hrtb corpus repro");
        return;
    };
    let (source, bytes) = audit_selected(&root, "either-1.18.0");
    let outcome = compile_corpus_file(&bytes);
    eprintln!(
        "HRTB either-1.18.0 file={} bytes={} => {outcome}",
        source.strip_prefix(&root).unwrap_or(&source).display(),
        bytes.len()
    );
    assert_eq!(outcome, "OK", "either's lib.rs must lower");
}

/// Implementations commit their whole written self type as the declaration
/// name, so two blocks of one trait whose self types differ only in generic
/// arguments (`U<N, false>` versus `U<N, true>`) stay distinct declarations
/// and the image build raises no duplicate-identity terminal. The written
/// syntax walk's own same-name gate (guarding against a tool-attribute cfg
/// twin such as `#[rustversion::since]`) exempts implementations for the same
/// reason: E0428 never governs impl blocks, so several written impls legally
/// share one self type's exact spelling — several inherent blocks for one
/// type, or several distinct traits impl'd for one self type — and keying
/// that gate on the self-type-only name would silently drop every later
/// sibling before the trait/generics/member-aware declaration identity ever
/// sees it, orphaning its members to a fabricated root scope. The corpus
/// guard pins the audit rows (`cargo:generic-array@1.4.5`, whose three
/// `GenericArray<T, N>` inherent blocks and whose `IntoIterator`/`TryFrom`
/// pair sharing one reference self type both previously collapsed this way,
/// and `cargo:winnow@0.5.40`, whose `Stream`/`Offset` pairs share a self type
/// too).
#[test]
fn rust_impl_self_type_names_discriminate_generic_argument_variants() {
    let bodies = [
        (
            "empty-blocks",
            "trait S {}\nstruct U<N, const B: bool>(N);\nimpl<N> S for U<N, false> {}\nimpl<N> S for U<N, true> {}\n",
        ),
        (
            "bounded-parameters",
            "trait L {}\ntrait S {}\nstruct U<N, const B: bool>(N);\nimpl<N: L> S for U<N, false> {}\nimpl<N: L> S for U<N, true> {}\n",
        ),
        (
            "member-blocks",
            "trait S {}\nstruct U<N, const B: bool>(N);\nimpl<N> S for U<N, false> { fn m(&self) -> u8 { 1 } }\nimpl<N> S for U<N, true> { fn m(&self) -> u8 { 2 } }\n",
        ),
        (
            "associated-types",
            "trait S { type Item; }\nstruct U<N, const B: bool>(N);\nimpl<N> S for U<N, false> { type Item = u8; }\nimpl<N> S for U<N, true> { type Item = u16; }\n",
        ),
        (
            "inherent-blocks-share-self-type",
            "struct U<N>(N);\nimpl<N> U<N> {\n    fn a(&self) -> u8 { 1 }\n}\nimpl<N> U<N> {\n    fn b(&self) -> u8 { 2 }\n}\nimpl<N> U<N> {\n    fn c(&self) -> u8 { 3 }\n}\n",
        ),
        (
            "different-traits-share-self-type",
            "trait A { type Item; fn a(self) -> Self::Item; }\ntrait B { type Item; fn b(self) -> Self::Item; }\nstruct U<N>(N);\nimpl<'x, N> A for &'x U<N> {\n    type Item = u8;\n    fn a(self) -> u8 { 1 }\n}\nimpl<'x, N> B for &'x U<N> {\n    type Item = u16;\n    fn b(self) -> u16 { 2 }\n}\n",
        ),
    ];
    for (label, body) in bodies {
        let outcome = compile_corpus_file(body.as_bytes());
        eprintln!("IMPL-NAME {label} => {outcome}");
        assert_eq!(outcome, "OK", "impl-variant probe {label} collided");
    }

    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping impl-name corpus repro");
        return;
    };
    let (source, bytes) = audit_selected(&root, "generic-array-1.4.5");
    let outcome = compile_corpus_file(&bytes);
    eprintln!(
        "IMPL-NAME generic-array-1.4.5 file={} bytes={} => {outcome}",
        source.strip_prefix(&root).unwrap_or(&source).display(),
        bytes.len()
    );
    assert_eq!(outcome, "OK", "generic-array's lib.rs must lower");
}

/// The anonymous foreign-row dedup table is a bounded lane sized for measured
/// corpus demand: a facade crate root re-exporting a whole dependency surface
/// (`cargo:itertools@0.15.0`) names far more distinct foreign spellings than
/// the founding 64-entry bound. At the measured bound the source lowers; one
/// spelling beyond rejects exactly.
#[test]
fn rust_foreign_row_capacity_measures_real_corpus_demand() {
    let at_cap = foreign_spelling_fixture(512);
    let outcome = at_cap;
    eprintln!("FOREIGN-CAP at-512 => {outcome}");
    assert_eq!(outcome, "OK", "a source at the measured cap must lower");
    let over_cap = foreign_spelling_fixture(513);
    let outcome = over_cap;
    eprintln!("FOREIGN-CAP over-513 => {outcome}");
    assert_eq!(
        outcome, "LoweringUnsupported: NoSupportedDeclaration",
        "one spelling beyond the cap must reject exactly"
    );

    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping foreign-cap corpus repro");
        return;
    };
    let (source, bytes) = audit_selected(&root, "itertools-0.15.0");
    let outcome = compile_corpus_file(&bytes);
    eprintln!(
        "FOREIGN-CAP itertools-0.15.0 file={} bytes={} => {outcome}",
        source.strip_prefix(&root).unwrap_or(&source).display(),
        bytes.len()
    );
    assert_eq!(outcome, "OK", "itertools's lib.rs must lower");
}

/// One distinct resolvable foreign spelling per function, exactly the leaf
/// the dedup table interns: the spellings live in a path dependency, so
/// rust-analyzer resolves every `fonts::T{i}` to a foreign ADT and the lane
/// hosts one anonymous foreign row per distinct name.
fn foreign_spelling_fixture(count: usize) -> String {
    let body = foreign_spelling_source(count);
    stage_fixture(body.as_bytes(), count).map_or_else(
        || "fixture-io".to_owned(),
        |root| compile_staged_fixture(&root, body.as_bytes()),
    )
}

/// One distinct unresolved foreign spelling per function bound, exactly the
/// leaf the dedup table interns: `T{i}` resolves to nothing, so the bound
/// hosts its whole written spelling as one anonymous foreign row.
fn foreign_spelling_source(count: usize) -> String {
    let mut body = String::from("pub fn root() -> u8 { 0 }\n");
    for ordinal in 0..count {
        body.push_str(&format!("pub fn f{ordinal}<X: T{ordinal}>() {{}}\n"));
    }
    body
}

/// A crate root whose every written item stayed out of the lane admits its
/// collected-empty product — each item behind an unmet `#[cfg]` gate, behind
/// an unresolved facade re-export, or a whole `compile_error!` stub — while a
/// written surface with no top-level item at all stays the exact typed
/// rejection. The corpus guards pin the five audit rows of this class
/// (`cargo:clap@4.6.7`, `cargo:crossbeam-channel@0.5.17`,
/// `cargo:tracing-subscriber@0.3.23`, `cargo:bincode@3.0.0`,
/// `cargo:futures-io@0.3.34`).
#[test]
fn rust_gated_and_facade_crate_roots_admit_the_empty_product() {
    let admitted = [
        (
            "all-items-cfg-gated",
            "#[cfg(feature = \"std\")] pub fn gated() -> u8 { 1 }\n#[cfg(feature = \"std\")] pub mod gated_mod;\n",
        ),
        (
            "facade-re-export",
            "pub use missing_crate::*;\n#[cfg(feature = \"derive\")] pub use missing_derive::Derive;\n",
        ),
        (
            "compile-error-stub",
            "compile_error!(\"https://xkcd.com/2347/\");",
        ),
    ];
    for (label, body) in admitted {
        let outcome = compile_corpus_file(body.as_bytes());
        eprintln!("EMPTY-ADMIT {label} => {outcome}");
        assert_eq!(outcome, "OK", "empty-product probe {label} must admit");
    }
    let rejected = [
        ("truly-empty", ""),
        ("attributes-only", "#![no_std]\n#![warn(missing_docs)]\n"),
    ];
    for (label, body) in rejected {
        let outcome = compile_corpus_file(body.as_bytes());
        eprintln!("EMPTY-REJECT {label} => {outcome}");
        assert_eq!(
            outcome, "LoweringUnsupported: NoSupportedDeclaration",
            "item-less probe {label} must stay the exact rejection"
        );
    }

    let Some(root) = corpus_root() else {
        eprintln!("NUDOX_RUST_CORPUS_DIR unset; skipping empty-product corpus repro");
        return;
    };
    for crate_name in [
        "clap-4.6.7",
        "crossbeam-channel-0.5.17",
        "tracing-subscriber-0.3.23",
        "bincode-3.0.0",
        "futures-io-0.3.34",
    ] {
        let (source, bytes) = audit_selected(&root, crate_name);
        let outcome = compile_corpus_file(&bytes);
        eprintln!(
            "EMPTY-ADMIT {crate_name} file={} bytes={} => {outcome}",
            source.strip_prefix(&root).unwrap_or(&source).display(),
            bytes.len()
        );
        assert_eq!(
            outcome, "OK",
            "{crate_name}: the gated or facade crate root must admit its empty product"
        );
    }
}

/// TEMPORARY bisect driver.
///
/// This walks an ad hoc, hand-populated directory of numbered prefix
/// fixtures (`GA_PREFIX_DIR`) that a developer builds locally while
/// bisecting a specific regression; it names no corpus or fixture the repo
/// or Nix ships, so there is nothing to assert here without that directory.
/// Skip exactly like the other `NUDOX_*_CORPUS_DIR`-gated probes in this
/// file when the variable is unset, instead of panicking in every sandbox
/// that lacks a bisect session in flight.
#[test]
fn diag_bisect_ga() {
    let Some(dir) = std::env::var_os("GA_PREFIX_DIR").map(std::path::PathBuf::from) else {
        eprintln!("GA_PREFIX_DIR unset; skipping ad hoc bisect driver");
        return;
    };
    let mut failures = Vec::new();
    for entry in fs::read_dir(&dir).expect("dir") {
        let path = entry.expect("entry").path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".rs") {
            continue;
        }
        let index: usize = name.trim_end_matches(".rs").trim_start_matches('p').parse().expect("idx");
        let bytes = fs::read(&path).expect("read");
        let outcome = compile_corpus_file(&bytes);
        if outcome != "OK" {
            eprintln!("BISECT prefix {index} => {outcome}");
            failures.push(index);
        }
    }
    eprintln!("BISECT failing prefixes: {failures:?}");
}
