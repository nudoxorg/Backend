//! Focused repro for the `npm:@types/react-dom@18.3.5` audit row.
//!
//! The audited file (`test-utils/index.d.ts`, 14343 bytes) declares
//! `mockComponent(...): typeof ReactTestUtils` where `ReactTestUtils` is a
//! namespace import of the whole module. The checker computes that namespace
//! type as one anonymous record with more exported members than the lane
//! holds per row, and the computed lane raised its typed `TypeChildCapacity`
//! rejection instead of committing the record. Wide anonymous records now
//! fold into per-row-bounded record rows exactly as wide unions fold: no
//! member is lost, none nests deeper than the fold requires, every member
//! keeps its source-backed name, and each folded row is named by the member
//! that closes its chunk. Records at or under the per-row bound are unchanged
//! byte for byte.
//!
//! The synthetic half proves the fold on a hand-built checker report whose
//! namespace type carries 130 members; the corpus half proves the exact
//! audited file end to end.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_typescript::legacy::{
    Checker, Declaration, Origin, Report, TypeTree,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static LEAK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-wide-namespace-repro",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn leak(source: &[u8]) -> &'static [u8] {
    Box::leak(Vec::from(source).into_boxed_slice())
}

fn diagnostic() -> &'static mut [u8] {
    let _ = LEAK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Box::leak(Box::new([0_u8; 8192]))
}

/// Drives the full `LowerIr` semantic compile exactly as the audit does, on
/// a dedicated stack: the checker-computed namespace types nest deeply, and
/// debug-build lowering frames need more margin than a default test thread
/// carries (the lane itself is depth-bounded; see `MAX_TYPE_DEPTH`).
fn lower_semantic(source: &'static [u8], authority: &'static Report) -> String {
    let outcome = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let cancelled = AtomicBool::new(false);
            let mut fragment_output = vec![0_u8; 16 * 1024 * 1024];
            match compile_semantic(
                CompileRequest {
                    profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                    stage: Stage::LowerIr,
                    source,
                    declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
                    toolchain: ToolchainSelection::ResolvedNative(toolchain()),
                    authority: SemanticAuthorityInput::TypeScript { report: authority },
                    control: CompileControl {
                        deadline: Instant::now() + Duration::from_secs(240),
                        cancelled: &cancelled,
                    },
                },
                CompileScratch {
                    diagnostic_output: diagnostic(),
                    native_work: Path::new("/tmp"),
                },
                backend_engine::driver::CompileOutput {
                    fragment_output: &mut fragment_output,
                },
            ) {
                Ok(_) => "OK".to_owned(),
                Err(failure) => format!("{failure:?}"),
            }
        })
        .expect("the lowering thread spawns")
        .join();
    match outcome {
        Ok(outcome) => outcome,
        Err(_) => "lowering thread panicked".to_owned(),
    }
}

/// A hand-built checker report: `probe` is one computed namespace-object type
/// over 130 source-spelled members, the exact shape the checker derives for
/// `typeof Ns` when the module exports more members than one row holds.
fn wide_report(source: &[u8], members: usize) -> Report {
    let text = String::from_utf8_lossy(source).into_owned();
    let name_start = u32::try_from(text.find("probe").expect("probe is spelled")).expect("span");
    let name_end = name_start + "probe".len() as u32;
    let tree_members: Vec<backend_frontend_typescript::legacy::ObjectMember> = (0..members)
        .map(|index| backend_frontend_typescript::legacy::ObjectMember {
            name: format!("m{index}"),
            optional: false,
            readonly: false,
            member_type: TypeTree::Primitive {
                name: "string".to_owned(),
            },
        })
        .collect();
    Report {
        schema_version: 1,
        source_digest: backend_frontend_typescript::legacy::source_digest(source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        declaration_file: true,
        diagnostics: Box::new([]),
        declarations: Box::new([Declaration {
            name_start,
            name_end,
            origin: Origin::Computed,
            overload_index: None,
            r#type: Some(TypeTree::Object {
                members: tree_members,
            }),
        }]),
        references: Box::new([]),
        narrowings: Box::new([]),
    }
}

/// 130 source-spelled member names as separate small declarations, plus the
/// namespace-typed owner: the report's namespace type names each member, and
/// every name is spelled in the source without any oversized written literal
/// (the whole point is a shape only the checker's computed type has).
fn wide_source(members: usize) -> Vec<u8> {
    let mut source = Vec::new();
    for index in 0..members {
        source.extend_from_slice(format!("declare const m{index}: string;\n").as_bytes());
    }
    source.extend_from_slice(b"declare const probe: typeof MissingNamespace;\n");
    source
}

#[test]
fn typescript_wide_computed_namespace_records_fold() {
    const MEMBERS: usize = 130;
    let source: &'static [u8] = leak(&wide_source(MEMBERS));
    let authority: &'static Report = Box::leak(Box::new(wide_report(source, MEMBERS)));
    let outcome = lower_semantic(source, authority);
    eprintln!("WIDE-NAMESPACE {MEMBERS} members => {outcome}");
    assert!(
        outcome.starts_with("OK"),
        "a namespace record wider than one row must fold, not reject: {outcome}"
    );
}

/// The corpus-guarded half: the exact audited file, through the real checker,
/// must produce output instead of the capacity rejection.
#[test]
fn typescript_react_dom_test_utils_lowers() {
    let Some(root) = std::env::var_os("NUDOX_TYPESCRIPT_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("NUDOX_TYPESCRIPT_CORPUS_DIR unset; skipping the corpus-guarded row");
        return;
    };
    let package = root.join("@types/react-dom-18.3.5");
    let entry = package.join("test-utils/index.d.ts");
    let Ok(bytes) = std::fs::read(&entry) else {
        panic!("corpus pinned entry must exist at {}", entry.display());
    };
    assert!(
        bytes.windows(b"typeof ReactTestUtils".len())
            .any(|window| window == b"typeof ReactTestUtils"),
        "the pinned corpus entry must still carry the namespace-import type"
    );
    let report: &'static Report = Box::leak(Box::new(
        Checker::default()
            .run_in_package(TypeScriptSource::TypeScript, &bytes, &package)
            .expect("the corpus entry checks"),
    ));
    let outcome = lower_semantic(leak(&bytes), report);
    eprintln!("REACT-DOM test-utils/index.d.ts => {outcome}");
    assert!(
        outcome.starts_with("OK"),
        "the audited react-dom entry must lower, got {outcome}"
    );
}
