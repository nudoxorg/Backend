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

#[test]
fn css_impact_capture_cli_and_mcp_accept_the_same_typed_request() {
    use backend_library::impact_comparison::ImpactCaptureRequest;
    use backend_library::stylo_query::{StyloSourceClaims, StyloSourceReference};
    let seed = StyloSourceReference::issue(StyloSourceClaims { manifest_root: [1; 32], native_root: [2; 32], project: [3; 32], host: [4; 32], site: [5; 32] }, &[6; 32]).unwrap();
    let request = ImpactCaptureRequest { package: backend_library::PackageReference::parse(PROJECT).unwrap(), seed: Some(seed), scan_rows: 100, scan_bytes: 1_000_000, retain_bytes: 1_000_000, source_bytes: 500_000, temporary_bytes: 1_000_000, time_ms: 1000 };
    let grammar = grammar_for("impact-capture").expect("impact grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(&serde_json::to_string(&request).unwrap());
    let json = serde_json::json!({"request": request});
    let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
    let extract = |call: &Invocation| {
        let Request::Surface(command) = lower(call, PROJECT).expect("impact request") else { panic!("surface request") };
        let backend_library::SurfaceCommand::ImpactCapture { request } = *command else { panic!("capture request") };
        request
    };
    assert_eq!(extract(&cli), extract(&mcp));
}

#[test]
fn impact_cli_and_mcp_lower_the_same_bounded_request() {
    use backend_library::impact::{
        ImpactDirection, ImpactDomain, ImpactSeed, ImpactStrengthFilter, IMPACT_DEFAULT_DEPTH,
    };
    let grammar = grammar_for("impact").expect("impact grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(PROJECT);
    cli.set("custom-property", "--brand");
    cli.set("depth", "4");
    cli.raise("exact");
    let json = serde_json::json!({
        "package": PROJECT, "custom_property": "--brand", "depth": "4", "exact": true
    });
    let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
    let extract = |call: &Invocation| {
        let Request::Surface(command) = lower(call, PROJECT).expect("impact request") else {
            panic!("surface request")
        };
        let backend_library::SurfaceCommand::Impact { request } = *command else {
            panic!("impact request")
        };
        request
    };
    let left = extract(&cli);
    assert_eq!(left, extract(&mcp));
    assert_eq!(left.depth, 4);
    assert_eq!(left.strength, ImpactStrengthFilter::Exact);
    assert_eq!(left.direction, ImpactDirection::Dependents);
    let ImpactSeed::Named(spec) = &left.seed else { panic!("named seed") };
    assert_eq!((spec.domain, spec.kind.as_str(), spec.name.as_str()), (ImpactDomain::Css, "custom-property", "--brand"));
    // Defaults come from the contract, not from either transport.
    let mut plain = Invocation::new(grammar);
    plain.push(PROJECT);
    plain.set("class", "card");
    assert_eq!(extract(&plain).depth, IMPACT_DEFAULT_DEPTH);
}

#[test]
fn impact_needs_exactly_one_seed_and_refuses_unbounded_questions() {
    let grammar = grammar_for("impact").expect("impact grammar");
    let lowered = |build: &dyn Fn(&mut Invocation)| {
        let mut call = Invocation::new(grammar);
        call.push(PROJECT);
        build(&mut call);
        lower(&call, PROJECT)
    };
    assert!(lowered(&|_| {}).is_err(), "no seed");
    assert!(lowered(&|c| { c.set("class", "a"); c.set("id", "b"); }).is_err(), "two seeds");
    assert!(lowered(&|c| { c.set("class", "a"); c.set("depth", "0"); }).is_err(), "zero depth");
    assert!(lowered(&|c| { c.set("class", "a"); c.set("depth", "99"); }).is_err(), "huge depth");
    assert!(lowered(&|c| { c.set("class", "a"); c.set("direction", "sideways"); }).is_err());
    assert!(lowered(&|c| c.set("class", "a")).is_ok());
}
