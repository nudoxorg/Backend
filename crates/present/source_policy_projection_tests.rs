//! Presentation-only fixtures: issued test claims are not owner admission.
use crate::{Answer, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Theme, encode_answer, markdown, text};
use backend_library::{
    Basis, DeclarationReadExtent, DeclarationRecord, Document, DocumentSelection, ProductText, Row,
    RowId, SemanticLanguageProfile, SourcePolicyObservation, SourcePolicyQualification,
    SurfaceReply, object_version, symbol_key, view_state_root,
};

fn fixture() -> (Document, Row) {
    fixture_for("/project::source.ts:1::f", 9)
}

fn fixture_for(coordinate: &str, entity_index: u32) -> (Document, Row) {
    let symbol = symbol_key(coordinate);
    let basis = Basis::new(view_state_root(&[]), object_version(b"fixture"));
    let row = Row::new(RowId::Symbol(symbol), basis, coordinate);
    let selection = DocumentSelection::from_row(&row, basis).expect("selection");
    let observation = SourcePolicyObservation {
        workspace_root: [1; 32],
        evidence_root: [2; 32],
        project: [3; 32],
        profile: SemanticLanguageProfile::from_name("typescript").expect("profile"),
        publication_profile: SemanticLanguageProfile::from_name("typescript")
            .expect("publication profile"),
        publication_coordinate: "pkg:npm/fixture@1.0.0".to_owned(),
        source_path: "source.ts".to_owned(),
        source_fact: [4; 32],
        input_digest: [5; 32],
        byte_len: 12,
        publication_binding: [6; 32],
        image_identity: [7; 32],
        image_byte_len: 128,
        program_membership: [8; 32],
        entity_index,
        declaration_family: [10; 16],
        declaration_variant: [11; 16],
    };
    let claim = SourcePolicyQualification::issue_observed(&[12; 32], &selection, observation)
        .expect("unverified fixture claim");
    let mut document = Document::new(symbol, basis.root, [])
        .with_source_basis(basis)
        .with_selection(selection)
        .expect("selected");
    document.source_policy = claim;
    (document, row)
}

#[test]
fn policy_claim_survives_document_summary_standard_full_and_budget() {
    let (document, row) = fixture();
    let page = crate::page_from_document(
        "/project::source.ts:1::f",
        &document,
        &[row],
        &[],
        Vec::new(),
    );
    let answer = Answer::Page(Box::new(page));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded page");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(value["source_policy"]["state"], "captured_observed");
        assert_eq!(
            value["source_policy"]["data"]["statement"]["selection_coordinate"],
            "/project::source.ts:1::f"
        );
        assert!(encode_answer(&answer, detail, None, encoded.bytes.len() - 1).is_err());
    }
    assert!(text::answer(&answer, Theme::plain()).contains("Unverified captured-source claim"));
    assert!(markdown::answer(&answer).contains("Unverified captured-source claim"));
}

#[test]
fn opaque_native_coordinate_renders_observed_location_and_captured_excerpt() {
    let coordinate = "/project::semantic::abc123::f";
    let (mut document, row) = fixture_for(coordinate, 9);
    document.location = backend_library::SourceAvailability::Captured(
        backend_library::SourceLocation::new("source.ts", 3).expect("observed site"),
    );
    document.excerpt = backend_library::SourceExcerpt::captured(
        "function f() {\r\n  return 'é';\r\n}",
        backend_library::SourceExcerptExtent::Truncated,
    ).expect("captured excerpt");
    let page = crate::page_from_document(coordinate, &document, &[row], &[], Vec::new());
    let answer = Answer::Page(Box::new(page));
    let value: serde_json::Value = serde_json::from_slice(
        &encode_answer(&answer, Detail::Standard, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded source").bytes,
    ).expect("JSON");
    assert_eq!(value["source_policy"]["state"], "captured_observed");
    assert_eq!(value["source_policy"]["data"]["statement"]["policy_stability"], "unknown");
    let rendered = text::answer(&answer, Theme::plain());
    assert!(rendered.contains("source.ts"));
    assert!(rendered.contains("function f()"));
    assert!(rendered.contains("return 'é'"));
    assert!(!rendered.contains("not-captured"));
}

#[test]
fn policy_claim_stays_with_its_read_record() {
    let (document, _) = fixture();
    let make = |label: &str, policy| DeclarationRecord {
        source_policy: policy,
        label: ProductText::new(label).expect("label"),
        stable_id: Some([4; 32]),
        signature: Some(ProductText::new("fn f()").expect("signature")),
        extent: Some(DeclarationReadExtent::Truncated),
    };
    let reply = SurfaceReply::Read(
        vec![
            make("first", document.source_policy),
            make("second", SourcePolicyQualification::Unknown),
        ]
        .into_boxed_slice(),
    );
    let answer = Answer::Product(Box::new(crate::product_view(&reply)));
    for rendered in [
        text::answer(&answer, Theme::plain()),
        markdown::answer(&answer),
    ] {
        assert_eq!(
            rendered.matches("Unverified captured-source claim").count(),
            1
        );
    }
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded read");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(
            value["records"][0]["source_policy"]["state"],
            "captured_observed"
        );
        assert!(value["records"][1].get("source_policy").is_none());
    }
}

#[test]
fn policy_claim_survives_source_page_without_changing_raw_bytes() {
    let (document, _) = fixture();
    let raw = "é漢🦀\r\nlast\r\n";
    let len = u32::try_from(raw.len()).expect("bounded");
    let source = backend_library::SourceRangePage {
        source_policy: document.source_policy,
        text: raw.to_owned(),
        start: 0,
        end: len,
        total: len,
        scope: backend_library::SourceRangeScope::NativePrimaryDeclaration,
        source_identity: [4; 32],
        owner_start: 0,
        owner_end: len,
    };
    let range = crate::DeclarationSourceRange {
        revision: "ab".repeat(32),
        selected_symbol: "cd".repeat(32),
        coordinate: None,
        verified_at_read: true,
        source,
        next_cursor: None,
    };
    let answer = Answer::Product(Box::new(
        crate::ProductView::assembled("source", Vec::new()).with_declaration_source_range(range),
    ));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded source");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(value["declaration_source_range"]["source"]["text"], raw);
        assert_eq!(
            value["declaration_source_range"]["source"]["source_policy"]["state"],
            "captured_observed"
        );
        assert!(encode_answer(&answer, detail, None, encoded.bytes.len() - 1).is_err());
    }
    for rendered in [
        text::answer(&answer, Theme::plain()),
        markdown::answer(&answer),
    ] {
        assert!(rendered.contains(raw));
        assert!(rendered.contains("Source bytes match the indexed capture"));
        assert!(rendered.contains("unverified policy claim; ignore-policy stability unknown"));
    }
}

#[test]
fn unknown_policy_is_not_promoted_to_an_observed_claim() {
    let (mut document, row) = fixture();
    document.source_policy = SourcePolicyQualification::Unknown;
    let answer = Answer::Page(Box::new(crate::page_from_document(
        "/project::source.ts:1::f",
        &document,
        &[row],
        &[],
        Vec::new(),
    )));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded page");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert!(value.get("source_policy").is_none());
    }
    for rendered in [
        text::answer(&answer, Theme::plain()),
        markdown::answer(&answer),
    ] {
        assert!(!rendered.contains("Unverified captured-source claim"));
    }
}

#[test]
fn same_file_observed_claims_remain_on_their_selected_read_rows() {
    let (first, _) = fixture_for("/project::source.ts:1::alpha", 9);
    let (second, _) = fixture_for("/project::source.ts:2::bravo", 10);
    assert_ne!(first.symbol, second.symbol);
    assert_eq!(first.source_basis(), second.source_basis());
    let expected_first = serde_json::to_value(&first.source_policy).expect("first claim");
    let expected_second = serde_json::to_value(&second.source_policy).expect("second claim");
    assert_ne!(expected_first, expected_second);
    assert_eq!(
        expected_first["data"]["statement"]["selection_coordinate"],
        "/project::source.ts:1::alpha"
    );
    assert_eq!(
        expected_second["data"]["statement"]["selection_coordinate"],
        "/project::source.ts:2::bravo"
    );
    let mut missing_coordinate = expected_first.clone();
    missing_coordinate["data"]["statement"]
        .as_object_mut()
        .expect("statement")
        .remove("selection_coordinate");
    assert!(serde_json::from_value::<SourcePolicyQualification>(missing_coordinate).is_err());
    let record = |label: &str, document: Document| DeclarationRecord {
        source_policy: document.source_policy,
        label: ProductText::new(label).expect("label"),
        stable_id: Some(*document.symbol.as_bytes()),
        signature: Some(ProductText::new("fn observed()").expect("signature")),
        extent: Some(DeclarationReadExtent::Truncated),
    };
    let reply = SurfaceReply::Read(
        vec![
            record("/project::source.ts:1::alpha", first),
            record("/project::source.ts:2::bravo", second),
        ]
        .into_boxed_slice(),
    );
    let answer = Answer::Product(Box::new(crate::product_view(&reply)));
    for rendered in [
        text::answer(&answer, Theme::plain()),
        markdown::answer(&answer),
    ] {
        assert!(
            rendered.contains("Unverified captured-source claim for /project::source.ts:1::alpha")
        );
        assert!(
            rendered.contains("Unverified captured-source claim for /project::source.ts:2::bravo")
        );
        assert_eq!(
            rendered
                .matches("effective ignore-policy stability unknown")
                .count(),
            2
        );
    }
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded read");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(value["records"][0]["source_policy"], expected_first);
        assert_eq!(value["records"][1]["source_policy"], expected_second);
        assert!(encode_answer(&answer, detail, None, encoded.bytes.len() - 1).is_err());
    }
}

#[test]
fn references_keep_each_unverified_policy_with_its_own_site() {
    let (first, _) = fixture_for("/project::source.ts:1::alpha", 9);
    let (second, _) = fixture_for("/project::source.ts:2::bravo", 10);
    let reference =
        |document: Document, label: &str, start: u32| backend_library::ReferenceRecord {
            site_symbol: Some(*document.symbol.as_bytes()),
            source_policy: document.source_policy,
            site: ProductText::new(label).expect("site"),
            target: backend_library::SemanticLinkTarget::Local {
                declaration: backend_library::SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_library::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Compiler,
                source: Some(backend_library::SemanticSourceSpan {
                    file: ProductText::new("source.ts").expect("path"),
                    start,
                    end: start + 1,
                }),
            },
        };
    // Presentation-only raw claims: these fixtures are not admitted owner evidence.
    let first_symbol = *first.symbol.as_bytes();
    let second_symbol = *second.symbol.as_bytes();
    let first_policy = first.source_policy.clone();
    let second_policy = second.source_policy.clone();
    let reply = SurfaceReply::References {
        target: ProductText::new("target").expect("target"),
        coverage: backend_library::ReferenceCoverage::Partial {
            reason: ProductText::new("unknown policy").expect("reason"),
        },
        references: vec![
            reference(first.clone(), "first site", 1),
            reference(first, "first site", 3),
            reference(second, "second site", 5),
        ]
        .into_boxed_slice(),
    };
    let answer = Answer::Product(Box::new(crate::product_view(&reply)));
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
            .expect("bounded references");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(
            value["records"][0]["site_symbol"],
            serde_json::to_value(first_symbol).expect("first id")
        );
        assert_eq!(
            value["records"][2]["site_symbol"],
            serde_json::to_value(second_symbol).expect("second id")
        );
        assert_eq!(
            value["records"][0]["source_policy"],
            serde_json::to_value(&first_policy).expect("first")
        );
        assert_eq!(
            value["records"][2]["source_policy"],
            serde_json::to_value(&second_policy).expect("second")
        );
        assert_eq!(
            value["records"][0]["reference_details"]["evidence"]["source"]["start"],
            1
        );
        assert_eq!(
            value["records"][1]["reference_details"]["evidence"]["source"]["start"],
            3
        );
        assert_eq!(
            value["records"][0]["site_symbol"],
            value["records"][1]["site_symbol"]
        );
        assert_eq!(
            value["records"][0]["source_policy"],
            value["records"][1]["source_policy"]
        );
        assert!(encode_answer(&answer, detail, None, encoded.bytes.len() - 1).is_err());
    }
    for rendered in [
        text::answer(&answer, Theme::plain()),
        markdown::answer(&answer),
    ] {
        assert!(rendered.contains("claim for /project::source.ts:1::alpha"));
        assert!(rendered.contains("claim for /project::source.ts:2::bravo"));
        assert!(rendered.contains("unknown policy"));
    }
}

#[test]
fn centerless_graph_page_preserves_edges_witness_and_continuation() {
    use backend_library::{
        Cursor, Freshness, Frontier, GraphAvailability, GraphEdgeKind, GraphNodeId,
        GraphProvenance, RichGraphBuilder, RichGraphEdge, RichGraphNode, RichGraphRevision,
        SemanticConfidence, SemanticGenerationId, SemanticLinkKind, ViewRoot, ViewSnapshot,
        view_key,
    };
    let (center_document, _) = fixture_for("/project::source.ts:1::center", 9);
    let (caller_document, caller_row) = fixture_for("/project::source.ts:2::caller", 10);
    let basis = center_document.source_basis().expect("basis");
    let revision = RichGraphRevision::new(
        basis.root.to_bytes(),
        SemanticGenerationId::new([1; 32]),
        [2; 32],
    );
    let center = GraphNodeId::for_symbol(center_document.symbol);
    let caller = GraphNodeId::for_symbol(caller_document.symbol);
    let mut builder = RichGraphBuilder::new(revision, center);
    for (id, name) in [(center, "center witness"), (caller, "caller")] {
        builder
            .add_node(
                RichGraphNode::new(id, name, None, None, GraphAvailability::Ready, None)
                    .expect("node"),
            )
            .expect("add");
    }
    let make_edge = |relation| {
        RichGraphEdge::new(
            caller,
            center,
            GraphEdgeKind::Code { relation },
            GraphAvailability::Ready,
            GraphProvenance::semantic(
                revision.semantic_generation,
                revision.semantic_root,
                [3; 32],
                SemanticConfidence::Compiler,
            ),
        )
    };
    let edge = make_edge(SemanticLinkKind::Calls);
    let edge_id = edge.id;
    builder.add_edge(edge).expect("edge");
    let unknown_edge = make_edge(SemanticLinkKind::Reads);
    let unknown_id = unknown_edge.id;
    builder.add_edge(unknown_edge).expect("second edge");
    let mut graph = builder.finish().expect("fixture graph");
    graph.availability =
        GraphAvailability::unavailable("policy stability unknown").expect("reason");
    for node in std::sync::Arc::make_mut(&mut graph.nodes) {
        node.source_policy = if node.id == center {
            center_document.source_policy.clone()
        } else {
            caller_document.source_policy.clone()
        };
    }
    std::sync::Arc::make_mut(&mut graph.edges)
        .iter_mut()
        .find(|edge| edge.id == edge_id)
        .expect("caller edge")
        .source_policy = caller_document.source_policy.clone();
    let root = ViewRoot::new_incomplete(
        view_key(b"centerless graph projection"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 1),
        vec![caller_row],
        vec![],
    )
    .expect("caller-only page");
    let next = Cursor::for_view_root(&root);
    let snapshot = ViewSnapshot {
        root,
        freshness: Freshness::Current,
        next: Some(next),
        graph_relations: None,
        rich_graph: Some(graph),
    };
    // These are raw claims in a projection fixture, not authenticated owner evidence.
    let list = crate::graph_page_records("center", &snapshot);
    assert_eq!(list.records().len(), 1, "center witness is not a page row");
    assert!(list.continuation().is_some());
    let answer = Answer::Records(Box::new(list));
    for rendered in [
        text::answer(&answer, Theme::plain()),
        markdown::answer(&answer),
    ] {
        assert!(rendered.contains("graph edge "));
        assert!(rendered.contains("policy stability unknown"));
    }
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(
            &answer,
            detail,
            Some("fixture-cursor"),
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("bounded graph");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(value["records"].as_array().expect("rows").len(), 1);
        assert_eq!(
            value["graph_center"],
            serde_json::to_value(center).expect("center")
        );
        let node_claims = value["graph_source_policies"]
            .as_array()
            .expect("node claims");
        assert_eq!(node_claims.len(), 2);
        assert!(node_claims.iter().any(|node| node["id"]
            == serde_json::to_value(center).expect("center")
            && node["source_policy"]
                == serde_json::to_value(&center_document.source_policy).expect("center claim")));
        assert!(node_claims.iter().any(|node| node["id"]
            == serde_json::to_value(caller).expect("caller")
            && node["source_policy"]
                == serde_json::to_value(&caller_document.source_policy).expect("caller claim")));
        let edges = value["graph_edges"].as_array().expect("edges");
        assert_eq!(edges.len(), 2);
        let captured = edges
            .iter()
            .find(|edge| edge["id"] == serde_json::to_value(edge_id).expect("id"))
            .expect("captured edge");
        let unknown = edges
            .iter()
            .find(|edge| edge["id"] == serde_json::to_value(unknown_id).expect("id"))
            .expect("unknown edge");
        for edge in edges {
            assert_eq!(edge["from"], serde_json::to_value(caller).expect("from"));
            assert_eq!(edge["to"], serde_json::to_value(center).expect("to"));
            assert_eq!(edge["provenance"]["confidence"], "compiler");
        }
        assert_eq!(
            captured["source_policy"],
            serde_json::to_value(&caller_document.source_policy).expect("claim")
        );
        assert!(unknown.get("source_policy").is_none());
        assert_ne!(captured["kind"], unknown["kind"]);
        assert_eq!(value["graph_gap"], "policy stability unknown");
        let preview = markdown::summary_with_cursor(
            &answer,
            Some("fixture-cursor"),
            crate::ContinuationTarget::McpTool("backend.graph"),
        );
        assert!(preview.contains("structuredContent.graph_edges"));
        assert!(preview.contains("Continue `backend.graph`"));
        let shortened =
            markdown::shorten_record_preview(&preview, preview.len()).expect("bounded preview");
        assert!(shortened.contains("structuredContent.graph_edges"));
        assert!(shortened.contains("Continue `backend.graph`"));
        assert!(
            encode_answer(
                &answer,
                detail,
                Some("fixture-cursor"),
                encoded.bytes.len() - 1
            )
            .is_err()
        );
    }
}
