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
