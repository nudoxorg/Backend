use backend_library::{SourceRangePage, SourceRangeRequest, SourceRangeScope};

use crate::{
    Answer, DEFAULT_RESPONSE_BUDGET_BYTES, DeclarationSourceRange, Detail, ProductView, Theme,
    encode_answer, markdown, text,
};

fn answer(page: SourceRangePage) -> Answer {
    Answer::Product(Box::new(
        ProductView::assembled("source-range", Vec::new()).with_declaration_source_range(
            DeclarationSourceRange {
                revision: "ab".repeat(32),
                selected_symbol: "cd".repeat(32),
                coordinate: None,
                verified_at_read: true,
                source: page,
                next_cursor: None,
            },
        ),
    ))
}

fn page(body: &str, scope: SourceRangeScope) -> SourceRangePage {
    let len = u32::try_from(body.len()).expect("bounded fixture");
    SourceRangePage {
        source_policy: backend_library::SourcePolicyQualification::Unknown,
        text: body.to_owned(),
        start: 0,
        end: len,
        total: len,
        scope,
        source_identity: [7; 32],
        owner_start: 17,
        owner_end: 17 + len,
    }
}

#[test]
fn source_scope_json_text_and_markdown_preserve_bytes_and_evidence() {
    let raw = "first é漢🦀\r\nlast\r\n";
    let native = page(raw, SourceRangeScope::NativePrimaryDeclaration);
    let mut legacy = serde_json::to_value(&native).expect("serialize page");
    legacy.as_object_mut().expect("object").remove("scope");
    let legacy: SourceRangePage = serde_json::from_value(legacy).expect("legacy page");
    assert_eq!(legacy.scope, SourceRangeScope::Unknown);
    let mut invalid = serde_json::to_value(&native).expect("serialize page");
    invalid["scope"] = serde_json::json!("complete_declaration");
    assert!(serde_json::from_value::<SourceRangePage>(invalid).is_err());

    for (page, expected) in [
        (legacy, "unknown"),
        (native, "native_primary_declaration"),
        (page(raw, SourceRangeScope::StructuralDeclaration), "structural_declaration"),
    ] {
        let scope = page.scope;
        let answer = answer(page);
        let encoded = encode_answer(
            &answer,
            Detail::Standard,
            None,
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("bounded page");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(value["declaration_source_range"]["source"]["text"], raw);
        assert_eq!(
            value["declaration_source_range"]["source"]["scope"],
            expected
        );
        let terminal = text::answer(&answer, Theme::plain());
        let markdown = markdown::answer(&answer);
        assert!(terminal.contains(raw));
        assert!(markdown.contains(raw));
        assert!(terminal.contains(scope.description()));
        assert!(markdown.contains(scope.description()));
        assert!(terminal.contains("Source bytes match the indexed capture"));
        assert!(terminal.contains("ignore-policy stability is not established"));
        assert!(markdown.contains("Source bytes match the indexed capture"));
        assert!(markdown.contains("ignore-policy stability is not established"));
    }

    let unverified = Answer::Product(Box::new(
        ProductView::assembled("source-range", Vec::new()).with_declaration_source_range(
            DeclarationSourceRange {
                revision: "ab".repeat(32),
                selected_symbol: "cd".repeat(32),
                coordinate: None,
                verified_at_read: false,
                source: page(raw, SourceRangeScope::Unknown),
                next_cursor: None,
            },
        ),
    ));
    for rendered in [
        text::answer(&unverified, Theme::plain()),
        markdown::answer(&unverified),
    ] {
        assert!(rendered.contains(raw));
        assert!(rendered.contains("ignore-policy stability is not established"));
        assert!(!rendered.contains("Source bytes match the indexed capture"));
    }
}

#[test]
fn source_scope_budget_and_owner_bounds_are_enforced() {
    let raw = "\0".repeat(2048);
    let page = page(&raw, SourceRangeScope::NativePrimaryDeclaration);
    let request = SourceRangeRequest {
        start: 0,
        limit: 2048,
        source_identity: None,
    };
    assert!(page.admits(request));
    let answer = answer(page.clone());
    let encoded = encode_answer(
        &answer,
        Detail::Standard,
        None,
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("escaped page fits default budget");
    let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
    assert_eq!(value["declaration_source_range"]["source"]["text"], raw);
    assert!(encoded.bytes.len() <= DEFAULT_RESPONSE_BUDGET_BYTES);
    assert!(encode_answer(&answer, Detail::Standard, None, encoded.bytes.len() - 1).is_err());

    let mut empty = self::page("", SourceRangeScope::NativePrimaryDeclaration);
    assert!(!empty.admits(request));
    empty.scope = SourceRangeScope::Unknown;
    assert!(empty.admits(request));
    let eof_request = SourceRangeRequest {
        start: page.total,
        limit: 4,
        source_identity: Some(page.source_identity),
    };
    let mut eof = page.clone();
    eof.start = eof.total;
    eof.end = eof.total;
    eof.text.clear();
    assert!(eof.admits(eof_request));
    eof.owner_end += 1;
    assert!(!eof.admits(eof_request));
}
