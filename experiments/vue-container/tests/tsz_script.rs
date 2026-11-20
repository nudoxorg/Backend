//! TSZ native parse and byte position probe on one raw SFC script region.
use nudox_vue_container_proof::parse;
use tsz_parser::parser::{syntax_kind_ext, ParserState};
#[test]
fn tsz_identifier_positions_map_from_unicode_script_to_original_host() {
    let source = "\u{feff}<!-- π 🚀 -->\r\n<script setup lang='ts'>\r\n// emoji 🚀\r\nconst café = 1;\r\nfunction привет() {}\r\nclass 雪 {}\r\n</script>";
    let container = parse(source).unwrap();
    let block = &container.blocks[0];
    let body = &source[block.content.clone().unwrap()];
    let mut parser = ParserState::new("script.ts".into(), body.into());
    parser.parse_source_file();
    assert!(parser.get_diagnostics().is_empty());
    let arena = parser.get_arena();
    let mut names = Vec::new();
    for node in &arena.nodes {
        let name = match node.kind {
            syntax_kind_ext::VARIABLE_DECLARATION => {
                arena.get_variable_declaration(node).map(|d| d.name)
            }
            syntax_kind_ext::FUNCTION_DECLARATION => arena.get_function(node).map(|d| d.name),
            syntax_kind_ext::CLASS_DECLARATION => arena.get_class(node).map(|d| d.name),
            _ => None,
        };
        if let Some(name) = name {
            let node = arena.get(name).unwrap();
            assert!(arena.get_identifier(node).is_some());
            let host = block
                .host_range(source, node.pos as usize..node.end as usize)
                .unwrap();
            names.push(&source[host]);
        }
    }
    assert_eq!(names, ["café", "привет", "雪"]);
}

#[test]
fn malformed_script_retains_tszevent_diagnostics_with_host_offsets() {
    let source = "<!-- 😀 -->\r\n<script>const x = ;</script>";
    let container = parse(source).unwrap();
    let block = &container.blocks[0];
    let body = &source[block.content.clone().unwrap()];
    let mut parser = ParserState::new("script.js".into(), body.into());
    parser.parse_source_file();
    assert!(!parser.get_diagnostics().is_empty());
    for diagnostic in parser.get_diagnostics() {
        let start = diagnostic.start as usize;
        let end = start + diagnostic.length as usize;
        assert!(block.host_range(source, start..end).is_some());
    }
}
