//! Golden rendering over the TypeScript semantic lane.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_semantic::ir::{EntityId, Ir, ItemKind, TypeExpr, UnknownReason, UnknownType};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

const CASES: &[(&str, &[u8])] = &[
    ("recursive", b"interface Node { next: Node | null }"),
    ("nominal", b"interface User { name: string }\nclass UserImpl { name: string = ''; }"),
    ("generic", b"interface Holder<T> { value: T }\ntype Pair<K, V> = [K, V];\nconst h: Holder<number> = { value: 1 };"),
    ("conditional", b"type Cond<T> = T extends string ? \"s\" : \"n\";"),
    ("mapped", b"type Readonlyify<T> = { readonly [K in keyof T]: T[K] };"),
    ("template", b"type Greet = `hi ${string}`;"),
    ("literals", b"type Literals = \"ok\" | 42 | 1n | true;"),
    ("self-nominal", b"class Box { self(): this { return this; } }"),
    ("inference", b"let x = 7;\nlet w: number = 0;\nw = \"t\";"),
    ("this", b"class Cell { value = 1; self(): this { return this; } pair(): [this, this] { return [this, this]; } }"),
    ("narrowing", b"let widened: number = 0;\nwidened = \"text\";"),
    ("jsdoc", b"/** Adds two values.\n * @param left first value\n * @param right second value\n * @returns their sum\n */\nfunction add(left: number, right: number): number { return left + right; }"),
    ("overloads", b"declare function g(value: number): string;\ndeclare function g(value: string): number;\nconst a = g(1);\nconst b = g(\"x\");"),
    ("extensions", b"interface Box<T> { value: T }"),
    (
        "anonymous-callable",
        b"interface Paint { rgb: (red: number, green: number, blue: number) => void; }\ntype Variadic = (label: string, ...values: number[]) => void;",
    ),
];

fn compile(source: &'static [u8]) -> Ir {
    let tool = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-render",
    )
    .expect("tool");
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let result = compile_ir(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    );
    result
        .unwrap_or_else(|error| {
            panic!("TypeScript semantic authority (real checker) required: {error:?}")
        })
        .ir
        .into()
}

fn item(ir: &Ir, name: &'static str, kind: ItemKind) -> EntityId {
    ir.items()
        .find(|item| item.name() == name.as_bytes() && item.kind() == kind)
        .map(|item| item.id())
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"))
}

fn signature(ir: &Ir, name: &'static str, kind: ItemKind, expected: &str) {
    let actual = ir
        .signature(item(ir, name, kind))
        .expect("signature")
        .to_string();
    assert_eq!(actual, expected, "{name} signature");
}

fn type_of(ir: &Ir, name: &'static str, kind: ItemKind, expected: &str) {
    let id = item(ir, name, kind);
    let ty = ir
        .item(id)
        .and_then(|item| item.semantic_type())
        .unwrap_or_else(|| panic!("{name} semantic type missing"));
    assert_eq!(
        ir.display_type(ty).expect("type display").to_string(),
        expected,
        "{name} type"
    );
}

fn declared_unknown_with_computed_type(ir: &Ir, name: &'static str, kind: ItemKind) {
    let id = item(ir, name, kind);
    // An unannotated declaration's declared type is the explicit
    // `Unknown(Unannotated)` row, never the checker's inference (f7af7b808:
    // a source `Unknown` is semantic truth, not absence).
    let declared = ir
        .item(id)
        .and_then(|item| item.semantic_type())
        .and_then(|ty| ir.ty(ty));
    assert!(
        matches!(
            declared,
            Some(TypeExpr::Unknown(UnknownType {
                reason: UnknownReason::Unannotated,
                spelling: None,
            }))
        ),
        "{name} must keep its declared type unknown, got {declared:?}"
    );
    assert!(
        ir.storage_columns()
            .language_extensions
            .typescript
            .get(id)
            .and_then(|facts| facts.observed)
            .is_some(),
        "{name} observed type must remain in the TypeScript extension plane"
    );
}

#[test]
fn recursive_renders() {
    let ir = compile(CASES[0].1);
    signature(&ir, "Node", ItemKind::Trait, "trait Node");
}

#[test]
fn nominal_renders() {
    let ir = compile(CASES[1].1);
    signature(&ir, "User", ItemKind::Trait, "trait User");
    signature(&ir, "UserImpl", ItemKind::Record, "struct UserImpl");
}

#[test]
fn generic_renders() {
    let ir = compile(CASES[2].1);
    signature(&ir, "Holder", ItemKind::Trait, "trait Holder");
    signature(&ir, "Pair", ItemKind::TypeAlias, "type Pair = (K, V)");
    item(&ir, "h", ItemKind::Constant);
}

#[test]
fn conditional_renders() {
    let ir = compile(CASES[3].1);
    signature(
        &ir,
        "Cond",
        ItemKind::TypeAlias,
        "type Cond = [T] extends str ? \"s\" : \"n\"",
    );
}

#[test]
fn mapped_renders() {
    let ir = compile(CASES[4].1);
    // Mapped `keyof`/indexed-access operands have no declared-plane record
    // representation; they stay reasoned unknowns that keep their written
    // spelling (f7af7b808). The written `readonly` survives as the explicit
    // `Add` modifier, which the renderer spells in its canonical `+` form.
    signature(
        &ir,
        "Readonlyify",
        ItemKind::TypeAlias,
        "type Readonlyify = { +readonly [K in ?no-ir-representation(keyof T)]: ?no-ir-representation(T[K]) }",
    );
}

#[test]
fn template_renders() {
    let ir = compile(CASES[5].1);
    // Template text parts are staged as text children (f7af7b808), so the
    // literal segment renders alongside the structurally decoded placeholder.
    signature(&ir, "Greet", ItemKind::TypeAlias, "type Greet = `hi ${str}`");
}

#[test]
fn literals_render() {
    let ir = compile(CASES[6].1);
    signature(
        &ir,
        "Literals",
        ItemKind::TypeAlias,
        "type Literals = \"ok\" | 42 | 1n | true",
    );
}

#[test]
fn self_nominal_renders() {
    let ir = compile(CASES[7].1);
    signature(&ir, "Box", ItemKind::Record, "struct Box");
}

#[test]
fn inference_renders() {
    let ir = compile(CASES[8].1);
    declared_unknown_with_computed_type(&ir, "x", ItemKind::Static);
}

#[test]
fn this_renders() {
    let ir = compile(CASES[9].1);
    signature(&ir, "Cell", ItemKind::Record, "struct Cell");
}

#[test]
fn narrowing_renders() {
    let ir = compile(CASES[10].1);
    type_of(&ir, "widened", ItemKind::Static, "f64");
}

#[test]
fn jsdoc_renders() {
    let ir = compile(CASES[11].1);
    signature(
        &ir,
        "add",
        ItemKind::Function,
        "fn add(left: f64, right: f64) -> f64",
    );
    let docs = ir
        .display_docs(item(&ir, "add", ItemKind::Function))
        .expect("docs")
        .to_string();
    assert_eq!(
        docs,
        "Adds two values. @param left first value @param right second value @returns their sum"
    );
    assert_eq!(
        ir.embedding_text(
            item(&ir, "add", ItemKind::Function),
            backend_semantic::ir::semantic_render::EmbeddingProfile::DOCUMENTED,
        )
        .expect("embedding")
        .to_string(),
        "fn add(left: f64, right: f64) -> f64\n\nAdds two values. @param left first value @param right second value @returns their sum"
    );
}

#[test]
fn overloads_render() {
    let ir = compile(CASES[12].1);
    signature(&ir, "g", ItemKind::Function, "fn g(value: f64) -> str");
    declared_unknown_with_computed_type(&ir, "a", ItemKind::Constant);
}

#[test]
fn extensions_render() {
    let ir = compile(CASES[13].1);
    signature(&ir, "Box", ItemKind::Trait, "trait Box");
    let plane = ir.storage_columns().language_extensions.typescript;
    assert!(plane.facts.iter().any(|fact| {
        ir.type_parameters(fact.type_parameters)
            .is_some_and(|parameters| parameters.len() == 1)
    }));
}

#[test]
fn anonymous_callable_renders() {
    // A function type's parameters are never `Parameter` carrier facts (only
    // a declared executable's parameters are); the label must come from the
    // written pattern, not from the target row's own (unrelated) fact name.
    let ir = compile(CASES[14].1);
    type_of(
        &ir,
        "rgb",
        ItemKind::Field,
        "fn(red: f64, green: f64, blue: f64) -> void",
    );
    signature(
        &ir,
        "Variadic",
        ItemKind::TypeAlias,
        "type Variadic = fn(label: str, ...values: []f64) -> void",
    );
}

#[test]
fn forward_nominal_render_truth_names_the_later_class() {
    let ir = compile(b"export const a = new B(); export class B {}");
    let a = item(&ir, "a", ItemKind::Constant);
    let _computed = ir
        .storage_columns()
        .language_extensions
        .typescript
        .get(a)
        .and_then(|facts| facts.observed)
        .expect("forward nominal observed type");
    let b = item(&ir, "B", ItemKind::Record);
    let b_type = ir
        .item(b)
        .and_then(|item| item.semantic_type())
        .expect("forward nominal class type");
    assert_eq!(
        ir.display_type(b_type)
            .expect("forward nominal class display")
            .to_string(),
        "B"
    );
}
