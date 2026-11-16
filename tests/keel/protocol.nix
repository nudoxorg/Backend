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
      [ "Usage: backend [OPTIONS] <COMMAND> [OPERANDS]" "--limit COUNT" ] [ ])
    (cli "cli-command-help-after-global-options"
      [ "--project" "/tmp/keel-probe" "search" "--help" ] 0
      [ "backend search" "operands" ] [ ])
    (cli "cli-unknown-command" [ "not-a-command" ] 64 [ ]
      [ "✗ usage" "`not-a-command` is not a command" "backend --help" ])
    (cli "cli-unknown-option" [ "search" "needle" "--not-an-option" ] 64 [ ]
      [ "✗ usage" "`--not-an-option`" "search takes only" ])
    (cli "cli-missing-global-option-value" [ "--project" ] 64 [ ]
      [ "✗ usage" "--project needs a value after it" ])
    (cli "cli-missing-command-operand" [ "show" ] 64 [ ]
      [ "✗ usage" "usage: backend show" ])
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
