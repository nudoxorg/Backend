use super::ParserState;

#[test]
fn u32_from_usize_clamps_overflow_without_panicking() {
    let parser = ParserState::new("a.ts".to_string(), String::new());

    assert_eq!(parser.u32_from_usize(usize::MAX), u32::MAX);
    assert!(parser.reported_offset_overflow.get());
}

#[test]
fn u16_from_node_flags_truncates_overflow_without_panicking() {
    let parser = ParserState::new("a.ts".to_string(), String::new());

    assert_eq!(parser.u16_from_node_flags(0x1_0001), 1);
    assert!(parser.reported_node_flag_overflow.get());
}

#[test]
fn reset_clears_conversion_overflow_markers() {
    let mut parser = ParserState::new("a.ts".to_string(), String::new());
    let _ = parser.u32_from_usize(usize::MAX);
    let _ = parser.u16_from_node_flags(0x1_0001);

    assert!(parser.reported_offset_overflow.get());
    assert!(parser.reported_node_flag_overflow.get());

    parser.reset("b.ts".to_string(), String::new());

    assert!(!parser.reported_offset_overflow.get());
    assert!(!parser.reported_node_flag_overflow.get());
}

fn module_span_parser(source: &str) -> ParserState {
    let mut parser = ParserState::new("module-spans.d.ts".into(), source.into());
    parser.parse_source_file();
    parser
}

fn module_span_node(parser: &ParserState, name: &str) -> super::NodeIndex {
    parser
        .arena
        .nodes
        .iter()
        .enumerate()
        .find_map(|(index, node)| {
            let module = parser.arena.get_module(node)?;
            let name_node = parser.arena.get(module.name)?;
            let text = parser
                .arena
                .get_literal(name_node)
                .map(|value| value.text.as_str())
                .or_else(|| {
                    parser
                        .arena
                        .get_identifier(name_node)
                        .map(|value| value.escaped_text.as_str())
                })?;
            (text == name).then_some(super::NodeIndex(index as u32))
        })
        .expect("actual module declaration")
}

#[test]
fn module_span_empty_ambient_comment_ends_before_adjacent_declaration() {
    let source = "declare module 'server-only' { /** original comment */ }\ndeclare module 'client-only' { /** second comment */ }";
    let parser = module_span_parser(source);
    assert!(parser.parse_diagnostics.is_empty());
    let first = parser
        .arena
        .get(module_span_node(&parser, "server-only"))
        .unwrap();
    let body = parser.arena.get_module(first).unwrap().body;
    let end = (source.find('}').unwrap() + 1) as u32;
    assert_eq!((first.pos, first.end), (0, end));
    assert_eq!(parser.arena.get(body).unwrap().end, end);
    let name = parser
        .arena
        .get(parser.arena.get_module(first).unwrap().name)
        .unwrap();
    assert_eq!(
        &source[name.pos as usize..name.end as usize],
        "'server-only'"
    );
    let second = parser
        .arena
        .get(module_span_node(&parser, "client-only"))
        .unwrap();
    assert_eq!(
        second.pos as usize,
        source.find("declare module 'client-only'").unwrap()
    );
    assert_eq!(second.end as usize, source.len());
}

#[test]
fn module_span_ordinary_namespace_ends_at_its_body() {
    let source = "namespace First {} namespace Second {}";
    let parser = module_span_parser(source);
    assert!(parser.parse_diagnostics.is_empty());
    let first = parser
        .arena
        .get(module_span_node(&parser, "First"))
        .unwrap();
    assert_eq!(first.end as usize, source.find('}').unwrap() + 1);
    let second = parser
        .arena
        .get(module_span_node(&parser, "Second"))
        .unwrap();
    assert_eq!(second.end as usize, source.len());
}

#[test]
fn module_span_nested_dotted_namespaces_share_the_consumed_body_end() {
    for prefix in ["namespace", "declare namespace"] {
        let source = format!("{prefix} Outer.Middle.Inner {{}} namespace After {{}}");
        let parser = module_span_parser(&source);
        assert!(parser.parse_diagnostics.is_empty());
        let end = (source.find('}').unwrap() + 1) as u32;
        for name in ["Outer", "Middle", "Inner"] {
            let node = parser.arena.get(module_span_node(&parser, name)).unwrap();
            assert_eq!(node.end, end, "{prefix} {name}");
            let body = parser.arena.get_module(node).unwrap().body;
            assert_eq!(parser.arena.get(body).unwrap().end, end);
        }
        assert_eq!(
            parser
                .arena
                .get(module_span_node(&parser, "After"))
                .unwrap()
                .end as usize,
            source.len()
        );
    }
}

#[test]
fn module_span_bodyless_declaration_owns_its_explicit_semicolon() {
    let source = "declare module 'first'; declare module 'second';";
    let parser = module_span_parser(source);
    assert!(parser.parse_diagnostics.is_empty());
    let first = parser
        .arena
        .get(module_span_node(&parser, "first"))
        .unwrap();
    assert!(parser.arena.get_module(first).unwrap().body.is_none());
    assert_eq!(first.end as usize, source.find(';').unwrap() + 1);
    assert_eq!(
        parser
            .arena
            .get(module_span_node(&parser, "second"))
            .unwrap()
            .end as usize,
        source.len()
    );
}

#[test]
fn module_span_bodyless_automatic_semicolon_never_owns_the_next_token() {
    let source = "declare module 'first'\ndeclare module 'second'";
    let parser = module_span_parser(source);
    assert!(parser.parse_diagnostics.is_empty());
    let first = parser
        .arena
        .get(module_span_node(&parser, "first"))
        .unwrap();
    assert!(parser.arena.get_module(first).unwrap().body.is_none());
    assert_eq!(first.end as usize, source.find('\n').unwrap());
    assert_eq!(
        parser
            .arena
            .get(module_span_node(&parser, "second"))
            .unwrap()
            .end as usize,
        source.len()
    );
}

#[test]
fn module_span_deferred_close_recovery_retains_the_body_boundary() {
    let source = "namespace Outer { namespace Inner { const callback = () => }} namespace After {}";
    let parser = module_span_parser(source);
    assert!(
        !parser.parse_diagnostics.is_empty(),
        "the missing arrow expression still refuses"
    );
    let inner = parser
        .arena
        .get(module_span_node(&parser, "Inner"))
        .unwrap();
    let body = parser.arena.get_module(inner).unwrap().body;
    assert_eq!(inner.end, parser.arena.get(body).unwrap().end);
    assert_eq!(
        inner.end as usize,
        source.find("}}").unwrap(),
        "deferred recovery does not claim an unconsumed brace"
    );
    let after = parser
        .arena
        .get(module_span_node(&parser, "After"))
        .unwrap();
    assert_eq!(after.end as usize, source.len());
}
