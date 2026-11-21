# Independent public process case for TypeScript's cross-file symbol join.
# The compiler can name Math.max from its ambient library, but only project
# source should carry a captured source note or produce a source page.
let
  eq = pointer: equals: { inherit pointer equals; };
  tsconfig = ''
    {"compilerOptions":{"strict":true,"target":"ES2022","lib":["ES2022"]},"include":["*.ts"]}
  '';
  helper = ''
    export function selectedHelper(value:number):number{return value+1;}
  '';
  view = ''
    import { selectedHelper } from "./helper";
    export function run(value:number):number{return Math.max(selectedHelper(value),0);}
  '';
  note = "Source was observed at capture; discovery policy stability is unknown.";
in
{
  schema = 1;
  cases = [
    {
      id = "typescript-cross-file-and-ambient-source";
      language = "typescript";
      node = "engine.lower.typescript";
      files = {
        "tsconfig.json" = tsconfig;
        "helper.ts" = helper;
        "view.ts" = view;
      };
      steps = [
        {
          id = "index-cross-file-typescript";
          kind = "index";
          timeout_seconds = 300;
        }
        {
          id = "resolve-cross-file-helper";
          kind = "query";
          cli = [
            "resolve"
            "selectedHelper"
            "--limit"
            "20"
          ];
          tool = "backend.resolve";
          arguments = {
            query = "selectedHelper";
            limit = 20;
          };
          bind = {
            helper_coordinate = "/records/0/identity/coordinate";
            helper_id = {
              pointer = "/records/0/identity/semantic_data";
              transform = "selected-symbol-key";
            };
          };
          expect = {
            exit = 0;
            json = [
              (eq "/records/0/identity/name" "selectedHelper")
              (eq "/records/0/identity/path" "helper.ts")
              (eq "/records/0/summary" note)
              {
                pointer = "/records/1";
                absent = true;
              }
              (eq "/more" false)
            ];
          };
        }
        {
          id = "search-cross-file-helper";
          kind = "query";
          wait_for = true;
          cli = [
            "search"
            "selectedHelper"
            "--limit"
            "20"
          ];
          tool = "backend.search";
          arguments = {
            query = "selectedHelper";
            limit = 20;
          };
          expect = {
            exit = 0;
            json = [
              {
                pointer = "/records";
                select = "/identity/path";
                set_equals = [
                  "helper.ts"
                  "view.ts"
                ];
              }
              {
                pointer = "/records";
                select = "/kind";
                set_equals = [
                  "function"
                  "import"
                ];
              }
              {
                pointer = "/records";
                select = "/identity/shape";
                set_equals = [
                  "semantic"
                  "semantic"
                ];
              }
              {
                pointer = "/records/2";
                absent = true;
              }
              {
                pointer = "/full_search";
                absent = true;
              }
              {
                pointer = "/search_scope";
                absent = true;
              }
              (eq "/more" false)
            ];
          };
        }
        {
          id = "show-cross-file-helper";
          kind = "query";
          cli = [
            "show"
            "\${helper_coordinate}"
          ];
          tool = "backend.document";
          arguments.coordinate = "\${helper_coordinate}";
          expect = {
            exit = 0;
            json = [
              (eq "/answer" "page")
              (eq "/identity/name" "selectedHelper")
              (eq "/source/path" "helper.ts")
              (eq "/prose" [ note ])
            ];
          };
        }
        {
          id = "resolve-ambient-max";
          kind = "query";
          cli = [
            "resolve"
            "max"
            "--limit"
            "20"
          ];
          tool = "backend.resolve";
          arguments = {
            query = "max";
            limit = 20;
          };
          bind = {
            max_coordinate = "/records/0/identity/coordinate";
          };
          expect = {
            exit = 0;
            json = [
              (eq "/records/0/identity/name" "max")
              (eq "/records/0/identity/shape" "external")
              {
                pointer = "/records/0/identity/path";
                absent = true;
              }
              {
                pointer = "/records/0/summary";
                absent = true;
              }
              {
                pointer = "/records/1";
                absent = true;
              }
              (eq "/more" false)
            ];
          };
        }
        {
          id = "show-ambient-max-without-source";
          kind = "query";
          cli = [
            "show"
            "\${max_coordinate}"
          ];
          tool = "backend.document";
          arguments.coordinate = "\${max_coordinate}";
          expect = {
            exit = 2;
            json = [
              (eq "/answer" "page")
              (eq "/identity/name" "max")
              (eq "/identity/shape" "external")
              (eq "/source_fault/slug" "source-unavailable")
              (eq "/source_fault/cause" "not-captured")
              {
                pointer = "/source";
                absent = true;
              }
              {
                pointer = "/prose";
                absent = true;
              }
            ];
          };
        }
        {
          id = "resolve-cross-file-caller";
          kind = "query";
          cli = [
            "resolve"
            "run"
            "--limit"
            "20"
          ];
          tool = "backend.resolve";
          arguments = {
            query = "run";
            limit = 20;
          };
          bind = {
            run_coordinate = "/records/0/identity/coordinate";
            run_id = {
              pointer = "/records/0/identity/semantic_data";
              transform = "selected-symbol-key";
            };
          };
          expect = {
            exit = 0;
            json = [
              (eq "/records/0/identity/name" "run")
              (eq "/records/0/identity/path" "view.ts")
              {
                pointer = "/records/1";
                absent = true;
              }
            ];
          };
        }
        {
          id = "one-cross-file-call-edge";
          kind = "cli";
          args = [
            "related"
            "\${helper_coordinate}"
          ];
          expect = {
            exit = 0;
            json = [
              {
                pointer = "/records";
                select = "/identity/name";
                contains = "run";
              }
              {
                pointer = "/records";
                select = "/identity/name";
                contains = "selectedHelper";
              }
              {
                pointer = "/records";
                select = "/identity/shape";
                not_contains = "external";
              }
            ];
          };
        }
        {
          id = "start-cross-file-reader";
          kind = "mcp-start";
          session = "cross-file";
        }
        {
          id = "typed-cross-file-call-target";
          kind = "mcp-call";
          session = "cross-file";
          tool = "backend.related";
          arguments = {
            coordinate = "\${helper_coordinate}";
            limit = 20;
          };
          expect = {
            exit = 0;
            json = [
              (eq "/related_basis/center_id" "\${helper_id}")
              (eq "/more" false)
              (eq "/related_evidence_withheld" false)
              {
                pointer = "/edges";
                contains = {
                  from = "\${run_coordinate}";
                  from_id = "\${run_id}";
                  to = "\${helper_coordinate}";
                  to_id = "\${helper_id}";
                  relation = "calls";
                  confidence = "compiler";
                };
              }
            ];
          };
        }
        {
          id = "stop-cross-file-reader";
          kind = "mcp-stop";
          session = "cross-file";
        }
        {
          id = "restart-cross-file-project";
          kind = "restart";
        }
        {
          id = "resolve-helper-after-restart";
          kind = "query";
          cli = [
            "resolve"
            "selectedHelper"
            "--limit"
            "20"
          ];
          tool = "backend.resolve";
          arguments = {
            query = "selectedHelper";
            limit = 20;
          };
          expect = {
            exit = 0;
            json = [
              (eq "/records/0/identity/name" "selectedHelper")
              (eq "/records/0/identity/path" "helper.ts")
              (eq "/records/0/summary" note)
              {
                pointer = "/records/1";
                absent = true;
              }
              (eq "/more" false)
            ];
          };
        }
      ];
    }
  ];
}
