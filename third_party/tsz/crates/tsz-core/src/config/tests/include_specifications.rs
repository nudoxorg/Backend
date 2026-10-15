//! Recoverable include-selector diagnostics retain native JSONC source spans.

use super::super::*;

#[test]
fn terminal_recursive_includes_keep_valid_selectors_and_original_jsonc_spans() {
    let source = r#"{
        /* π: { "include": ["comment/**"] } */
        "metadata": {"include": ["nested/**"]},
        "include": ["overridden/**"],
        "incl\u0075de": ["src\u002f**", "windows\\**", "valid/**/*",],
        "exclude": ["dist/**"],
        "files": ["explicit.ts"]
    }"#;
    let parsed = parse_tsconfig_with_diagnostics(source, "project/tsconfig.json").unwrap();
    assert_eq!(parsed.config.include, Some(vec!["valid/**/*".into()]));
    assert_eq!(parsed.config.exclude, Some(vec!["dist/**".into()]));
    assert_eq!(parsed.config.files, Some(vec!["explicit.ts".into()]));
    assert_eq!(parsed.diagnostics.len(), 2);
    for (diagnostic, token, specification) in [
        (&parsed.diagnostics[0], r#""src\u002f**""#, "src/**"),
        (&parsed.diagnostics[1], r#""windows\\**""#, "windows\\**"),
    ] {
        assert_eq!(diagnostic.code, 5010);
        assert_eq!(diagnostic.file, "project/tsconfig.json");
        assert_eq!(diagnostic.start as usize, source.find(token).unwrap());
        assert_eq!(diagnostic.length as usize, token.len());
        assert_eq!(
            &source[diagnostic.start as usize..(diagnostic.start + diagnostic.length) as usize],
            token
        );
        assert!(diagnostic.message_text.contains(specification));
    }
}

#[test]
fn invalid_include_recovery_preserves_an_explicit_empty_selection() {
    let parsed = parse_tsconfig_with_diagnostics(
        r#"{"include":["**","src/**/","src\\**\\."],"exclude":["excluded/**"]}"#,
        "tsconfig.json",
    )
    .unwrap();
    assert_eq!(parsed.config.include, Some(Vec::new()));
    assert_eq!(parsed.config.exclude, Some(vec!["excluded/**".into()]));
    assert_eq!(parsed.diagnostics.len(), 3);
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == 5010)
    );
    for specification in ["**/*", "src/*", "src/**/main.ts", "src/**/..", ""] {
        assert!(!include_specification_has_trailing_recursion(specification));
    }
}
