# Real owner CSS/SQL integration: these are separate from worker-frame cases.
# The expected source handles come from the owner and are read back through
# public CLI and MCP; they are not guessed or blessed into this manifest.
let
  read = path: builtins.readFile path;
  eq = pointer: equals: { inherit pointer equals; };
  project = "\${project}";
  css = id: option: field: name: title: {
    inherit id; kind = "query"; wait_for = true;
    cli = [ "stylo-query" project option name ];
    tool = "backend.stylo_query";
    arguments = { package = project; } // { "${field}" = name; };
    expect = { exit = 0; json = [
      (eq "/answer" "product")
      (eq "/semantic_data/kind" "stylo-query-summary")
      (eq "/semantic_data/value/basis/attempt/state" "latest_observed")
      (eq "/semantic_data/value/basis/worker_capability/state" "admitted")
      (eq "/semantic_data/value/terminal/state" "complete")
      { pointer = "/records"; select = "/title"; contains = title; }
    ]; };
    bind = { css_source = "/semantic_data/value/first_site"; };
  };
  sql = id: role: name: title: {
    inherit id; kind = "query"; wait_for = true;
    cli = [ "sql-query" project "--role" role "--name" name ];
    tool = "backend.sql_query";
    arguments = { package = project; inherit role name; };
    expect = { exit = 0; json = [
      (eq "/answer" "product")
      (eq "/semantic_data/kind" "sql-query-summary")
      (eq "/semantic_data/value/terminal/state" "complete")
      (eq "/semantic_data/value/basis/catalog_is_deployed" false)
      (eq "/semantic_data/value/basis/attempt/state" "latest_observed")
      (eq "/semantic_data/value/basis/inventory_complete" true)
      (eq "/semantic_data/value/basis/gaps/unavailable_hosts" 0)
      (eq "/semantic_data/value/basis/gaps/native_error_hosts" 0)
      { pointer = "/records"; select = "/title"; contains = title; }
    ]; };
    bind = { sql_source = "/semantic_data/value/first_source"; };
  };
  noCss = id: name: {
    inherit id; kind = "query"; wait_for = true;
    cli = [ "stylo-query" project "--class" name ];
    tool = "backend.stylo_query"; arguments = { package = project; class = name; };
    expect = { exit = 0; json = [
      (eq "/semantic_data/value/basis/attempt/state" "latest_observed")
      (eq "/semantic_data/value/basis/worker_capability/state" "admitted")
      (eq "/semantic_data/value/terminal/state" "complete")
      (eq "/records" [ ])
    ]; };
  };
  noSql = id: name: {
    inherit id; kind = "query"; wait_for = true;
    cli = [ "sql-query" project "--role" "definition" "--name" name ];
    tool = "backend.sql_query"; arguments = { package = project; role = "definition"; inherit name; };
    expect = { exit = 0; json = [
      (eq "/semantic_data/value/terminal/state" "complete")
      (eq "/semantic_data/value/basis/attempt/state" "latest_observed")
      (eq "/semantic_data/value/basis/inventory_complete" true)
      (eq "/semantic_data/value/basis/gaps/unavailable_hosts" 0)
      (eq "/semantic_data/value/basis/gaps/native_error_hosts" 0)
      (eq "/records" [ ])
    ]; };
  };
  cssRead = id: text: {
    inherit id; kind = "query";
    cli = [ "stylo-read-site" "\${css_source}" ];
    tool = "backend.stylo_read_site";
    arguments.reference = "\${css_source}";
    expect = { exit = 0; json = [
      (eq "/semantic_data/value/state" "available")
      (eq "/semantic_data/value/path" "styles/site.css")
      (eq "/semantic_data/value/text" text)
    ]; };
  };
  sqlRead = id: text: {
    inherit id; kind = "query";
    cli = [ "sql-read-source" "\${sql_source}" ];
    tool = "backend.sql_read_source";
    arguments.reference = "\${sql_source}";
    expect = { exit = 0; json = [
      (eq "/semantic_data/value/state" "available")
      (eq "/semantic_data/value/path" "db/schema.sql")
      (eq "/semantic_data/value/text" text)
    ]; };
  };
in {
  schema = 1;
  cases = [ {
    id = "owner-css-sql-history"; language = "rust"; node = "engine.lanes";
    files = {
      "Cargo.toml" = read ./fixtures/owner-native/Cargo.toml;
      "src/lib.rs" = read ./fixtures/owner-native/lib.rs;
      "styles/site.css" = read ./fixtures/owner-native/before.css;
      "db/schema.sql" = read ./fixtures/owner-native/before.sql;
      ".nudox/sql.json" = read ./fixtures/owner-native/sql.json;
    };
    steps = [
      { id = "index-initial"; kind = "index"; }
      (css "class-card" "--class" "class" "card" "card")
      (cssRead "read-card" "card")
      (css "property-tone" "--custom-property" "custom_property" "--tone" "--tone")
      (cssRead "read-tone" "--tone")
      (sql "table-members" "definition" "members" "table members (created)")
      (sqlRead "read-members" "members")
      (sql "view-member-names" "definition" "member_names" "view member_names (created)")
      (sqlRead "read-member-names" "member_names")
      {
        id = "reject-css-handle"; kind = "cli"; args = [ "stylo-read-site" "invalid" ];
        expect = { exit = 64; json = [ (eq "/answer" "fault") (eq "/slug" "usage") (eq "/operand" "reference") ]; };
      }
      {
        id = "reject-sql-handle"; kind = "cli"; args = [ "sql-read-source" "invalid" ];
        expect = { exit = 64; json = [ (eq "/answer" "fault") (eq "/slug" "usage") (eq "/operand" "reference") ]; };
      }
      {
        id = "edit-both"; kind = "write";
        files = {
          "styles/site.css" = read ./fixtures/owner-native/after.css;
          "db/schema.sql" = read ./fixtures/owner-native/after.sql;
        };
      }
      { id = "reindex-edits"; kind = "index"; }
      (css "class-panel" "--class" "class" "panel" "panel")
      (cssRead "read-panel" "panel")
      (noCss "removed-card" "card")
      (css "property-shade" "--custom-property" "custom_property" "--shade" "--shade")
      (sql "table-persons" "definition" "persons" "table persons (created)")
      (sqlRead "read-persons" "persons")
      (noSql "removed-members" "members")
      (sql "view-person-names" "definition" "person_names" "view person_names (created)")
      { id = "cold-restart"; kind = "restart"; }
      (css "panel-after-restart" "--class" "class" "panel" "panel")
      (cssRead "panel-source-after-restart" "panel")
      (sql "persons-after-restart" "definition" "persons" "table persons (created)")
      (sqlRead "persons-source-after-restart" "persons")
    ];
  } ];
}
