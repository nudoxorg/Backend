//! Real MCP tool envelope around a typed CSS reply; owner provenance is tested separately.
use super::*;
use backend_library::stylo_query::*;

fn css_page() -> StyloQueryPage {
    StyloQueryPage {
        basis: StyloQueryBasis {
            manifest_root: [1; 32],
            native_root: Some([2; 32]),
            attempt: StyloAttemptState::LatestObserved,
            worker_capability: StyloWorkerCapability::Unknown,
            host_terminal_summary: StyloHostTerminalSummary::Unknown,
            source_partial: true,
            gaps: vec![
                StyloScopeGap::AuthoredSourcePartial,
                StyloScopeGap::AuthoredReferenceSitesUnavailable,
                StyloScopeGap::OrdinaryVariableReferencesUnavailable,
                StyloScopeGap::NativeDeclarationNameDecodeUnavailable,
                StyloScopeGap::PreferenceStabilityUnknown,
                StyloScopeGap::ImportsNotResolved,
                StyloScopeGap::AncestorRuleContextNotReturned,
                StyloScopeGap::FrameworkNotInterpreted,
                StyloScopeGap::DomAndCascadeNotAttempted,
                StyloScopeGap::ProviderUnknown,
            ],
            producer: Some([3; 32]),
            selection_complete: false,
        },
        hits: Vec::new(),
        terminal: StyloQueryTerminal::Complete,
    }
}

#[test]
fn css_tool_summary_keeps_unknown_health_and_explains_recorded_worker_failure() {
    let mut page = css_page();
    page.basis.attempt = StyloAttemptState::RetainedAfterUnavailable { reason: StyloFailure::WorkerFailure };
    let mut server = ready(Fake { surface_reply: Some(SurfaceReply::StyloQuery(page)), ..Fake::default() });
    let result = call(&mut server, "backend.stylo_query", &json!({"package":PROJECT,"class":"foo"}));
    let text = text_of(&result);
    assert!(text.contains("CSS worker or its protocol failed"), "{result}");
    assert!(text.contains("host parse health unknown"), "{result}");
    assert!(text.contains("scan of retained records complete"), "{result}");
}
#[test]
fn css_tool_default_and_full_retain_bounded_typed_scope() {
    let page = css_page();
    for detail in ["summary", "full"] {
        let mut server = ready(Fake {
            surface_reply: Some(SurfaceReply::StyloQuery(page.clone())),
            ..Fake::default()
        });
        let response = request(
            &mut server,
            "tools/call",
            &json!({"name":"backend.stylo_query", "arguments":{"package":PROJECT,"class":"foo","detail":detail}}),
        );
        assert_context_bounded(&response);
        assert!(wire_tokens(&response).expect("actual tool envelope") <= MCP_CL100K_TOKEN_LIMIT);
        let result = &response["result"];
        assert_eq!(result["isError"], false, "{response}");
        let dto: backend_present::ProductDto =
            serde_json::from_value(result["structuredContent"].clone())
                .expect("typed product output");
        match (detail, dto.semantic_data) {
            ("summary", Some(backend_present::ProductSemanticData::StyloQuerySummary(summary))) => {
                assert_eq!(summary.basis, page.basis);
                assert_eq!(summary.terminal, StyloQueryTerminal::Complete);
            }
            ("full", Some(backend_present::ProductSemanticData::StyloQuery(full))) => {
                assert_eq!(full, page)
            }
            (_, other) => panic!("unexpected CSS detail output: {other:?}"),
        }
    }
}
