# Public source continuation law: independently authored UTF-8/CRLF bytes,
# including a host offset beyond the first 4 KiB. The scoped native extent,
# restart and stale-source refusal have identified public-driver receipts.
let
  eq = pointer: equals: { inherit pointer equals; };
  crlf = "\r\n";
  padding = builtins.concatStringsSep "" (
    builtins.genList (n: "function padding_${toString n}() { return ${toString n}; }${crlf}") 400
  );
  body =
    "function ferris() {${crlf}"
    + builtins.concatStringsSep "" (
      builtins.genList (n: "  const value_${toString n} = \"é漢🦀\";${crlf}") 200
    )
    + "${crlf}}";
  host = "// prefix${crlf}${padding}${body}${crlf}";
  edited = builtins.replaceStrings [ "const value_" ] [ "const other_" ] host;
  tsconfig = builtins.toJSON {
    compilerOptions = {
      strict = true;
      target = "ES2022";
      lib = [ "ES2022" ];
    };
    include = [ "*.ts" ];
  };
in
{
  schema = 1;
  cases = [
    {
      id = "typescript-source-range-restart";
      language = "typescript";
      node = "engine.lower.typescript";
      files = {
        "tsconfig.json" = tsconfig;
        "entry.ts" = host;
      };
      steps = [
        {
          id = "index";
          kind = "index";
          timeout_seconds = 300;
        }
        {
          id = "resolve-ferris";
          kind = "query";
          cli = [
            "resolve"
            "ferris"
            "--limit"
            "100"
          ];
          tool = "backend.resolve";
          arguments = {
            query = "ferris";
            limit = 100;
          };
          bind = {
            coordinate = "/records/0/identity/coordinate";
            selected = "/records/0/identity/semantic_data";
          };
          expect = {
            exit = 0;
            json = [
              (eq "/records/0/identity/name" "ferris")
              (eq "/records/0/identity/path" "entry.ts")
              (eq "/records/0/identity/semantic_data/kind" "selected-symbol-id")
              {
                pointer = "/records/1";
                absent = true;
              }
            ];
          };
        }
        {
          id = "source-pages";
          kind = "pages";
          page_format = "source-range";
          tool = "backend.source";
          arguments = {
            coordinate = "\${coordinate}";
            selected_symbol = "\${selected}";
            byte_range = true;
          };
          cli = [
            "source"
            "\${coordinate}"
            "--byte-range"
          ];
          minimum_pages = 2;
          maximum_pages = 32;
          timeout_seconds = 300;
          expect = {
            exit = 0;
            json = [
              (eq "/text" body)
              (eq "/scope" "native_primary_declaration")
              (eq "/owner_start" (builtins.stringLength ("// prefix${crlf}" + padding)))
            ];
          };
        }
        {
          id = "start-mcp";
          kind = "mcp-start";
          session = "source";
        }
        {
          id = "first-page";
          kind = "mcp-call";
          session = "source";
          tool = "backend.source";
          arguments = {
            coordinate = "\${coordinate}";
            selected_symbol = "\${selected}";
            byte_range = true;
          };
          bind = {
            continuation = "/continuation_call";
            first_end = "/source/end";
          };
          expect = {
            exit = 0;
            json = [
              (eq "/source/start" 0)
              (eq "/source/scope" "native_primary_declaration")
              (eq "/continuation_call/name" "backend.source")
              {
                pointer = "/nextCursor";
                contains = "mcp2-";
              }
            ];
          };
        }
        {
          id = "stop-mcp";
          kind = "mcp-stop";
          session = "source";
        }
        {
          id = "restart-owner";
          kind = "restart";
        }
        {
          id = "restart-mcp";
          kind = "mcp-start";
          session = "source";
        }
        {
          id = "resume-after-restart";
          kind = "mcp-call";
          session = "source";
          call = "\${continuation}";
          expect = {
            exit = 0;
            json = [
              (eq "/source/start" "\${first_end}")
              (eq "/source/scope" "native_primary_declaration")
            ];
          };
        }
        {
          id = "same-size-edit";
          kind = "write";
          files."entry.ts" = edited;
        }
        {
          id = "reject-stale-source";
          kind = "mcp-call";
          session = "source";
          call = "\${continuation}";
          mcp_error = true;
          expect = {
            exit = 0;
            json = [
              (eq "/answer" "fault")
              (eq "/slug" "invalid-query")
              (eq "/operand" "source")
              (eq "/cause" "malformed")
              {
                pointer = "/detail";
                contains = "indexed source changed on disk; reindex and retry";
              }
              {
                pointer = "/source";
                absent = true;
              }
            ];
          };
        }
        {
          id = "close-mcp";
          kind = "mcp-stop";
          session = "source";
        }
      ];
    }
  ];
}
