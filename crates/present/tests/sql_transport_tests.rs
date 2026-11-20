use super::*;
use backend_library::sql_query::SqlQueryRole;

#[test]
fn cli_and_mcp_lower_same_exact_sql_filter() {
    let grammar = grammar_for("sql-query").expect("SQL grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(PROJECT);
    cli.set("role", "relation");
    cli.set("name", "Users");
    let json = serde_json::json!({"package": PROJECT, "role": "relation", "name": "Users"});
    let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
    let extract = |call: &Invocation| {
        let Request::Surface(command) = lower(call, PROJECT).expect("SQL request") else {
            panic!("surface")
        };
        let backend_library::SurfaceCommand::SqlQuery { request } = *command else {
            panic!("SQL query")
        };
        request
    };
    let left = extract(&cli);
    let right = extract(&mcp);
    assert_eq!(left, right);
    assert_eq!(left.role, SqlQueryRole::Relation);
    assert_eq!(left.name.as_deref(), Some("Users"));
    cli.set("role", "diagnostic");
    assert!(lower(&cli, PROJECT).is_err());
}

#[test]
fn postgres_column_read_does_not_accept_guessed_name_filter() {
    let grammar = grammar_for("sql-query").expect("SQL grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(PROJECT);
    cli.set("role", "column_read");
    assert!(lower(&cli, PROJECT).is_ok());
    cli.set("name", "id");
    assert!(lower(&cli, PROJECT).is_err());
}

fn role_less(options: &[(&'static str, &str)]) -> backend_library::sql_query::SqlQueryRequest {
    let grammar = grammar_for("sql-query").expect("SQL grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(PROJECT);
    let mut json = serde_json::json!({"package": PROJECT});
    for (key, value) in options {
        cli.set(*key, *value);
        json[*key] = serde_json::json!(*value);
    }
    let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
    let extract = |call: &Invocation| {
        let Request::Surface(command) = lower(call, PROJECT).expect("role-less SQL request") else {
            panic!("surface")
        };
        let backend_library::SurfaceCommand::SqlQuery { request } = *command else {
            panic!("SQL query")
        };
        request
    };
    let request = extract(&cli);
    assert_eq!(request, extract(&mcp), "CLI and MCP agree");
    request
}

#[test]
fn role_less_filters_select_the_fact_family_they_belong_to() {
    let uses = role_less(&[("uses", "members.full_name")]);
    assert_eq!(uses.role, SqlQueryRole::Reference);
    assert_eq!(uses.uses.as_deref(), Some("members.full_name"));

    let parent = role_less(&[("parent", "members")]);
    assert_eq!(parent.role, SqlQueryRole::Definition);

    let name = role_less(&[("name", "members")]);
    assert_eq!(name.role, SqlQueryRole::Any);
    assert_eq!(name.name.as_deref(), Some("members"));
    name.validate().expect("a bare name is a valid query");

    let kind = role_less(&[("name", "members"), ("kind", "table")]);
    assert_eq!(kind.role, SqlQueryRole::Any);
    kind.validate().expect("name and kind together");

    let bare = role_less(&[]);
    assert_eq!(bare.role, SqlQueryRole::Any);
}

#[test]
fn explicit_role_still_wins_over_the_implied_one() {
    let grammar = grammar_for("sql-query").expect("SQL grammar");
    let mut cli = Invocation::new(grammar);
    cli.push(PROJECT);
    cli.set("role", "definition");
    cli.set("uses", "members");
    assert!(lower(&cli, PROJECT).is_err(), "--uses is reference-only and says so");
}

#[test]
fn view_index_and_function_seeds_lower_identically_from_cli_and_mcp() {
    use backend_library::impact::{ImpactDomain, ImpactSeed};
    let grammar = grammar_for("impact").expect("impact grammar");
    for (option, json_name, kind) in [
        ("sql-view", "sql_view", "view"),
        ("sql-index", "sql_index", "index"),
        ("sql-function", "sql_function", "function"),
        ("sql-trigger", "sql_trigger", "trigger"),
        ("sql-table", "sql_table", "table"),
        ("sql-column", "sql_column", "column"),
    ] {
        let mut cli = Invocation::new(grammar);
        cli.push(PROJECT);
        cli.set(option, "account_spend");
        let json = serde_json::json!({"package": PROJECT, json_name: "account_spend"});
        let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
        let extract = |call: &Invocation| {
            let Request::Surface(command) = lower(call, PROJECT).expect("impact request") else {
                panic!("surface")
            };
            let backend_library::SurfaceCommand::Impact { request } = *command else {
                panic!("impact")
            };
            request
        };
        let left = extract(&cli);
        assert_eq!(left, extract(&mcp), "{option}");
        let ImpactSeed::Named(spec) = &left.seed else { panic!("named seed for {option}") };
        assert_eq!((spec.domain, spec.kind.as_str(), spec.name.as_str()), (ImpactDomain::Sql, kind, "account_spend"));
    }
    // Two seeds are still refused, now naming the SQL ones.
    let mut both = Invocation::new(grammar);
    both.push(PROJECT);
    both.set("sql-view", "a");
    both.set("sql-index", "b");
    assert!(lower(&both, PROJECT).is_err());
}

#[test]
fn typed_uses_seeds_lower_to_a_kind_qualified_reference_query() {
    let grammar = grammar_for("sql-query").expect("SQL grammar");
    for (option, json_name, expected) in [
        ("uses-view", "uses_view", "view:account_spend"),
        ("uses-index", "uses_index", "index:account_spend"),
        ("uses-function", "uses_function", "function:account_spend"),
    ] {
        let mut cli = Invocation::new(grammar);
        cli.push(PROJECT);
        cli.set(option, "account_spend");
        let json = serde_json::json!({"package": PROJECT, json_name: "account_spend"});
        let mcp = Invocation::from_json(grammar, json.as_object().unwrap()).expect("MCP input");
        let extract = |call: &Invocation| {
            let Request::Surface(command) = lower(call, PROJECT).expect("SQL request") else {
                panic!("surface")
            };
            let backend_library::SurfaceCommand::SqlQuery { request } = *command else {
                panic!("SQL query")
            };
            request
        };
        let left = extract(&cli);
        assert_eq!(left, extract(&mcp), "{option}");
        // A seed means "who mentions this", so the role defaults to references.
        assert_eq!(left.role, SqlQueryRole::Reference);
        assert_eq!(left.uses.as_deref(), Some(expected));
    }
    let mut two = Invocation::new(grammar);
    two.push(PROJECT);
    two.set("uses-view", "a");
    two.set("uses", "b");
    assert!(lower(&two, PROJECT).is_err());
}
