//! Native Vue source-region and custody tests.
use backend_frontend_vue::{BlockKind, Gap, RawAttribute, parse, parse_admitted};
fn text<'a>(source: &'a str, range: &std::ops::Range<usize>) -> &'a str {
    &source[range.clone()]
}
#[test]
fn unicode_bom_crlf_regions_are_original_host_bytes() {
    let source = include_str!("fixtures/UnicodeCard.vue");
    let parsed = parse(source.as_bytes()).unwrap();
    assert!(
        parsed.diagnostics().is_empty(),
        "{:?}",
        parsed.diagnostics()
    );
    assert_eq!(
        parsed
            .blocks()
            .iter()
            .map(|b| text(source, &b.name))
            .collect::<Vec<_>>(),
        ["template", "script", "style", "style"]
    );
    assert_eq!(parsed.kind(1), Some(BlockKind::Script { setup: true }));
    assert_eq!(
        parsed.kind(2),
        Some(BlockKind::Style {
            scoped: false,
            module: true
        })
    );
    assert_eq!(
        parsed.kind(3),
        Some(BlockKind::Style {
            scoped: true,
            module: false
        })
    );
    assert!(matches!(
        parsed.raw_attribute(1, "setup"),
        Some(RawAttribute::Boolean)
    ));
    assert!(
        matches!(parsed.raw_attribute(1, "lang"), Some(RawAttribute::Value(ref value)) if value.text() == "ts")
    );
    let script = &parsed.blocks()[1];
    let body = text(source, script.content.as_ref().unwrap());
    let local = body.find("π").unwrap();
    let content = parsed.content(1).unwrap();
    let host = content
        .subregion(local..local + "π".len())
        .unwrap()
        .byte_range();
    assert_eq!(text(source, &host), "π");
    assert!(content.subregion(local..local + 1).is_err());
    assert!(content.subregion(0..body.len() + 1).is_err());
    assert_eq!(text(source, &script.attributes[1].name), "lang");
    assert_eq!(
        text(source, script.attributes[1].value.as_ref().unwrap()),
        "ts"
    );
    for block in parsed.blocks() {
        assert!(text(source, &block.open).starts_with('<'));
        assert!(text(source, block.close.as_ref().unwrap()).starts_with("</"));
    }
}
#[test]
fn duplicate_script_and_external_sources_remain_visible() {
    let source = "<script src='./a.js'></script>\n<script setup lang='ts'>const π = 1</script>\n<script src=\"./b.js\"></script>\n<style src='./a.css'></style><style lang='scss'>.x {}</style>";
    let parsed = parse(source.as_bytes()).unwrap();
    assert_eq!(parsed.blocks().len(), 5, "{parsed:?}");
    let source_values: Vec<_> = parsed
        .blocks()
        .iter()
        .flat_map(|b| &b.attributes)
        .filter(|a| text(source, &a.name) == "src")
        .map(|a| text(source, a.value.as_ref().unwrap()))
        .collect();
    assert_eq!(source_values, ["./a.js", "./b.js", "./a.css"]);
    assert!(parsed.inline_content(0).is_none());
    assert!(parsed.inline_content(3).is_none());
    assert!(parsed.inline_content(1).is_some());
    assert_eq!(
        parsed
            .gaps()
            .iter()
            .filter(|g| matches!(g, Gap::ExternalSource { .. }))
            .count(),
        3
    );
    assert!(
        parsed
            .diagnostics()
            .iter()
            .any(|d| format!("{:?}", d.code) == "DuplicateScript"),
        "{parsed:?}"
    );
}
#[test]
fn duplicate_templates_and_custom_blocks_are_observable() {
    let source = "<template><div><br></div></template><docs lang='md'>hello</docs><template><p>other</p></template>";
    let parsed = parse(source.as_bytes()).unwrap();
    assert_eq!(
        parsed
            .blocks()
            .iter()
            .map(|b| text(source, &b.name))
            .collect::<Vec<_>>(),
        ["template", "docs", "template"],
        "{parsed:?}"
    );
    assert!(parsed.gaps().contains(&Gap::DuplicateTemplate { block: 2 }));
}
#[test]
fn malformed_source_is_not_silently_complete() {
    let parsed = parse(b"<template><div></template>").unwrap();
    assert!(!parsed.diagnostics().is_empty(), "{parsed:?}");
}

#[test]
fn source_limits_and_identity_fence() {
    use backend_frontend_vue::VueSourceError;
    assert!(matches!(parse(&[0xff]), Err(VueSourceError::NonUtf8)));
    assert!(matches!(
        parse(&vec![b' '; 1024 * 1024 + 1]),
        Err(VueSourceError::SourceLimit)
    ));
    let source = "<script>const π = 1</script>";
    let host = parse(source.as_bytes()).unwrap();
    let content = host.content(0).unwrap();
    let edited = source.replace('1', "2");
    let other = parse(edited.as_bytes()).unwrap();
    assert_ne!(content.source(), other.content(0).unwrap().source());
    assert!(
        parse_admitted(
            edited.as_bytes(),
            host.host().content(),
            host.host().source()
        )
        .is_err()
    );
    assert!(
        parse_admitted(
            source.as_bytes(),
            host.host().content(),
            host.host().source()
        )
        .is_ok()
    );
}

#[test]
fn many_blocks_refuse_instead_of_publishing_partial_inventory() {
    use backend_frontend_vue::VueSourceError;
    let source = "<style></style>".repeat(4097);
    assert!(matches!(
        parse(source.as_bytes()),
        Err(VueSourceError::WorkLimit)
    ));
}

#[test]
fn attributes_remain_raw_and_duplicates_are_not_selected() {
    let source = br#"<script lang='t&#115;' lang="ts">a</script>"#;
    let parsed = parse(source).unwrap();
    assert!(matches!(
        parsed.raw_attribute(0, "lang"),
        Some(RawAttribute::Duplicate)
    ));
    assert!(!parsed.diagnostics().is_empty());
}

#[test]
fn native_script_declarations_map_to_verified_host() {
    use backend_frontend_vue::DeclarationKind;
    let source = "\u{feff}<!-- π 🚀 -->\r\n<script setup lang='ts'>\r\n// emoji 🚀\r\nconst café = 1;\r\nfunction привет() {}\r\nclass 雪 {}\r\n</script>";
    let parsed = parse(source.as_bytes()).unwrap();
    let script = parsed.parse_script(0).unwrap();
    assert!(script.diagnostics.is_empty(), "{:?}", script.diagnostics);
    let rows: Vec<_> = script
        .declarations
        .iter()
        .map(|d| (d.kind, d.name.text()))
        .collect();
    assert_eq!(
        rows,
        [
            (DeclarationKind::Variable, "café"),
            (DeclarationKind::Function, "привет"),
            (DeclarationKind::Class, "雪")
        ]
    );
    for declaration in script.declarations {
        assert_eq!(
            &source[declaration.name.byte_range()],
            declaration.name.text()
        );
        assert_eq!(declaration.name.source(), parsed.host().source());
        let owner = declaration.owner.byte_range();
        let name = declaration.name.byte_range();
        assert!(owner.start <= name.start && name.end <= owner.end);
    }
}

#[test]
fn script_refuses_external_and_unknown_language() {
    use backend_frontend_vue::ScriptParseError;
    let parsed =
        parse(b"<script src='./x.js'></script><script setup lang='coffee'>x</script>").unwrap();
    assert!(matches!(
        parsed.parse_script(0),
        Err(ScriptParseError::NoInlineContent)
    ));
    assert!(matches!(
        parsed.parse_script(1),
        Err(ScriptParseError::UnsupportedLanguage)
    ));
}

#[test]
fn empty_host_never_implies_complete_vue_semantics() {
    let parsed = parse(b"<!-- only a comment -->").unwrap();
    assert!(parsed.gaps().contains(&Gap::HostSemanticsUnanalyzed));
}

#[test]
fn native_named_import_is_host_spelled_and_not_a_resolved_edge() {
    use backend_frontend_vue::DeclarationKind;
    let source =
        "<script setup>import { onMounted as mounted } from 'vue'; mounted(() => {});</script>";
    let parsed = parse(source.as_bytes()).unwrap();
    let script = parsed.parse_script(0).unwrap();
    let row = script
        .declarations
        .iter()
        .find(|d| d.kind == DeclarationKind::Import)
        .unwrap();
    assert_eq!(row.name.text(), "mounted");
    assert_eq!(&source[row.name.byte_range()], "mounted");
}

#[test]
fn only_inline_plain_css_is_admitted_to_stylo() {
    use backend_frontend_vue::StyleRegionError;
    let source = br#"<style>.a{}</style><style lang="css">.b{}</style><style lang="scss">.c{}</style><style src="./x.css"></style>"#;
    let parsed = parse(source).unwrap();
    assert!(parsed.inline_css(0).is_ok());
    assert!(parsed.inline_css(1).is_ok());
    assert!(matches!(
        parsed.inline_css(2),
        Err(StyleRegionError::UnsupportedLanguage)
    ));
    assert!(matches!(
        parsed.inline_css(3),
        Err(StyleRegionError::NoInlineContent)
    ));
}

#[test]
fn native_binder_resolves_direct_call_with_shadowing() {
    let source = "<script setup>import { onMounted } from 'vue'; onMounted(() => {}); function outer(onMounted) { onMounted(); }</script>";
    let parsed = parse(source.as_bytes()).unwrap();
    let script = parsed.parse_script(0).unwrap();
    let import = script
        .declarations
        .iter()
        .find(|d| d.name.text() == "onMounted")
        .unwrap();
    let calls: Vec<_> = script
        .calls
        .iter()
        .filter(|c| c.target.byte_range() == import.name.byte_range())
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].site.text(), "onMounted");
    assert_ne!(calls[0].site.byte_range(), import.name.byte_range());
}

#[test]
fn cancelled_native_script_capture_is_not_published() {
    use std::sync::atomic::AtomicBool;
    let parsed = parse(b"<script>const x = 1;</script>").unwrap();
    let cancelled = AtomicBool::new(true);
    assert!(
        backend_frontend_vue::VueHostInventory::capture_cancellable(
            [1; 32],
            "src/App.vue",
            [2; 32],
            &parsed,
            &cancelled
        )
        .is_err()
    );
}
