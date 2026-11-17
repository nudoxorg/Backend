# The public CLI bound and MCP continuation path must agree on the same seven
# independently named symbols. Three rows per page requires three live pages.
let
  eq = pointer: equals: { inherit pointer equals; };
  names = [
    "KeelPageBeacon00"
    "KeelPageBeacon01"
    "KeelPageBeacon02"
    "KeelPageBeacon03"
    "KeelPageBeacon04"
    "KeelPageBeacon05"
    "KeelPageBeacon06"
  ];
  source = builtins.concatStringsSep "\n" (map (name: "pub fn ${name}() {}") names) + "\n";
in
{
  schema = 1;
  cases = [
    {
      id = "rust-public-search-page-bound";
      language = "rust";
      node = "engine.lower.rust";
      files = {
        "Cargo.toml" = ''
          [package]
          name = "keel-page-bound"
          version = "0.1.0"
          edition = "2021"
        '';
        "src/lib.rs" = source;
      };
      steps = [
        {
          id = "index-seven-page-beacons";
          kind = "index";
        }
        {
          id = "standard-exact-readiness-search";
          kind = "query";
          wait_for = true;
          cli = [
            "search"
            "KeelPageBeacon00"
            "--limit"
            "20"
          ];
          tool = "backend.search";
          arguments = {
            query = "KeelPageBeacon00";
            limit = 20;
          };
          expect = {
            exit = 0;
            json = [
              (eq "/records/0/identity/name" "KeelPageBeacon00")
              (eq "/records/0/identity/path" "src/lib.rs")
              {
                pointer = "/records/1";
                absent = true;
              }
              (eq "/more" false)
            ];
          };
        }
        {
          id = "cli-global-limit-three";
          kind = "cli";
          args = [
            "--limit"
            "3"
            "search"
            "KeelPageBeacon"
            "--detail"
            "standard"
          ];
          bind = {
            bounded_records = "/records";
          };
          expect = {
            exit = 0;
            json = [
              {
                pointer = "/records/0/identity/name";
                contains = "KeelPageBeacon";
              }
              {
                pointer = "/records/1/identity/name";
                contains = "KeelPageBeacon";
              }
              {
                pointer = "/records/2/identity/name";
                contains = "KeelPageBeacon";
              }
              {
                pointer = "/records/3";
                absent = true;
              }
              (eq "/more" true)
            ];
          };
        }
        {
          id = "mcp-continuations-at-three";
          kind = "pages";
          tool = "backend.search";
          arguments = {
            query = "KeelPageBeacon";
            limit = 3;
            detail = "standard";
          };
          minimum_pages = 3;
          maximum_pages = 3;
          expect = {
            exit = 0;
            json = [
              {
                pointer = "/records";
                select = "/identity/name";
                set_equals = names;
              }
              (eq "/first/records" "\${bounded_records}")
              (eq "/first/continuation_call/arguments/limit" 3)
              (eq "/first/more" true)
              (eq "/last/more" false)
            ];
          };
        }
      ];
    }
  ];
}
