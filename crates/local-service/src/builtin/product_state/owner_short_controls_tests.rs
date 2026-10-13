#[test]
fn owner_subscription_transactions_match_general_path_and_cold_reopen() {
    let root = fixture("independent-subscriptions");
    let mut direct = ProductState::open(root.join("direct.json")).expect("direct state");
    let mut general = ProductState::open(root.join("general.json")).expect("general state");
    let package = PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("package");
    let name = ProjectName::new("Followed app").expect("project name");
    let project = direct.create_project(name.clone(), None).expect("direct project");
    assert_eq!(project, general.create_project(name, None).expect("general project"));
    let selected = view();
    let catalog = vec![registry_row("pkg:cargo/unrelated@1.0.0", "unrelated")];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let graph = indexed_graph(Vec::new());
    for command in [
        SurfaceCommand::Subscribe { package: package.clone(), project: None },
        SurfaceCommand::Subscribe { package: package.clone(), project: Some(ProjectSelector::Id(project.id)) },
        SurfaceCommand::Subscriptions,
        SurfaceCommand::Unsubscribe { package: package.clone() },
        SurfaceCommand::Unsubscribe { package: package.clone() },
        SurfaceCommand::Subscriptions,
    ] {
        let short = match command.clone() {
            SurfaceCommand::Subscribe { package, project } => OwnerSubscriptionCommand::Subscribe { package, project },
            SurfaceCommand::Unsubscribe { package } => OwnerSubscriptionCommand::Unsubscribe { package },
            SurfaceCommand::Subscriptions => OwnerSubscriptionCommand::List,
            _ => unreachable!("closed test commands"),
        };
        let actual = direct.execute_subscription(short).expect("short transaction");
        let expected = general.execute(command, &selected, &catalog, &index, &graph, None)
            .expect("general transaction");
        assert_eq!(actual, expected);
        assert_eq!(serde_json::to_vec(&actual).expect("short wire"), serde_json::to_vec(&expected).expect("general wire"));
        assert_eq!(direct.state, general.state, "same epoch and complete retained product state");
        assert_eq!(fs::read(&direct.path).expect("direct durable state"), fs::read(&general.path).expect("general durable state"));
        direct = ProductState::open(direct.path.clone()).expect("cold reopen");
        assert_eq!(direct.state, general.state);
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn owner_subscription_refusal_does_not_install_or_persist_partial_state() {
    let root = fixture("subscription-refusal");
    let path = root.join("state.json");
    let mut state = ProductState::open(path.clone()).expect("state");
    let package = PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("package");
    let before = state.state.clone();
    let refusal = state.execute_subscription(OwnerSubscriptionCommand::Subscribe {
        package: package.clone(),
        project: Some(ProjectSelector::Name(ProjectName::new("Absent").expect("name"))),
    }).expect_err("unknown project must still refuse");
    assert_eq!(refusal, "unknown project");
    assert_eq!(state.state, before);
    assert!(!path.exists(), "refusal precedes persistence");
    fs::create_dir(&path).expect("unwritable file target");
    assert!(state.execute_subscription(OwnerSubscriptionCommand::Subscribe { package, project: None }).is_err());
    assert_eq!(state.state, before, "failed atomic persistence cannot install pending state");
    assert!(path.is_dir());
    assert!(state.execute_subscription(OwnerSubscriptionCommand::List).is_ok(), "read needs no writer");
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn short_advisory_preserves_selected_catalog_and_exact_refusals() {
    let root = fixture("short-advisory");
    let mut state = ProductState::open(root.join("state.json")).expect("state");
    let package = PackageReference::parse("pkg:cargo/fixture@1.0.0").expect("package");
    let mut row = registry_row(package.as_str(), "fixture");
    let reasons = Box::new([backend_library::PolicyReason::IncompleteCoverage]);
    row.advisory.decision = backend_library::AcquisitionDecision::Deny(reasons.clone());
    row.advisory.reasons = reasons;
    let catalog = vec![row];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let selected = view();
    let graph = indexed_graph(Vec::new());
    let command = SurfaceCommand::Advisory { package: package.clone(), override_evidence: None };
    let short = read_advisory(&package, None, &catalog, &index).expect("short catalog read");
    assert_eq!(short, state.execute(command, &selected, &catalog, &index, &graph, None).expect("general catalog read"));
    assert_eq!(short, SurfaceReply::Advisory(catalog[0].advisory.clone()), "exact selected coverage/freshness/decision retained");
    let evidence = backend_library::OverrideEvidence {
        actor: "reviewer".to_owned(),
        reason: "explicit reviewed override".to_owned(),
        policy_version: 1,
        expires_at: None,
    };
    let short = read_advisory(&package, Some(evidence.clone()), &catalog, &index).expect("typed override");
    let general = state.execute(SurfaceCommand::Advisory { package: package.clone(), override_evidence: Some(evidence) }, &selected, &catalog, &index, &graph, None).expect("general typed override");
    assert_eq!(short, general, "same read-only override semantics");
    assert!(matches!(short, SurfaceReply::Advisory(value) if matches!(value.decision, backend_library::AcquisitionDecision::Warn(_))));
    let duplicate = vec![catalog[0].clone(), catalog[0].clone()];
    let duplicate_index = CatalogLookupIndex::from_catalog(&duplicate);
    assert!(read_advisory(&package, None, &duplicate, &duplicate_index).expect_err("ambiguous authority").contains("multiple registry authorities"));
    assert!(read_advisory(&package, None, &[], &index).expect_err("missing admitted row").contains("does not match"));
    assert!(!state.path.exists(), "advisory cannot mutate product state");
    fs::remove_dir_all(root).expect("remove fixture");
}
