//! These are typed presentation fixtures, not compiler admission or live-index proof.
#![allow(clippy::expect_used, clippy::panic)]

use crate::*;
use backend_library::{DeclarationKind, SourceAvailability, symbol_key};
use serde_json::{Value, json};

const COORDINATE: &str = "/abs/app::src/entry.ts:42::entrySummary";
const SIGNATURE: &str = "function entrySummary(entry: Entry): string";
const SOURCE: &str = "SOURCE_BODY_SENTINEL_λ";
const PROSE: &str = "PROSE_SENTINEL";
const MEMBER: &str = "MEMBER_SENTINEL";
const RELATION: &str = "RELATION_SENTINEL";
const NOTE: &str = "SECTION_FAULT_SENTINEL";

fn page(source: Source) -> Page {
    Page::new(
        Identity::parse_with_key(
            COORDINATE,
            IdentityKey::Symbol(symbol_key("selected-native-entry")),
        ),
        Some(DeclarationKind::Function),
        source,
    )
    .with_signature(Signature::tokenize(SIGNATURE, Language::TypeScript))
    .with_prose(vec![Prose::Text(PROSE.to_owned())])
    .with_members(MemberGroup::group(vec![Member::new(
        Identity::parse(&format!("{COORDINATE}::{MEMBER}")),
        Some(DeclarationKind::Function),
        None,
    )]))
    .with_relations(vec![RelationGroup::new(
        RelationLabel::Related,
        vec![Relation::new(
            Identity::parse(&format!("{COORDINATE}::{RELATION}")),
            Some(DeclarationKind::Function),
        )],
    )])
    .with_notes(vec![Fault::usage("members", NOTE)])
}

fn captured() -> Page {
    let line = LineNumber::new(42).expect("one-based");
    let body = format!("{SOURCE}\n{}", "retained_source_only\n".repeat(240));
    assert!(body.len() > 4096);
    page(Source::Captured {
        site: SourceSite::new(PackagePath::new("src/entry.ts"), line),
        lines: Source::number_lines(&body, line),
        truncation: Truncation::Truncated,
    })
}

fn encoded(answer: &Answer, detail: Detail) -> Value {
    let payload = encode_answer(answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
        .expect("fixture projection fits");
    assert_eq!(payload.budget.bytes, payload.bytes.len());
    let value: Value = serde_json::from_slice(&payload.bytes).expect("typed JSON");
    assert_eq!(value["budget"]["bytes"], payload.bytes.len());
    value
}

#[test]
fn page_projection_summary_omits_evidence_without_replacing_or_mutating_it() {
    let page = captured();
    let original = page.clone();
    let summary = PageProjection::new(&page, Detail::Summary);
    assert!(matches!(summary.source(), PageSection::OmittedByDetail));
    assert!(matches!(summary.prose(), PageSection::OmittedByDetail));
    assert!(matches!(summary.members(), PageSection::OmittedByDetail));
    assert!(matches!(summary.relations(), PageSection::OmittedByDetail));
    assert!(matches!(
        summary.signature_tokens(),
        PageSection::OmittedByDetail
    ));
    assert!(std::ptr::eq(summary.identity(), page.identity()));
    assert!(std::ptr::eq(
        summary.signature().expect("signature"),
        page.signature().expect("signature")
    ));
    assert!(std::ptr::eq(summary.notes(), page.notes()));
    assert!(summary.source_fault().is_none());
    let source = PageProjection::new(&page, Detail::Full);
    assert!(std::ptr::eq(
        source.source().included().expect("source"),
        page.source()
    ));

    let answer = Answer::Page(Box::new(page));
    let value = encoded(&answer, Detail::Summary);
    let mut fields: Vec<_> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "answer",
            "budget",
            "detail",
            "identity",
            "kind",
            "language",
            "notes",
            "signature"
        ]
    );
    assert_eq!(value["identity"]["coordinate"], COORDINATE);
    assert_eq!(
        value["identity"]["semantic_data"]["kind"],
        "selected-symbol-id"
    );
    assert_eq!(
        value["identity"]["semantic_data"]["value"],
        json!(symbol_key("selected-native-entry").as_bytes())
    );
    assert_eq!(value["signature"], SIGNATURE);
    assert_eq!(value["notes"][0]["detail"], NOTE);

    for rendered in [
        markdown::answer_with_detail(
            &answer,
            Detail::Summary,
            None,
            ContinuationTarget::CliOption,
        ),
        text::answer_with_detail(
            &answer,
            Theme::plain(),
            Detail::Summary,
            None,
            ContinuationTarget::CliOption,
        ),
    ] {
        assert!(rendered.contains(COORDINATE));
        assert!(rendered.contains(SIGNATURE));
        assert!(rendered.contains(NOTE));
        assert!(rendered.contains("src/entry.ts:42"));
        for omitted in [
            SOURCE,
            PROSE,
            MEMBER,
            RELATION,
            "## source",
            "source-unavailable",
        ] {
            assert!(
                !rendered.contains(omitted),
                "summary leaked {omitted}: {rendered}"
            );
        }
    }
    for detail in [Detail::Standard, Detail::Full] {
        let value = encoded(&answer, detail);
        assert_eq!(value["signature"], SIGNATURE);
        assert_eq!(value["source"]["path"], "src/entry.ts");
        assert_eq!(value["source"]["line"], 42);
        assert_eq!(value["source"]["extent"], "truncated");
        assert_eq!(value["source"]["lines"][0], SOURCE);
        assert_eq!(
            value["source"]["lines"].as_array().expect("source").len(),
            241
        );
        assert_eq!(value["prose"][0], PROSE);
        assert_eq!(
            value["members"][0]["members"][0]["identity"]["name"],
            MEMBER
        );
        assert_eq!(value["relations"][0]["relations"][0]["name"], RELATION);
        assert_eq!(value["notes"][0]["detail"], NOTE);
        assert!(!value["tokens"].as_array().expect("tokens").is_empty());
        assert!(value.get("source_fault").is_none());
        for rendered in [
            markdown::answer_with_detail(&answer, detail, None, ContinuationTarget::CliOption),
            text::answer_with_detail(
                &answer,
                Theme::plain(),
                detail,
                None,
                ContinuationTarget::CliOption,
            ),
        ] {
            for included in [
                SOURCE,
                PROSE,
                MEMBER,
                RELATION,
                NOTE,
                SIGNATURE,
                "source continues past the retained bound",
            ] {
                assert!(rendered.contains(included), "lost {included}: {rendered}");
            }
        }
    }
    let Answer::Page(page) = &answer else {
        panic!("page")
    };
    assert_eq!(**page, original);
    assert_eq!(
        markdown::page(page),
        markdown::answer_with_detail(
            &answer,
            Detail::Standard,
            None,
            ContinuationTarget::CliOption
        )
    );
    assert_eq!(
        text::page(page, Theme::plain()),
        text::answer_with_detail(
            &answer,
            Theme::plain(),
            Detail::Standard,
            None,
            ContinuationTarget::CliOption
        )
    );
    assert!(markdown::answer(&answer).contains(SOURCE));
    assert!(
        markdown::answer_with_cursor(&answer, None, ContinuationTarget::CliOption).contains(SOURCE)
    );
    assert!(text::answer(&answer, Theme::plain()).contains(SOURCE));
    assert!(
        text::answer_with_cursor(&answer, Theme::plain(), None, ContinuationTarget::CliOption)
            .contains(SOURCE)
    );
}

#[test]
fn page_projection_summary_preserves_actual_source_and_section_faults() {
    for (availability, cause) in [
        (SourceAvailability::NotCaptured, "not-captured"),
        (SourceAvailability::NotHydrated, "not-resident"),
        (SourceAvailability::Unconfigured, "unconfigured"),
    ] {
        let fault = Fault::source(
            &availability,
            Operand::Coordinate(Identity::parse(COORDINATE).coordinate().clone()),
        )
        .expect("actual unavailable source fault");
        let source = if availability == SourceAvailability::NotHydrated {
            Source::Sited {
                site: SourceSite::new(
                    PackagePath::new("src/entry.ts"),
                    LineNumber::new(42).expect("line"),
                ),
                fault,
            }
        } else {
            Source::Absent { fault }
        };
        let answer = Answer::Page(Box::new(page(source)));
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let value = encoded(&answer, detail);
            assert!(value.get("source").is_none());
            assert_eq!(value["source_fault"]["slug"], "source-unavailable");
            assert_eq!(value["source_fault"]["cause"], cause);
            assert_eq!(value["source_fault"]["operand"], COORDINATE);
            assert_eq!(value["notes"][0]["detail"], NOTE);
            for rendered in [
                markdown::answer_with_detail(&answer, detail, None, ContinuationTarget::CliOption),
                text::answer_with_detail(
                    &answer,
                    Theme::plain(),
                    detail,
                    None,
                    ContinuationTarget::CliOption,
                ),
            ] {
                assert!(rendered.contains("source-unavailable"));
                assert!(rendered.contains(NOTE));
                assert!(rendered.contains(COORDINATE));
                assert!(!rendered.contains(SOURCE));
            }
        }
    }
}

#[test]
fn page_projection_oversize_evidence_refuses_atomically_without_omitting_faults() {
    let line = LineNumber::new(42).expect("line");
    let mut page = page(Source::Captured {
        site: SourceSite::new(PackagePath::new("src/entry.ts"), line),
        lines: Source::number_lines(&"source_body".repeat(DEFAULT_RESPONSE_BUDGET_BYTES), line),
        truncation: Truncation::Complete,
    });
    let answer = Answer::Page(Box::new(page.clone()));
    for detail in [Detail::Standard, Detail::Full] {
        let error = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect_err("complete typed source is too large; no partial success");
        assert_eq!(error.budget, DEFAULT_RESPONSE_BUDGET_BYTES);
        assert!(error.bytes > error.budget);
    }
    let summary = encoded(&answer, Detail::Summary);
    assert!(summary.get("source").is_none());
    assert_eq!(summary["notes"][0]["detail"], NOTE);
    page = page.with_notes(vec![Fault::usage(
        "members",
        "retained_fault".repeat(DEFAULT_RESPONSE_BUDGET_BYTES),
    )]);
    let answer = Answer::Page(Box::new(page));
    assert!(
        encode_answer(
            &answer,
            Detail::Summary,
            None,
            DEFAULT_RESPONSE_BUDGET_BYTES
        )
        .is_err(),
        "summary cannot drop actual faults to manufacture a fitting answer"
    );
}

#[test]
fn retained_external_document_page_keeps_actual_identity_and_explicit_source_refusal() {
    let native = symbol_key("retained-scoped-external-target");
    let coordinate = format!(
        "/abs/httpie::external::{}::HTTPieArgumentParser",
        backend_library::RowId::Symbol(native)
    );
    let root = backend_library::view_state_root(&[]);
    let basis = backend_library::Basis::new(
        root,
        backend_library::object_version(b"external-source-fixture"),
    );
    let row = backend_library::Row::new(backend_library::RowId::Symbol(native), basis, &coordinate);
    let document = backend_library::Document::new(native, root, row.document.clone())
        .with_source_basis(basis)
        .with_location(row.source.clone())
        .with_excerpt(row.excerpt.clone())
        .with_selection(
            backend_library::DocumentSelection::from_row(&row, basis)
                .expect("fixture native selection"),
        )
        .expect("actual row and exact basis");
    let page = crate::assemble::page_from_document(&coordinate, &document, &[], &[], Vec::new());
    assert_eq!(page.identity().key(), IdentityKey::Symbol(native));
    assert!(page.prose().is_empty());
    assert!(page.signature().is_none());
    assert!(page.source().site().is_none());
    let answer = Answer::Page(Box::new(page));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let value = encoded(&answer, detail);
        assert_eq!(value["identity"]["coordinate"], coordinate);
        assert_eq!(value["identity"]["shape"], "external");
        assert_eq!(
            value["identity"]["semantic_data"]["value"],
            json!(native.as_bytes())
        );
        assert!(value["identity"].get("path").is_none());
        assert!(value["identity"].get("line").is_none());
        assert_eq!(value["source_fault"]["slug"], "source-unavailable");
        assert_eq!(value["source_fault"]["cause"], "not-captured");
        assert_eq!(value["source_fault"]["operand"], coordinate);
        assert!(value.get("source").is_none());
        for rendered in [
            markdown::answer_with_detail(
                &answer,
                detail,
                None,
                ContinuationTarget::McpTool("backend.document"),
            ),
            text::answer_with_detail(
                &answer,
                Theme::plain(),
                detail,
                None,
                ContinuationTarget::CliOption,
            ),
        ] {
            assert!(rendered.contains(&coordinate));
            assert!(rendered.contains("source-unavailable"));
            assert!(!rendered.contains("not-found"));
        }
    }
}
