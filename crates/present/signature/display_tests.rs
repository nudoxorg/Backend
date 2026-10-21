use super::*;
use backend_library::{DeclarationKind, SourceExcerpt, SourceExcerptExtent};

fn projected(text: &str, language: Language, kind: DeclarationKind) -> Option<String> {
    Signature::from_declaration(text, language, Some(kind), "", &SourceExcerpt::NotCaptured)
        .map(|signature| signature.text())
}

#[test]
fn written_polyglot_headers_compact_layout_without_changing_punctuation() {
    for (language, source, expected) in [
        (
            Language::TypeScript,
            "export function fetchApi<T>(\n  url: string,\n  options: RequestInit,\n): Promise<T>",
            "export function fetchApi<T>(url: string, options: RequestInit,): Promise<T>",
        ),
        (
            Language::Python,
            "async def delete_provider_model(\n    request: Request,\n    url_idx: int,\n    user=Depends(get_admin_user),\n):",
            "async def delete_provider_model(request: Request, url_idx: int, user=Depends(get_admin_user),):",
        ),
        (
            Language::Go,
            "func Expand(\n    mapping func(string) string,\n    s string,\n) string",
            "func Expand(mapping func(string) string, s string,) string",
        ),
        (
            Language::Rust,
            "pub fn try_from_vec<T>(\n    data: Vec<T>,\n) -> Result<Self, InvalidFont>\nwhere\n    T: Clone,",
            "pub fn try_from_vec<T>(data: Vec<T>,) -> Result<Self, InvalidFont> where T: Clone,",
        ),
    ] {
        let retained = source.to_owned();
        assert_eq!(
            projected(source, language, DeclarationKind::Function).as_deref(),
            Some(expected)
        );
        assert_eq!(source, retained, "source signature stays untouched");
        assert_eq!(
            Signature::tokenize(source, language).text(),
            source,
            "lexical API remains exact"
        );
    }
}

#[test]
fn declaration_list_preview_folds_layout_before_the_existing_character_cap() {
    let mut source = String::from("export function load(\r\n");
    for index in 0..80 {
        source.push_str(&format!("  value_{index}: string,\r\n"));
    }
    source.push_str("): Promise<void>");
    let retained = source.clone();
    let old_preview = Signature::preview(&source, Language::TypeScript).text();
    let preview = Signature::preview_declaration(
        &source,
        Language::TypeScript,
        Some(DeclarationKind::Function),
        "load",
        &SourceExcerpt::NotCaptured,
    )
    .expect("written declaration preview")
    .text();

    assert!(old_preview.chars().count() <= 240);
    assert!(preview.chars().count() <= 240);
    assert!(!old_preview.contains("value_10: string"));
    assert!(preview.starts_with("export function load(value_0: string, value_1: string,"));
    assert!(preview.contains("value_10: string"));
    assert!(preview.ends_with('…'));
    assert_eq!(preview.chars().count(), 240);
    assert_eq!(
        source, retained,
        "projection never rewrites captured source"
    );
}

#[test]
fn literal_contents_singleton_tuples_comments_and_lifetimes_remain_exact() {
    for (language, source, expected) in [
        (
            Language::Python,
            "def f(\n text='''first\n  second \"quote\"''',\n):",
            "def f(text='''first\n  second \"quote\"''',):",
        ),
        (
            Language::Python,
            "def f(value=(1,\n)):",
            "def f(value=(1,)):",
        ),
        (
            Language::Go,
            "const Help = `first\n  second`\n",
            "const Help = `first\n  second`",
        ),
        (
            Language::Rust,
            "const HELP: &str = r##\"first\n  second \"quoted\"\"##;\n",
            "const HELP: &str = r##\"first\n  second \"quoted\"\"##;",
        ),
        (
            Language::Rust,
            "pub fn borrow<'a>(\n value: &'a str,\n) -> &'a str",
            "pub fn borrow<'a>(value: &'a str,) -> &'a str",
        ),
        (
            Language::Python,
            "def f(\n text=\"a  \\\"quote\\\" b\",\n):",
            "def f(text=\"a  \\\"quote\\\" b\",):",
        ),
        (Language::Go, "const Space = ' '\n", "const Space = ' '"),
    ] {
        assert_eq!(
            projected(source, language, DeclarationKind::Function).as_deref(),
            Some(expected),
            "{source}"
        );
    }
    // Conservative whole-header preservation for syntax whose interpolation
    // owns another language grammar, comments, and incomplete captured text.
    for (language, source) in [
        (
            Language::TypeScript,
            "function f(\n value = `first\n  ${`nested ${name}`} second`,\n)",
        ),
        (
            Language::Python,
            "def f(\n value=f\"{x!r:>{width}}  words\",\n):",
        ),
        (Language::Python, "def f(text='''unterminated\n  "),
        (Language::Rust, "fn f( // parameter follows\n x: u8,\n)"),
        (Language::Python, "def f( # parameter follows\n x: int,\n):"),
        (Language::TypeScript, "function f(\n pattern = /a  b/,\n)"),
        (
            Language::CSharp,
            "void F(string x = \"\"\"A \"  B\"\"\", string y = \"x\")",
        ),
        (Language::Java, "@A(\"\"\"first\n  second\"\"\") void f()"),
        (
            Language::Unknown,
            "header(raw=@\"\"\"first\n  second\"\"\")",
        ),
    ] {
        assert_eq!(
            projected(source, language, DeclarationKind::Function).as_deref(),
            Some(source)
        );
    }
}

#[test]
fn actual_import_fragment_contracts_keep_local_aliases_and_written_default_imports() {
    // Encodings match compile::syntax::{value_import,qualifier_import}; local
    // names are separate row facts, not a fourth line invented by a renderer.
    for (language, encoded, local, written, expected) in [
        (
            Language::TypeScript,
            "value\n@/lib/fetchApi\nfetchApi",
            "read",
            None,
            "import { fetchApi as read } from \"@/lib/fetchApi\"",
        ),
        (
            Language::TypeScript,
            "qualifier\n@/lib/api",
            "api",
            None,
            "import * as api from \"@/lib/api\"",
        ),
        (
            Language::TypeScript,
            "value\nreact\nReact",
            "React",
            Some("import React from \"react\";\nfunction unrelated_body() { return 4; }"),
            "import React from \"react\";",
        ),
        (
            Language::TypeScript,
            "value\nreact\nReact",
            "React",
            Some("import React from \"react\"\nfunction unrelated_body() { return 4; }"),
            "import React from \"react\"",
        ),
        (
            Language::TypeScript,
            "value\nmodule\nname",
            "name",
            Some("import {\n name,\n} from \"module\";"),
            "import { name, } from \"module\";",
        ),
        (
            Language::TypeScript,
            "value\n./x.json\ncfg",
            "cfg",
            Some("import cfg from './x.json'\nwith{ type: 'json' };\nfunction unrelated_body() {}"),
            "import cfg from './x.json' with{ type: 'json' };",
        ),
        (
            Language::Python,
            "value\n//pkg/mod\nExported",
            "local",
            None,
            "from ..pkg.mod import Exported as local",
        ),
        (
            Language::Python,
            "qualifier\nos.path",
            "path",
            None,
            "import os.path as path",
        ),
        (
            Language::Rust,
            "value\ncrate::module\nThing",
            "Local",
            None,
            "use crate::module::Thing as Local;",
        ),
        (
            Language::Rust,
            "value\ncrate::module\nThing",
            "Local",
            Some("pub(crate) use crate::module::Thing as Local;\nfn unrelated_body() {}"),
            "pub(crate) use crate::module::Thing as Local;",
        ),
        (
            Language::Rust,
            "value\ncrate::module\nThing",
            "Local",
            Some("pub(in crate::api)\nuse crate::module::Thing as Local;\nfn unrelated_body() {}"),
            "pub(in crate::api) use crate::module::Thing as Local;",
        ),
        (
            Language::Rust,
            "value\ncrate::module\nThing",
            "Local",
            Some("pub(crate)use\ncrate::module::Thing as Local;\nfn unrelated_body() {}"),
            "pub(crate)use crate::module::Thing as Local;",
        ),
        (
            Language::Go,
            "qualifier\nnet/http",
            "http",
            None,
            "import http \"net/http\"",
        ),
        (
            Language::Go,
            "qualifier\nmath",
            ".",
            None,
            "import . \"math\"",
        ),
        (
            Language::TypeScript,
            "type\n./a\"b\\c\nConfig",
            "Config",
            None,
            "import type { Config } from \"./a\\\"b\\\\c\"",
        ),
    ] {
        let excerpt = written.map_or(SourceExcerpt::NotCaptured, |text| SourceExcerpt::Captured {
            text: text.into(),
            extent: SourceExcerptExtent::Complete,
        });
        let shown = Signature::from_declaration(
            encoded,
            language,
            Some(DeclarationKind::Import),
            local,
            &excerpt,
        )
        .map(|signature| signature.text());
        assert_eq!(shown.as_deref(), Some(expected), "{encoded} as {local}");
    }
    // Refusing a captured written import must not reconstruct a private or
    // partial declaration from its lossy row fragment, nor borrow its body.
    for (extent, written) in [
        (
            SourceExcerptExtent::Complete,
            "pub(crate) use crate::module::{Thing, /* note */ Other};\nfn unrelated_body() {}",
        ),
        (
            SourceExcerptExtent::Complete,
            "pub(in crate::api) use\n// note\ncrate::module::Thing;\nfn unrelated_body() {}",
        ),
        (
            SourceExcerptExtent::Complete,
            "pub(crate) use crate::module::Thing",
        ),
        (
            SourceExcerptExtent::Truncated,
            "pub(crate) use crate::module::{Thing,",
        ),
    ] {
        assert!(
            Signature::from_declaration(
                "value\ncrate::module\nThing",
                Language::Rust,
                Some(DeclarationKind::Import),
                "Thing",
                &SourceExcerpt::Captured {
                    text: written.into(),
                    extent,
                }
            )
            .is_none(),
            "captured source stays authoritative: {written}"
        );
    }
    for (encoded, local) in [
        ("value\nreact\nReact", "React"), // named versus default was not captured
        ("qualifier\nos.path", ""),       // missing local binding
        ("value\nmath\n.", "."),          // a dot alias is qualifier-only
        ("value\nmodule\na; evil()", "a; evil()"),
        ("value\nmodule\nname\nextra", "name"),
    ] {
        assert!(
            Signature::from_declaration(
                encoded,
                Language::TypeScript,
                Some(DeclarationKind::Import),
                local,
                &SourceExcerpt::NotCaptured
            )
            .is_none()
        );
    }
    assert_eq!(
        projected(
            "future\nmodule\nname",
            Language::TypeScript,
            DeclarationKind::Import
        )
        .as_deref(),
        Some("future module name")
    );
}

#[test]
fn actual_typed_renderers_keep_source_headers_and_omit_legacy_machine_encodings() {
    use backend_semantic::ir::{
        ArrayShape, BuiltinType, CanonicalTypeRenderLimits, ConcreteType, IrBuilder, TupleElement,
        TupleElementKind, UnknownReason, UnknownType, VariadicForm, prepare_canonical_type,
        render_source_callable, render_source_type,
    };
    use backend_semantic::vocabulary::{
        GoVersion, LanguageProfile, PythonVersion, RustEdition, TypeScriptSource,
    };
    let mut builder = IrBuilder::new();
    let byte = builder
        .intern_concrete(ConcreteType::Builtin(BuiltinType::U8))
        .expect("byte")
        .erase();
    let label = builder.intern_atom(b"value").expect("parameter name");
    let parameters = builder
        .intern_tuple_elements(&[TupleElement {
            label: Some(label),
            ty: byte,
            kind: TupleElementKind::Required,
        }])
        .expect("parameters");
    let results = builder
        .intern_tuple_elements(&[TupleElement {
            label: None,
            ty: byte,
            kind: TupleElementKind::Required,
        }])
        .expect("results");
    let callable = builder
        .intern_concrete(ConcreteType::Function {
            parameters,
            results,
            abi: None,
            variadic: VariadicForm::None,
            unsafe_: false,
        })
        .expect("callable")
        .erase();
    let unknown = builder
        .intern_unknown(UnknownType::new(UnknownReason::OracleGap))
        .expect("unknown")
        .erase();
    let name = builder
        .intern_atom(b"tuple")
        .expect("actual source type name");
    let named = builder
        .intern_unknown(UnknownType::new(UnknownReason::NoIrRepresentation).with_spelling(name))
        .expect("named type")
        .erase();
    let array = builder
        .intern_concrete(ConcreteType::Array {
            element: named,
            shape: ArrayShape::Sequence,
        })
        .expect("source array")
        .erase();
    let empty_elements = builder
        .intern_tuple_elements(&[])
        .expect("empty tuple elements");
    let empty_tuple = builder
        .intern_concrete(ConcreteType::Tuple(empty_elements))
        .expect("empty canonical tuple")
        .erase();
    let ir = builder.finish().expect("admitted IR");
    let limits = CanonicalTypeRenderLimits::new(std::num::NonZeroUsize::new(32).expect("depth"));
    for ty in [callable] {
        let mut canonical = String::new();
        prepare_canonical_type(&ir, ty, limits)
            .expect("typed canonical render")
            .write_to(&mut canonical)
            .expect("render bytes");
        assert!(
            projected(&canonical, Language::Rust, DeclarationKind::Function).is_none(),
            "{canonical}"
        );
    }
    for (language, profile, expected) in [
        (
            Language::Rust,
            LanguageProfile::Rust(RustEdition::Rust2024),
            "fn run(value: u8) -> u8",
        ),
        (
            Language::Go,
            LanguageProfile::Go(GoVersion::Go125),
            "func run(value uint8) uint8",
        ),
        (
            Language::Python,
            LanguageProfile::Python(PythonVersion::Python314),
            "def run(value: int) -> int",
        ),
        (
            Language::TypeScript,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            "function run(value: number): number",
        ),
    ] {
        let source = render_source_callable(&ir, profile, "run", callable, limits)
            .expect("truthful source header");
        assert_eq!(
            projected(&source, language, DeclarationKind::Function).as_deref(),
            Some(expected)
        );
        assert!(
            render_source_type(&ir, profile, unknown, limits).is_none(),
            "the typed producer owns truthful absence"
        );
    }
    let source = render_source_type(
        &ir,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        array,
        limits,
    )
    .expect("actual source tuple array");
    let mut machine = String::new();
    prepare_canonical_type(&ir, empty_tuple, limits)
        .expect("canonical tuple")
        .write_to(&mut machine)
        .expect("canonical bytes");
    assert_eq!(source, "tuple[]");
    assert_eq!(
        machine, source,
        "plain legacy strings cannot establish origin"
    );
    assert_eq!(
        projected(&source, Language::TypeScript, DeclarationKind::Variable).as_deref(),
        Some("tuple[]")
    );
    for kept in [
        "unknown(value)",
        "unknown(error)",
        "unknown(unannotated)",
        "unknown(oracle-gap)",
        "tuple[]",
        "object[]",
        "builtin(string)",
        "wildcard",
        "function(parameters=[])",
        "function(parameters=[],results=[],abi=none,variadic=none,unsafe=false)",
        "function(parameters=[element(label=x\"7\")],results=[],abi=none,variadic=none,unsafe=false)",
        "fn literal() -> &'static str = \"unknown(unannotated)\"",
    ] {
        assert_eq!(
            projected(kept, Language::TypeScript, DeclarationKind::Method).as_deref(),
            Some(kept),
            "written/malformed lookalike stays intact"
        );
    }
}

#[test]
fn page_member_record_and_wire_link_targets_survive_the_display_projection() {
    const MACHINE: &str = "function(parameters=[element(kind=required,label=x\"76616C7565\",type=builtin(u8))],results=[],abi=none,variadic=none,unsafe=false)";
    let own = declaration_row().with_signature(MACHINE);
    let centre = symbol_key(DECLARATION);
    let target = symbol_key("actual-link-target");
    let fragments = vec![
        Fragment::Text("A [source] ".to_owned()),
        Fragment::Link {
            label: "quoted `label`".to_owned(),
            target,
        },
        Fragment::Text("; next paragraph follows.".to_owned()),
        Fragment::Break,
        Fragment::Code("let x = `first\n  second`;".to_owned()),
    ];
    let mut document = Document::new(centre, basis().root, fragments.clone()).with_location(
        SourceAvailability::Captured(SourceLocation::new("src/lib.rs", 2).expect("source site")),
    );
    document.excerpt = SourceExcerpt::Captured {
        text: "pub fn ferris() {\n    beacon();\n}".into(),
        extent: SourceExcerptExtent::Complete,
    };
    document.signature = Some(MACHINE.to_owned());
    let member = backend_library::Row::new(
        RowId::Symbol(symbol_key("member")),
        basis(),
        "/abs/polyglot::src/lib.rs:3::member",
    )
    .with_parent(centre)
    .with_kind(DeclarationKind::Method)
    .with_signature("fn member(\n value: u8,\n)");
    let page = page_from_document(
        DECLARATION,
        &document,
        &[own.clone(), member],
        &[],
        Vec::new(),
    );
    assert!(page.signature().is_none());
    assert!(Record::from_row(&own).signature().is_none());
    assert_eq!(
        page.members()[0].members()[0]
            .signature()
            .map(Signature::text)
            .as_deref(),
        Some("fn member(value: u8,)")
    );
    assert!(page.prose().iter().any(|block| matches!(block, Prose::Link { label, target: link } if label == "quoted `label`" && *link == target)));
    assert!(
        page.prose().iter().any(
            |block| matches!(block, Prose::Code(text) if text == "let x = `first\n  second`;")
        )
    );
    assert_eq!(document.fragments.as_ref(), fragments);
    assert!(
        matches!(page.source(), Source::Captured { site, lines, .. } if site.path().as_str() == "src/lib.rs" && site.line().get() == 2 && lines.len() == 3)
    );
    let reply =
        backend_library::ReplyDto::new(7, backend_library::CommandReply::Document(document));
    let encoded = serde_json::to_vec(&reply).expect("wire retains link variant and exact target");
    let decoded = backend_library::ReplyDto::decode_against(&encoded, &reply)
        .expect("actual reply DTO round trip");
    assert_eq!(decoded, reply);
    let wire: serde_json::Value = serde_json::from_slice(&encoded).expect("wire JSON");
    let link = &wire["reply"]["data"]["fragments"][1];
    assert_eq!(link["kind"], "link");
    assert_eq!(link["data"]["label"], "quoted `label`");
    let encoded_target = link["data"]["target"]
        .as_str()
        .expect("link target retained");
    assert_eq!(encoded_target.len(), 64);
    let decoded_target = (0..64)
        .step_by(2)
        .map(|at| u8::from_str_radix(&encoded_target[at..at + 2], 16).expect("target hex"))
        .collect::<Vec<_>>();
    assert_eq!(decoded_target.as_slice(), target.as_bytes());
    let mut forged = wire.clone();
    forged["reply"]["data"]["fragments"][1]["data"]["target"] = serde_json::json!("00".repeat(32));
    assert!(
        backend_library::ReplyDto::decode_against(
            &serde_json::to_vec(&forged).expect("changed target"),
            &reply
        )
        .is_err()
    );
    for shown in [
        text::page(&page, Theme::plain()),
        markdown::page(&page),
        serde_json::to_string(&PageDto::new(&page)).expect("page DTO"),
    ] {
        assert!(!shown.contains("function(parameters="));
        assert!(shown.contains("quoted"));
    }
}
