//! Focused repro for the `npm:@types/async@3.2.24` audit row.
//!
//! The package's ambient `index.d.ts` declares one interface index signature
//! (`Dictionary<T>`) and several anonymous object-literal index signatures
//! (the `transform` overload group), every one spelling `[key: string]: …`.
//! Lowering used to mint one Field fact per emission route, so two
//! byte-identical rows carried consecutive ordinals and the identity builder
//! rejected the family as `DuplicateDeclarationIdentity`. The type-literal
//! member walk now owns the fact and the syntax walk skips what it already
//! registered, so each written declaration keeps exactly one row whose name
//! is the exact written spelling — and the identity frame stays coordinate
//! free: nothing in a minted version embeds a raw source offset.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_typescript::legacy::{Checker, Report};
use backend_semantic::ir::{EntityId, EntityVersion, Ir, ItemKind};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static LEAK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// The exact index-signature surface of the corpus `index.d.ts` (lines 1-8
/// and 700-710): one interface member plus the `transform` overload group's
/// anonymous object literals, six written `[key: string]: …` declarations.
const ASYNC_SHAPE: &[u8] = b"
export as namespace async;

export interface Dictionary<T> {
    [key: string]: T;
}

export function transform<T, R, E = Error>(
    arr: { [key: string]: T },
    iteratee: (acc: { [key: string]: R }, item: T, key: string, callback: (error?: E) => void) => void,
    callback?: AsyncResultObjectCallback<T, E>,
): void;

export function transform<T, R, E = Error>(
    arr: { [key: string]: T },
    acc: { [key: string]: R },
    iteratee: (acc: { [key: string]: R }, item: T, key: string, callback: (error?: E) => void) => void,
    callback?: AsyncResultObjectCallback<T, E>,
): void;
";

/// An ambient witness report: the lowering drives from the written syntax,
/// the checker report only proves the bytes are a declaration file.
fn ambient_report(source: &[u8]) -> Report {
    let digest = backend_frontend_typescript::legacy::source_digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Report {
        schema_version: 1,
        source_digest: digest,
        declaration_file: true,
        diagnostics: Box::new([]),
        declarations: Box::new([]),
        references: Box::new([]),
        narrowings: Box::new([]),
    }
}

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-async-index-signature-test",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn leak(source: &[u8]) -> &'static [u8] {
    Box::leak(Vec::from(source).into_boxed_slice())
}

fn diagnostic() -> &'static mut [u8] {
    let _ = LEAK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Box::leak(Box::new([0_u8; 4096]))
}

fn lower_ambient(source: &'static [u8], authority: &Report) -> backend_engine::driver::CompiledIr {
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain()),
            authority: SemanticAuthorityInput::TypeScript { report: authority },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &CANCELLED,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic(),
            native_work: Path::new("/tmp"),
        },
    )
    .expect("the ambient index-signature surface must lower")
}

/// One (name, kind, parent id, minted version) row per entity, in lane order,
/// plus each entity's name keyed by id. The version carries the
/// coordinate-free family, variant, and core payload, so equality here pins
/// every minted identity plane at once.
fn rows(
    ir: &Ir,
) -> (
    std::collections::BTreeMap<u32, Vec<u8>>,
    Vec<(Vec<u8>, ItemKind, Option<u32>, EntityVersion)>,
) {
    let mut names = std::collections::BTreeMap::new();
    for item in ir.items() {
        names.insert(item.id().raw, item.name().to_vec());
    }
    let list = ir
        .items()
        .map(|item| {
            (
                item.name().to_vec(),
                item.kind(),
                item.parent().map(|parent: EntityId| parent.raw),
                item.version(),
            )
        })
        .collect();
    (names, list)
}

/// Replaces each row's parent id with the owner's name, so two lowerings of
/// identical content compare without borrowing either run's ordinals.
fn named(
    names: &std::collections::BTreeMap<u32, Vec<u8>>,
    list: &[(Vec<u8>, ItemKind, Option<u32>, EntityVersion)],
) -> Vec<(Vec<u8>, ItemKind, Vec<u8>, EntityVersion)> {
    list.iter()
        .map(|(name, kind, parent, version)| {
            let parent_name = parent
                .and_then(|parent| names.get(&parent).cloned())
                .unwrap_or_default();
            (name.clone(), *kind, parent_name, *version)
        })
        .collect()
}

#[test]
fn async_object_index_signatures_commit_exactly_one_row_per_declaration() {
    let source: &'static [u8] = leak(ASYNC_SHAPE);
    let authority = ambient_report(source);
    let compiled = lower_ambient(source, &authority);
    let fields: Vec<_> = compiled
        .ir
        .items()
        .filter(|item| item.kind() == ItemKind::Field)
        .collect();
    assert_eq!(
        fields.len(),
        6,
        "one Field row per written index signature, not one per emission route"
    );
    let mut owners = Vec::new();
    for field in &fields {
        assert_eq!(field.name(), b"[key: string]: ");
        assert!(
            field.semantic_type().is_some(),
            "an index signature keeps its annotated element type"
        );
        let parent = field
            .parent()
            .expect("an index signature member has an owner");
        assert!(
            !owners.contains(&parent),
            "two same-spelled index signatures must not share one owner"
        );
        owners.push(parent);
    }
}

/// A uniform offset shift (a leading comment) moves every byte coordinate but
/// changes no declaration. Every minted entity version — family, variant,
/// and core payload — must be identical, proving the span-derived index
/// signature names and their identity frames carry no raw source offset.
#[test]
fn async_row_identity_is_coordinate_free_under_an_offset_shift() {
    let source: &'static [u8] = leak(ASYNC_SHAPE);
    let authority = ambient_report(source);
    let (unshifted_names, unshifted_rows) = rows(&lower_ambient(source, &authority).ir);

    let mut shifted_source = b"// shifts every later byte coordinate\n".to_vec();
    shifted_source.extend_from_slice(ASYNC_SHAPE);
    let shifted: &'static [u8] = leak(&shifted_source);
    let shifted_authority = ambient_report(shifted);
    let (shifted_names, shifted_rows) = rows(&lower_ambient(shifted, &shifted_authority).ir);

    let unshifted_fields = unshifted_rows
        .iter()
        .filter(|(_, kind, _, _)| *kind == ItemKind::Field)
        .count();
    let shifted_fields = shifted_rows
        .iter()
        .filter(|(_, kind, _, _)| *kind == ItemKind::Field)
        .count();
    assert_eq!(unshifted_fields, 6);
    assert_eq!(unshifted_fields, shifted_fields);

    assert_eq!(
        named(&unshifted_names, &unshifted_rows),
        named(&shifted_names, &shifted_rows),
        "an offset shift must not move any name, kind, parentage name, or minted version"
    );
}

/// The corpus-guarded half of the row: the exact `index.d.ts` bytes the audit
/// selects lower end to end with the real checker report, one Field row per
/// written index signature.
#[test]
fn async_corpus_index_lowers_with_one_row_per_index_signature() {
    let Some(root) = std::env::var_os("NUDOX_TYPESCRIPT_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("NUDOX_TYPESCRIPT_CORPUS_DIR unset; skipping the corpus-guarded row");
        return;
    };
    let package = root.join("@types/async-3.2.24");
    let entry = package.join("index.d.ts");
    let Ok(bytes) = std::fs::read(&entry) else {
        panic!(
            "corpus pinned @types/async-3.2.24/index.d.ts must exist at {}",
            entry.display()
        );
    };
    let written_signatures = bytes
        .windows(b"[key: string]:".len())
        .filter(|window| window == b"[key: string]:")
        .count();
    assert!(
        written_signatures > 0,
        "the pinned corpus entry lost its index signatures"
    );
    let report = Checker::default()
        .run_in_package(TypeScriptSource::TypeScript, &bytes, &package)
        .expect("the corpus entry checks");
    assert!(
        report.declaration_file,
        "the corpus entry classifies as an ambient declaration file"
    );
    let source: &'static [u8] = leak(&bytes);
    let compiled = lower_ambient(source, &report);
    let fields = compiled
        .ir
        .items()
        .filter(|item| item.kind() == ItemKind::Field && item.name() == b"[key: string]: ")
        .count();
    assert_eq!(
        fields, written_signatures,
        "one Field row per written index signature in the corpus entry"
    );
}
