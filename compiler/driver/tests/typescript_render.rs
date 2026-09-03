//! Golden rendering over the TypeScript semantic lane.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use compiler_ir::{EntityId, Ir, ItemKind};
use compiler_languages_typescript::{Checker, CheckerError};
use compiler_vocabulary::{LanguageProfile, Stage, TypeScriptSource};

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
    ("this", b"function make(): this { return this; }\nconst value = this;"),
    ("narrowing", b"let widened: number = 0;\nwidened = \"text\";"),
    ("jsdoc", b"/** Adds two values.\n * @param left first value\n * @param right second value\n * @returns their sum\n */\nfunction add(left: number, right: number): number { return left + right; }"),
    ("overloads", b"declare function g(value: number): string;\ndeclare function g(value: string): number;\nconst a = g(1);\nconst b = g(\"x\");"),
    ("extensions", b"interface Box<T> { value: T }"),
];

fn compile(source: &'static [u8]) -> Option<Ir> {
    let report = match Checker::default().run(TypeScriptSource::TypeScript, source) {
        Ok(report) => report,
        Err(
            CheckerError::ModuleUnavailable { .. }
            | CheckerError::Spawn { .. }
            | CheckerError::ToolingUnavailable { .. },
        ) => return None,
        Err(error) => panic!("checker report: {error}"),
    };
    let tool = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-render",
    )
    .expect("tool");
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::TypeScript { report: &report },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    )
    .expect("IR")
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
        .expect("semantic type");
    assert_eq!(
        ir.display_type(ty).expect("type display").to_string(),
        expected,
        "{name} type"
    );
}

#[test]
fn typescript_feature_classes_render() {
    let Some(ir) = compile(CASES[0].1) else {
        return;
    };
    signature(
        &ir,
        "Node",
        ItemKind::Trait,
        "/* visibility unknown */ trait Node",
    );
    let Some(ir) = compile(CASES[1].1) else {
        return;
    };
    signature(
        &ir,
        "User",
        ItemKind::Trait,
        "/* visibility unknown */ trait User",
    );
    signature(
        &ir,
        "UserImpl",
        ItemKind::Record,
        "/* visibility unknown */ struct UserImpl",
    );
    let Some(ir) = compile(CASES[2].1) else {
        return;
    };
    signature(
        &ir,
        "Holder",
        ItemKind::Trait,
        "/* visibility unknown */ trait Holder",
    );
    signature(
        &ir,
        "Pair",
        ItemKind::TypeAlias,
        "/* visibility unknown */ type Pair",
    );
    type_of(&ir, "h", ItemKind::Static, "Holder<number>");
    let Some(ir) = compile(CASES[3].1) else {
        return;
    };
    signature(
        &ir,
        "Cond",
        ItemKind::TypeAlias,
        "/* visibility unknown */ type Cond",
    );
    let Some(ir) = compile(CASES[4].1) else {
        return;
    };
    signature(
        &ir,
        "Readonlyify",
        ItemKind::TypeAlias,
        "/* visibility unknown */ type Readonlyify",
    );
    let Some(ir) = compile(CASES[5].1) else {
        return;
    };
    signature(
        &ir,
        "Greet",
        ItemKind::TypeAlias,
        "/* visibility unknown */ type Greet",
    );
    let Some(ir) = compile(CASES[6].1) else {
        return;
    };
    signature(
        &ir,
        "Literals",
        ItemKind::TypeAlias,
        "/* visibility unknown */ type Literals",
    );
    let Some(ir) = compile(CASES[7].1) else {
        return;
    };
    signature(
        &ir,
        "Box",
        ItemKind::Record,
        "/* visibility unknown */ struct Box",
    );
    let Some(ir) = compile(CASES[8].1) else {
        return;
    };
    type_of(&ir, "x", ItemKind::Static, "number");
    let Some(ir) = compile(CASES[9].1) else {
        return;
    };
    signature(
        &ir,
        "make",
        ItemKind::Function,
        "/* visibility unknown */ fn make() -> this",
    );
    let Some(ir) = compile(CASES[10].1) else {
        return;
    };
    type_of(&ir, "widened", ItemKind::Static, "number");
    let Some(ir) = compile(CASES[11].1) else {
        return;
    };
    signature(
        &ir,
        "add",
        ItemKind::Function,
        "/* visibility unknown */ fn add(left: number, right: number) -> number",
    );
    let docs = ir
        .display_docs(item(&ir, "add", ItemKind::Function))
        .expect("docs")
        .to_string();
    assert_eq!(docs, "Adds two values.");
    assert_eq!(
        ir.embedding_text(
            item(&ir, "add", ItemKind::Function),
            compiler_ir::EmbeddingProfile::DOCUMENTED,
        )
        .expect("embedding")
        .to_string(),
        "/* visibility unknown */ fn add(left: number, right: number) -> number\n\nAdds two values."
    );
    let Some(ir) = compile(CASES[12].1) else {
        return;
    };
    signature(
        &ir,
        "g",
        ItemKind::Function,
        "/* visibility unknown */ fn g(value: ?unsupported) -> ?unsupported",
    );
    type_of(&ir, "a", ItemKind::Static, "string");
    let Some(ir) = compile(CASES[13].1) else {
        return;
    };
    signature(
        &ir,
        "Box",
        ItemKind::Trait,
        "/* visibility unknown */ trait Box",
    );
    let plane = ir.storage_columns().language_extensions.typescript;
    assert!(plane.facts.iter().any(|fact| {
        ir.type_parameters(fact.type_parameters)
            .is_some_and(|parameters| parameters.len() == 1)
    }));
}
