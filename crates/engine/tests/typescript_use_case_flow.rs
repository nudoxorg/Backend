//! TypeScript cross-file binding use-case flow with shared metering.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_typescript::legacy::{Checker, CheckerError, Report};
use backend_semantic::ir::{
    DecodedTypeFact, EntityId, EntityKind, FragmentView, NominalRef, SemanticTypeTag,
    TypeChildTarget, TypeFactSegment, TypeRef,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

#[path = "use_case_support/mod.rs"]
mod use_case_support;

const SOURCE: &[u8] = br#"export class Box<T> { constructor(value: T) { this.read = null as (left: T) => void; } }
export function take(input: Box<string>): Box<number> { return null as Box<number>; }
"#;

static CANCELLED: AtomicBool = AtomicBool::new(false);

fn try_lower(
    source: &'static [u8],
    authority: Option<&Report>,
) -> Result<backend_engine::driver::CompiledIr, CompileFailure<'static>> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-authority-test",
    )
    .unwrap();
    let diagnostic: &'static mut [u8] = Box::leak(Box::new([0; 4096]));
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: authority.map_or(SemanticAuthorityInput::None, |report| {
                SemanticAuthorityInput::TypeScript { report }
            }),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &CANCELLED,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: Path::new("/tmp"),
        },
    )
}

fn view(source: &'static [u8], authority: &Report) -> FragmentView<'static> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-authority-test",
    )
    .unwrap();
    let diagnostic: &'static mut [u8] = Box::leak(Box::new([0; 4096]));
    let output: &'static mut [u8] = Box::leak(vec![0; 8 * 1024 * 1024].into_boxed_slice());
    backend_engine::driver::compile(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::TypeScript { report: authority },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &CANCELLED,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: Path::new("/tmp"),
        },
        backend_engine::driver::CompileOutput {
            fragment_output: output,
        },
    )
    .unwrap()
    .fragment
}

fn entities(view: &FragmentView<'_>) -> Vec<(u32, Vec<u8>, EntityKind)> {
    let atoms: Vec<_> = view
        .atoms()
        .map(|a| (a.ordinal.raw, a.bytes.to_vec()))
        .collect();
    view.entities()
        .map(|e| {
            (
                e.entity.raw,
                atoms
                    .iter()
                    .find(|(n, _)| *n == e.name.raw)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default(),
                e.kind,
            )
        })
        .collect()
}

fn named(view: &FragmentView<'_>, name: &[u8]) -> (u32, EntityKind) {
    entities(view)
        .into_iter()
        .find(|(_, n, _)| n == name)
        .map(|(id, _, kind)| (id, kind))
        .unwrap()
}

fn facts<'a>(view: &'a FragmentView<'a>) -> Vec<DecodedTypeFact<'a>> {
    view.type_facts().into_iter().flatten().flatten().collect()
}

fn parameter_count(view: &FragmentView<'_>, name: &[u8]) -> usize {
    entities(view)
        .into_iter()
        .filter(|(_, entity_name, kind)| entity_name == name && *kind == EntityKind::Parameter)
        .count()
}

fn declared_fact<'a>(
    view: &'a FragmentView<'a>,
    owner: u32,
) -> Option<DecodedTypeFact<'a>> {
    facts(view).into_iter().find(|fact| {
        fact.owner.raw == owner && fact.segment == TypeFactSegment::Declared
    })
}

fn child_type_fact<'a>(
    view: &'a FragmentView<'a>,
    type_id: u32,
) -> Option<DecodedTypeFact<'a>> {
    declared_fact(view, type_id)
}

fn mentions_box<'a>(
    view: &'a FragmentView<'a>,
    fact: &DecodedTypeFact<'a>,
    box_owner: u32,
) -> bool {
    if fact.record.text == Some(b"Box") {
        return true;
    }
    if fact.record.nominal == Some(NominalRef::Local(EntityId::new(box_owner))) {
        return true;
    }
    if fact.record.tag != SemanticTypeTag::Apply {
        return false;
    }
    let start = fact.record.children.start;
    let end = start + fact.record.children.length;
    let children = view
        .type_facts()
        .and_then(|facts| facts.children().ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|child| child.ordinal >= start && child.ordinal < end);
    for child in children {
        if let TypeChildTarget::Type(TypeRef::Local(type_id)) = child.child.target {
            if let Some(child_fact) = child_type_fact(view, type_id.raw) {
                if mentions_box(view, &child_fact, box_owner) {
                    return true;
                }
            }
        }
    }
    false
}

fn checker_missing(error: &CheckerError) -> bool {
    matches!(
        error,
        CheckerError::Spawn { .. } | CheckerError::ToolingUnavailable { .. }
    )
}

#[test]
fn cross_file_bindings() {
    let authority = match Checker::default().run(TypeScriptSource::TypeScript, SOURCE) {
        Ok(report) => report,
        Err(error) if checker_missing(&error) => {
            use_case_support::skip("typescript", "cross-file-bindings", "checker-missing")
                .unwrap();
            return;
        }
        Err(error) => panic!("checker failed: {error}"),
    };

    let timer = use_case_support::CompileTimer::start();
    let compiled = match try_lower(SOURCE, Some(&authority)) {
        Ok(compiled) => compiled,
        Err(failure) => panic!("lowering failed: {failure:?}"),
    };
    let ir = &compiled.ir;
    use_case_support::finish("typescript", "cross-file-bindings", timer.elapsed(), ir).unwrap();

    let decoded = view(SOURCE, &authority);
    assert_eq!(parameter_count(&decoded, b"left"), 1);
    assert_eq!(use_case_support::count_named(ir, EntityKind::Record, b"Box"), 1);
    assert_eq!(use_case_support::count_named(ir, EntityKind::Function, b"take"), 1);

    let box_owner = named(&decoded, b"Box").0;
    let input_owner = named(&decoded, b"input").0;
    let input_type = declared_fact(&decoded, input_owner).unwrap();
    assert!(
        mentions_box(&decoded, &input_type, box_owner),
        "take's input parameter must mention Box"
    );
}
