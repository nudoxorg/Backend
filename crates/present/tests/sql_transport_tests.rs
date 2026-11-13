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
