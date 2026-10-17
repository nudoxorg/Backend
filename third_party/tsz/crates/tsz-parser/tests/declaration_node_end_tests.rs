//! Exact declaration-node end positions shared by diagnostics and tooling.

use crate::parser::syntax_kind_ext;
use crate::parser::test_fixture::{assert_span_on, count_nodes, parse_source};

#[test]
fn bodyless_declarations_with_semicolons_end_before_following_tokens() {
    let source = r#"function plain(): void; const afterPlain = 1;
export default function optional(): void; const afterOptional = 2;
class C {
    method(): void; next() {}
    get value(): string; other() {}
    set value(v: string); final() {}
}"#;
    let (parser, _) = parse_source(source);

    for (kind, expected) in [
        (
            syntax_kind_ext::FUNCTION_DECLARATION,
            "function plain(): void;",
        ),
        (
            syntax_kind_ext::FUNCTION_DECLARATION,
            "function optional(): void;",
        ),
        (syntax_kind_ext::METHOD_DECLARATION, "method(): void;"),
        (syntax_kind_ext::GET_ACCESSOR, "get value(): string;"),
        (syntax_kind_ext::SET_ACCESSOR, "set value(v: string);"),
    ] {
        assert_span_on(&parser, source, kind, expected);
    }
}

#[test]
fn bodyless_declarations_with_asi_end_before_following_trivia() {
    let source = r#"function plain(): void
const afterPlain = 1;
export default function optional(): void
const afterOptional = 2;
class C {
    method(): void
    next() {}
    get value(): string
    other() {}
    set value(v: string)
    final() {}
}"#;
    let (parser, _) = parse_source(source);

    for (kind, expected) in [
        (
            syntax_kind_ext::FUNCTION_DECLARATION,
            "function plain(): void",
        ),
        (
            syntax_kind_ext::FUNCTION_DECLARATION,
            "function optional(): void",
        ),
        (syntax_kind_ext::METHOD_DECLARATION, "method(): void"),
        (syntax_kind_ext::GET_ACCESSOR, "get value(): string"),
        (syntax_kind_ext::SET_ACCESSOR, "set value(v: string)"),
    ] {
        assert_span_on(&parser, source, kind, expected);
    }
}


#[test]
fn variable_declarators_end_at_the_last_owned_component_not_the_lookahead_token() {
    for (source, declarations) in [
        ("export const scheduleVersion = 1;", vec!["scheduleVersion = 1"]),
        ("const first = 1, second = 2;", vec!["first = 1", "second = 2"]),
        ("const value = 1\nexport const next = 2;", vec!["value = 1", "next = 2"]),
        ("const value = 1 /* trailing */ ;", vec!["value = 1"]),
        ("let typed: string;", vec!["typed: string"]),
        ("let definite!: number;", vec!["definite!: number"]),
        ("let bare!;", vec!["bare!"]),
        ("for (let entry = 0; entry < 2; entry++) {}", vec!["entry = 0"]),
        ("for (const entry of values) {}", vec!["entry"]),
        ("const value = { nested: [1, 2] };", vec!["value = { nested: [1, 2] }"]),
    ] {
        let (parser, _) = parse_source(source);
        for declaration in declarations {
            assert_span_on(&parser, source, syntax_kind_ext::VARIABLE_DECLARATION, declaration);
        }
    }
}

#[test]
fn for_header_declarators_share_owned_ends_with_standalone_declarations() {
    for (source, declarations) in [
        (
            "for (let entry = 0; entry < 2; entry++) {}",
            vec!["entry = 0"],
        ),
        (
            "for (let first = 0, second = first + 1; first < 2; first++) {}",
            vec!["first = 0", "second = first + 1"],
        ),
        (
            "for (let entry: number = 0 /* boundary */; entry < 2; entry++) {}",
            vec!["entry: number = 0"],
        ),
        ("for (let entry; ; ) {}", vec!["entry"]),
        (
            "for (const entry /* boundary */ of values) {}",
            vec!["entry"],
        ),
        (
            "for (const { value: entry = 1, ...rest } of values) {}",
            vec!["{ value: entry = 1, ...rest }"],
        ),
        (
            "for (let [entry, ...rest] of values) {}",
            vec!["[entry, ...rest]"],
        ),
        (
            "for (let entry /* boundary */ in values) {}",
            vec!["entry"],
        ),
        ("let entry = 0; after();", vec!["entry = 0"]),
        ("let entry: number = 0; after();", vec!["entry: number = 0"]),
        ("let entry; after();", vec!["entry"]),
        (
            "const { value: entry = 1, ...rest } = source; after();",
            vec!["{ value: entry = 1, ...rest } = source"],
        ),
        (
            "let entry\nfor (const item of values) {}",
            vec!["entry", "item"],
        ),
    ] {
        let (parser, _) = parse_source(source);
        assert!(
            parser.get_diagnostics().is_empty(),
            "valid declaration fixture must parse without recovery: {source:?}: {:?}",
            parser.get_diagnostics()
        );
        assert_eq!(
            count_nodes(&parser, syntax_kind_ext::VARIABLE_DECLARATION),
            declarations.len(),
            "all declarators must remain present in {source:?}"
        );
        for declaration in declarations {
            assert_span_on(
                &parser,
                source,
                syntax_kind_ext::VARIABLE_DECLARATION,
                declaration,
            );
        }
    }
}
