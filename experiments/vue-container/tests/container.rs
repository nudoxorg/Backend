use nudox_vue_container_proof::{parse, Gap};
fn text<'a>(source: &'a str, range: &std::ops::Range<usize>) -> &'a str { &source[range.clone()] }
#[test]
fn unicode_bom_crlf_regions_are_original_host_bytes() {
    let source = include_str!("fixtures/UnicodeCard.vue");
    let parsed = parse(source).unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.blocks.iter().map(|b| text(source, &b.name)).collect::<Vec<_>>(), ["template", "script", "style", "style"]);
    let script = &parsed.blocks[1];
    let body = text(source, script.content.as_ref().unwrap());
    let local = body.find("π").unwrap();
    let host = script.host_range(source, local..local + "π".len()).unwrap();
    assert_eq!(text(source, &host), "π");
    assert!(script.host_range(source, local..local + 1).is_none());
    assert!(script.host_range(source, 0..body.len() + 1).is_none());
    assert_eq!(text(source, &script.attributes[1].name), "lang");
    assert_eq!(text(source, script.attributes[1].value.as_ref().unwrap()), "ts");
    for block in &parsed.blocks {
        assert!(text(source, &block.open).starts_with('<'));
        assert!(text(source, block.close.as_ref().unwrap()).starts_with("</"));
    }
}
#[test]
fn duplicate_script_and_external_sources_remain_visible() {
    let source = "<script src='./a.js'></script>\n<script setup lang='ts'>const π = 1</script>\n<script src=\"./b.js\"></script>\n<style src='./a.css'></style><style lang='scss'>.x {}</style>";
    let parsed = parse(source).unwrap();
    assert_eq!(parsed.blocks.len(), 5, "{parsed:?}");
    let source_values: Vec<_> = parsed.blocks.iter().flat_map(|b| &b.attributes)
        .filter(|a| text(source, &a.name) == "src").map(|a| text(source, a.value.as_ref().unwrap())).collect();
    assert_eq!(source_values, ["./a.js", "./b.js", "./a.css"]);
    assert_eq!(parsed.gaps.iter().filter(|g| matches!(g, Gap::ExternalSource { .. })).count(), 3);
    assert!(parsed.diagnostics.iter().any(|d| format!("{:?}", d.code) == "DuplicateScript"), "{parsed:?}");
}
#[test]
fn duplicate_templates_and_custom_blocks_are_observable() {
    let source = "<template><div><br></div></template><docs lang='md'>hello</docs><template><p>other</p></template>";
    let parsed = parse(source).unwrap();
    assert_eq!(parsed.blocks.iter().map(|b| text(source, &b.name)).collect::<Vec<_>>(), ["template", "docs", "template"], "{parsed:?}");
    assert!(parsed.gaps.contains(&Gap::DuplicateTemplate { block: 2 }));
}
#[test]
fn malformed_source_is_not_silently_complete() {
    let parsed = parse("<template><div></template>").unwrap();
    assert!(!parsed.diagnostics.is_empty(), "{parsed:?}");
}
