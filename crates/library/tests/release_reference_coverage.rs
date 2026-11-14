//! Reference answers must say whether absence and partial lists are exhaustive.
use backend_library::{ProductText, SurfaceReply};

#[test]
fn reference_reply_exposes_coverage_even_when_empty() {
    let reply = SurfaceReply::References {
        target: ProductText::from_static("/work/project::file.ts:1::foo"),
        coverage: backend_library::ReferenceCoverage::default(),
        references: Box::new([]),
    };
    let json = serde_json::to_value(reply).expect("encode references");
    assert!(
        json["data"].get("coverage").is_some(),
        "empty references lack coverage: {json}"
    );
    assert_eq!(json["data"]["coverage"]["state"], "unknown");
    let old = serde_json::json!({"result": "references", "data": {"target": "/work/project::file.ts:1::foo", "references": []}});
    let decoded: SurfaceReply = serde_json::from_value(old).expect("legacy references reply");
    assert!(matches!(
        decoded,
        SurfaceReply::References {
            coverage: backend_library::ReferenceCoverage::Unknown { .. },
            ..
        }
    ));
}

#[test]
fn reference_reply_exposes_coverage_when_sites_are_present() {
    use backend_library::{
        ReferenceRecord, SemanticConfidence, SemanticDeclarationIdentity, SemanticLinkEvidence,
        SemanticLinkKind, SemanticLinkTarget,
    };
    let reply = SurfaceReply::References {
        target: ProductText::from_static("/work/project::file.ts:1::foo"),
        coverage: backend_library::ReferenceCoverage::Partial {
            reason: ProductText::from_static("TSX partial"),
        },
        references: Box::new([ReferenceRecord {
                site_symbol: None,
                source_policy: Default::default(),
            site: ProductText::from_static("/work/project::page.tsx:2::Page"),
            target: SemanticLinkTarget::Local {
                declaration: SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: SemanticLinkKind::Calls,
            evidence: SemanticLinkEvidence {
                confidence: SemanticConfidence::Syntactic,
                source: None,
            },
        }]),
    };
    let json = serde_json::to_value(reply).expect("encode references");
    assert!(
        json["data"].get("coverage").is_some(),
        "nonempty references lack coverage: {json}"
    );
    assert_eq!(json["data"]["coverage"]["state"], "partial");
    let mut legacy = json;
    legacy["data"].as_object_mut().expect("data object").remove("coverage");
    let decoded: SurfaceReply = serde_json::from_value(legacy).expect("legacy nonempty references reply");
    assert!(matches!(decoded, SurfaceReply::References { coverage: backend_library::ReferenceCoverage::Unknown { .. }, references, .. } if references.len() == 1));
}

#[test]
fn coverage_rejects_unknown_fields_and_variants() {
    for coverage in [
        serde_json::json!({"state": "complete", "extra": true}),
        serde_json::json!({"state": "partial", "reason": "TSX partial", "extra": true}),
        serde_json::json!({"state": "unknown", "reason": "TSX unavailable", "extra": true}),
        serde_json::json!({"state": "future-coverage"}),
    ] {
        let reply = serde_json::json!({"result": "references", "data": {
            "target": "fixture::target", "references": [], "coverage": coverage.clone(),
        }});
        assert!(serde_json::from_value::<SurfaceReply>(reply).is_err(), "unexpectedly admitted {coverage}");
    }
}
