# CLI grammar and MCP JSON-RPC laws through the installed product binaries.
# These requests require no pre-seeded project. MCP startup defers its owner
# connection until an owner-backed request; protocol failures remain correlated.
let
  check = pointer: equals: { inherit pointer equals; };
  cli = id: args: exit: stdout: stderr: {
    inherit id;
    language = "protocol";
    daemon = false;
    steps = [ {
      id = "invoke";
      kind = "process";
      binary = "backend-cli";
      inherit args;
      timeout_seconds = 15;
      expect = { inherit exit; stdout_contains = stdout; stderr_contains = stderr; };
    } ];
  };
  cliJson = id: args: exit: json: {
    inherit id;
    language = "protocol";
    daemon = false;
    steps = [ {
      id = "invoke";
      kind = "process";
      binary = "backend-cli";
      protocol = "json";
      inherit args;
      timeout_seconds = 15;
      expect = { inherit exit json; };
    } ];
  };
  # Include broken JSON, a successful initialize, its notification, a valid
  # tools/list, a malformed envelope, an unknown method, and another valid
  # tools/list. It proves parse/control failures do not poison the stdio stream.
  jsonLine = builtins.toJSON;
  mcpInput = builtins.concatStringsSep "\n" [
    "{not-json"
    (jsonLine {
      jsonrpc = "2.0"; id = 1; method = "initialize";
      params = {
        protocolVersion = "2025-06-18"; capabilities = { };
        clientInfo = { name = "keel-process-contract"; version = "1"; };
      };
    })
    (jsonLine { jsonrpc = "2.0"; method = "notifications/initialized"; })
    (jsonLine { jsonrpc = "2.0"; id = 2; method = "tools/list"; params = { }; })
    (jsonLine { jsonrpc = "1.0"; id = 4; method = "tools/list"; params = { }; })
    (jsonLine { jsonrpc = "2.0"; id = 5; method = "keel/unknown"; params = { }; })
    (jsonLine { jsonrpc = "2.0"; id = 6; method = "tools/list"; params = { }; })
  ] + "\n";
in {
  schema = 1;
  cases = [
    (cli "cli-root-help" [ "--help" ] 0
      [ "Usage: nudox [OPTIONS] <COMMAND> [OPERANDS]" "--limit COUNT" "library" "registry" "home" "session" "system" "surface <JSON>" "--format human|markdown|json" "--passive" "nudox add ." "nudox search \"error handling\"" "nudox mcp-config" "no config writes or service start" ] [ ])
    (cli "cli-add-help" [ "add" "--help" ] 0
      [ "nudox add" "use `.` for the current directory" "--execution-intent INTENT" "--cargo-target NAME" "bounded remote calibration" ] [ ])
    (cliJson "cli-add-unknown-execution-intent"
      [ "--format" "json" "--project" "\${project}" "add" "\${project}" "--execution-intent" "remote" ]
      64 [
        (check "/answer" "fault")
        (check "/slug" "usage")
        (check "/operand" "execution-intent")
        { pointer = "/detail"; contains = "choose interactive or background"; }
      ])
    (cli "cli-command-help-after-global-options"
      [ "--project" "/tmp/keel-probe" "search" "--help" ] 0
      [ "nudox search" "operands" ] [ ])
    (cli "cli-unknown-command" [ "not-a-command" ] 64 [ ]
      [ "✗ usage" "`not-a-command` is not a command" "nudox --help" ])
    (cli "cli-nearest-command" [ "serch" "ferris" ] 64 [ ]
      [ "✗ usage" "did you mean `search`" ])
    (cli "cli-document-alias-missing-operand" [ "document" ] 64 [ ]
      [ "usage: nudox document [COORDINATE] [--selected-id SELECTED_ID] [--cursor CURSOR]" ])
    (cli "cli-unpaged-bound" [ "packages" "--limit" "20" ] 64 [ ]
      [ "does not take a page bound" ])
    (cli "cli-unpaged-zero-bound" [ "packages" "--limit" "0" ] 64 [ ]
      [ "does not take a page bound" ])
    (cli "cli-unpaged-health-bound" [ "health" "--limit" "5" ] 64 [ ]
      [ "does not take a page bound" ])
    (cli "cli-passive-search-refused" [ "--passive" "search" "needle" ] 64 [ ]
      [ "--passive" "health" "status" ])
    (cliJson "cli-passive-status-connect-only"
      [ "--passive" "--format" "json" "--project" "\${project}" "--workspace" "\${project}/passive-status-workspace" "--endpoint" "\${project}/passive-status.sock" "status" ]
      1 [
        (check "/answer" "fault")
        (check "/slug" "endpoint")
        (check "/cause" "unreachable")
        { pointer = "/detail"; contains = "without starting it"; }
      ])
    (cliJson "cli-passive-health-connect-only"
      [ "--passive" "--format" "json" "--project" "\${project}" "--workspace" "\${project}/passive-health-workspace" "--endpoint" "\${project}/passive-health.sock" "health" ]
      1 [
        (check "/answer" "fault")
        (check "/slug" "endpoint")
        (check "/cause" "unreachable")
        { pointer = "/detail"; contains = "without starting it"; }
      ])
    (cli "cli-unknown-option" [ "search" "needle" "--not-an-option" ] 64 [ ]
      [ "✗ usage" "--not-an-option" "search takes only" "--limit" ])
    (cli "cli-missing-global-option-value" [ "--project" ] 64 [ ]
      [ "✗ usage" "--project needs a value after it" ])
    (cli "cli-missing-command-operand" [ "show" ] 64 [ ]
      [ "✗ usage" "usage: nudox show [COORDINATE]" ])
    (cli "cli-package-graph-help-bound" [ "package-graph" "--help" ] 0
      [ "nudox package-graph" "Range: 1 to 128; default 64." ] [ ])
    (cliJson "cli-package-graph-bound-129"
      [ "--format" "json" "--project" "\${project}" "package-graph" "pkg:cargo/serde@1.0.228" "dependencies" "--limit" "129" ]
      64 [
        (check "/answer" "fault")
        (check "/slug" "usage")
        (check "/operand" "limit")
        { pointer = "/detail"; contains = "129"; }
      ])
    (cli "cli-invalid-page-bound" [ "search" "needle" "--limit" "201" ] 64 [ ]
      [ "✗ usage" "`201` is not a page bound; choose a whole number from 1 to 200" ])
    (cli "cli-invalid-format" [ "--format" "jsonl" ] 64 [ ]
      [ "✗ usage" "`jsonl` is not a rendering; choose human, markdown, or json" ])
    {
      id = "mcp-stdio-error-recovery";
      language = "protocol";
      daemon = false;
      steps = [ {
        id = "handshake-and-recovery";
        kind = "process";
        binary = "backend-mcp";
        protocol = "jsonrpc-lines";
        stdin = mcpInput;
        timeout_seconds = 30;
        expect = {
          exit = 0;
          json = [
            (check "/null/error/code" (-32700))
            (check "/1/result/protocolVersion" "2025-06-18")
            { pointer = "/2/result/tools"; select = "/name"; contains = "backend.search"; }
            { pointer = "/2/result/tools"; select = "/name"; contains = "backend.status"; }
            (check "/4/error/code" (-32600))
            (check "/5/error/code" (-32601))
            { pointer = "/6/result/tools"; select = "/name"; contains = "backend.search"; }
          ];
        };
      } ];
    }
  ];
}
