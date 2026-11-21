# Candidate public stdio obligations. This table is deliberately unregistered:
# it must run against a source-bound real backend-mcp before release admission.
# Every step starts an installed product process; no product response is mocked.
let
  eq = pointer: equals: { inherit pointer equals; };
  absent = pointer: {
    inherit pointer;
    absent = true;
  };
  contains = pointer: value: {
    inherit pointer;
    contains = value;
  };
  selected = pointer: field: value: {
    inherit pointer;
    select = field;
    contains = value;
  };
  line = builtins.toJSON;
  request = id: method: params: {
    jsonrpc = "2.0";
    inherit id method params;
  };
  notification = method: params: {
    jsonrpc = "2.0";
    inherit method params;
  };
  initialize =
    id: version:
    request id "initialize" {
      protocolVersion = version;
      capabilities = { };
      clientInfo = {
        name = "keel-stdio-law";
        version = "1";
      };
    };
  acknowledge = notification "notifications/initialized" { };
  ready = [
    (initialize 1 "2025-06-18")
    acknowledge
  ];
  lines = frames: builtins.concatStringsSep "\n" (map line frames) + "\n";
  process = id: stdin: json: {
    inherit id stdin;
    kind = "process";
    binary = "backend-mcp";
    protocol = "jsonrpc-lines";
    timeout_seconds = 30;
    expect = {
      exit = 0;
      inherit json;
    };
  };
  case = id: steps: {
    inherit id steps;
    language = "protocol";
    daemon = false;
  };
  versionStep =
    id: offered: expected:
    process id
      (lines [
        (initialize 1 offered)
        acknowledge
        (request 2 "ping" { })
      ])
      [
        (eq "/1/jsonrpc" "2.0")
        (eq "/1/id" 1)
        (eq "/1/result/protocolVersion" expected)
        (eq "/2/result/_meta/backend~1wireBudget/limitBytes" 49152)
        (eq "/2/result/_meta/backend~1wireBudget/bytesPerToken" 4)
        (absent "/2/error")
        (absent "/null")
      ];
  deepValue = builtins.foldl' (value: _: [ value ]) "leaf" (builtins.genList (x: x) 40);
in
{
  schema = 1;
  cases = [
    # An early initialized notification is not permission to skip initialize;
    # both the initialize reply and its acknowledgement are required.
    (case "mcp-stdio-handshake-state" [
      (process "unsolicited-ack-is-silent" (lines [ (notification "notifications/initialized" { }) ]) [
        (eq "" { })
      ])
      (process "state-transitions"
        (lines [
          (notification "notifications/initialized" { })
          (request 8 "resources/read" { uri = "backend://workspace/current"; })
          (request 2 "resources/list" { })
          (initialize 3 "2025-06-18")
          (request 4 "tools/list" { })
          acknowledge
          (request 5 "ping" { })
          (initialize 6 "2025-06-18")
          (request 7 "ping" { })
        ])
        [
          (eq "/8/error/code" (-32002))
          (absent "/8/result")
          (eq "/2/error/code" (-32002))
          (absent "/2/result")
          (eq "/3/result/protocolVersion" "2025-06-18")
          (eq "/3/result/serverInfo/name" "backend")
          (eq "/3/result/capabilities/tools/listChanged" false)
          (eq "/3/result/capabilities/resources/subscribe" false)
          (eq "/3/result/capabilities/prompts/listChanged" false)
          (eq "/4/error/code" (-32002))
          (eq "/5/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/5/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/5/error")
          (eq "/6/error/code" (-32600))
          (eq "/7/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/7/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/7/error")
          (absent "/null")
        ]
      )
    ])
    # The negotiated response is a server offer, not a reflection of arbitrary
    # client input. Each offer starts a fresh actual server process.
    (case "mcp-stdio-version-negotiation" [
      (versionStep "current" "2025-06-18" "2025-06-18")
      (versionStep "previous" "2025-03-26" "2025-03-26")
      (versionStep "newer" "2025-11-25" "2025-06-18")
      (versionStep "newer-2026" "2026-07-28" "2025-06-18")
      (versionStep "unknown" "2099-01-01" "2025-06-18")
    ])
    (case "mcp-stdio-init-refusal-retry" [
      (process "invalid-then-valid"
        (lines [
          (request 1 "initialize" [ ])
          (request 2 "ping" { })
          (request 3 "initialize" {
            protocolVersion = "2025-06-18";
            capabilities = { };
          })
          (initialize 4 "2025-06-18")
          acknowledge
          (request 5 "ping" { })
        ])
        [
          (eq "/1/error/code" (-32602))
          (eq "/2/error/code" (-32002))
          (eq "/3/error/code" (-32602))
          (eq "/4/result/protocolVersion" "2025-06-18")
          (eq "/5/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/5/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/5/error")
          (absent "/null")
        ]
      )
    ])
    # Discovery is available without an owner. Invalid cursors and malformed
    # tool calls are correlated protocol faults; subsequent discovery works.
    (case "mcp-stdio-discovery-envelope" [
      (process "discover-and-recover"
        (lines (
          ready
          ++ [
            (request 2 "resources/list" { })
            (request 3 "resources/templates/list" { })
            (request 4 "prompts/list" { })
            (request 5 "tools/list" { cursor = "foreign-cursor"; })
            (request 6 "resources/list" { cursor = "foreign-cursor"; })
            (request 7 "prompts/list" { cursor = "foreign-cursor"; })
            (request 8 "tools/call" {
              name = "backend.not-a-tool";
              arguments = { };
            })
            (request 9 "tools/call" {
              name = "backend.index";
              arguments = [ ];
            })
            (request 10 "prompts/get" { name = "not-a-prompt"; })
            (request 11 "resources/list" { })
          ]
        ))
        [
          (selected "/2/result/resources" "/uri" "backend://workspace/current")
          (selected "/2/result/resources" "/uri" "backend://schema/query")
          (selected "/3/result/resourceTemplates" "/uriTemplate" "backend://document/{coordinate}")
          (selected "/4/result/prompts" "/name" "backend.explore")
          (eq "/5/error/code" (-32602))
          (eq "/6/error/code" (-32602))
          (eq "/7/error/code" (-32602))
          (eq "/8/error/code" (-32602))
          (eq "/9/error/code" (-32602))
          (eq "/10/error/code" (-32602))
          (absent "/8/result")
          (selected "/11/result/resources" "/uri" "backend://workspace/current")
          (absent "/null")
        ]
      )
    ])
    # CRLF is a complete frame and EOF after a final non-newline frame must
    # still drain both correlated replies. Notifications never get replies.
    (case "mcp-stdio-line-endings-notifications" [
      (process "crlf-and-eof"
        (builtins.concatStringsSep "\r\n" (
          map line (
            ready
            ++ [
              (notification "notifications/cancelled" {
                requestId = 999;
                reason = "no longer needed";
              })
              (notification "notifications/unknown" { })
              (request 2 "ping" { })
              (request 3 "ping" { })
            ]
          )
        ))
        [
          (eq "/1/result/protocolVersion" "2025-06-18")
          (eq "/2/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/2/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/2/error")
          (eq "/3/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/3/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/3/error")
          (absent "/null")
        ]
      )
    ])
    # Request identifiers retain their JSON type and exact value, including a
    # string containing a JSON-pointer separator and a negative numeric id.
    (case "mcp-stdio-correlation" [
      (process "correlate-id-types"
        (lines (
          ready
          ++ [
            (request 0 "ping" { })
            (request (-7) "ping" { })
            (request "caller/a" "ping" { })
            (request "missing-route" "not-a-method" { })
          ]
        ))
        [
          (eq "/0/id" 0)
          (eq "/0/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/0/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/0/error")
          (eq "/-7/id" (-7))
          (eq "/-7/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/-7/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/-7/error")
          (eq ''/"caller~1a"/id'' "caller/a")
          (eq ''/"caller~1a"/result/_meta/backend~1wireBudget/limitBytes'' 49152)
          (eq ''/"caller~1a"/result/_meta/backend~1wireBudget/bytesPerToken'' 4)
          (absent ''/"caller~1a"/error'')
          (eq ''/"missing-route"/id'' "missing-route")
          (eq ''/"missing-route"/error/code'' (-32601))
          (absent "/null")
        ]
      )
    ])
    # Depth is rejected before an owner-backed tool could be admitted; the
    # next ordinary request demonstrates that the stream remains usable.
    (case "mcp-stdio-depth-recovery" [
      (process "reject-nesting"
        (lines (
          ready
          ++ [
            (request 2 "tools/call" {
              name = "backend.query";
              arguments = {
                nested = deepValue;
              };
            })
            (request 3 "ping" { })
          ]
        ))
        [
          (eq "/null/error/code" (-32600))
          (eq "/3/result/_meta/backend~1wireBudget/limitBytes" 49152)
          (eq "/3/result/_meta/backend~1wireBudget/bytesPerToken" 4)
          (absent "/3/error")
        ]
      )
    ])
  ];
}
