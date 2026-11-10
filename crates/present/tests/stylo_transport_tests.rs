use super::*;
use backend_library::stylo_query::StyloNameRole;

#[test]
fn cli_and_mcp_lower_the_same_exact_css_name() {
    let grammar = grammar_for("stylo-query").expect("CSS grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(PROJECT);
    cli.set("class", ".foo");
    let json = serde_json::json!({"package": PROJECT, "class": ".foo"});
    let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
    let extract = |call: &Invocation| {
        let Request::Surface(command) = lower(call, PROJECT).expect("CSS request") else {
            panic!("surface request")
        };
        let backend_library::SurfaceCommand::StyloQuery { request } = *command else {
            panic!("CSS query")
        };
        request
    };
    let left = extract(&cli);
    let right = extract(&mcp);
    assert_eq!(left, right);
    assert_eq!(left.role, StyloNameRole::Class);
    assert_eq!(left.name, ".foo");
    cli.set("custom-property", "--tone");
    assert!(lower(&cli, PROJECT).is_err());
}

#[test]
fn css_summary_distinguishes_enumeration_from_host_health_and_reports_recorded_failure() {
    use backend_library::stylo_query::*;
    let page = StyloQueryPage {
        basis: StyloQueryBasis {
            manifest_root: [1; 32], native_root: Some([2; 32]),
            attempt: StyloAttemptState::LatestObserved,
            worker_capability: StyloWorkerCapability::Admitted([3; 32]),
            host_terminal_summary: StyloHostTerminalSummary::Unknown,
            source_partial: true, gaps: vec![
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
            ], producer: Some([4; 32]),
            selection_complete: false,
        },
        hits: vec![], terminal: StyloQueryTerminal::Complete,
    };
    let observed = product_view(&backend_library::SurfaceReply::StyloQuery(page.clone()));
    let text = observed.note().expect("summary");
    assert!(text.contains("scan of retained records complete"));
    assert!(text.contains("host parse health unknown"));
    assert!(!text.contains("worker or its protocol failed"));

    let mut failed = page;
    failed.basis.attempt = StyloAttemptState::RetainedAfterUnavailable { reason: StyloFailure::WorkerFailure };
    let failed = product_view(&backend_library::SurfaceReply::StyloQuery(failed));
    let text = failed.note().expect("summary");
    assert!(text.contains("Retained prior observation"));
    assert!(text.contains("CSS worker or its protocol failed"));
    assert!(text.contains("host parse health unknown"));
}
