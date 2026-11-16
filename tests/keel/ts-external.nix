# Public counterpart of the native TypeScript source-availability regression.
# The selected external row is compiler evidence for Math.max, not a claim that
# the standard library's source was captured in this project.
let
  read = path: builtins.readFile path;
  note = "Source was observed at capture; discovery policy stability is unknown.";
  eq = pointer: equals: { inherit pointer equals; };
in {
  schema = 1;
  cases = [ {
    id = "typescript-external-source-policy";
    language = "typescript";
    node = "engine.lower.typescript";
    files = {
      "tsconfig.json" = read ./fixtures/ts-external/tsconfig.json;
      "view.ts" = read ./fixtures/ts-external/view.ts;
    };
    steps = [
      { id = "index-typescript"; kind = "index"; timeout_seconds = 300; }
      {
        id = "resolve-local"; kind = "query";
        cli = [ "resolve" "local" "--limit" "100" ];
        tool = "backend.resolve"; arguments = { query = "local"; limit = 100; };
        bind.local_coordinate = "/records/0/identity/coordinate";
        expect = { exit = 0; json = [
          (eq "/records/0/identity/name" "local")
          (eq "/records/0/identity/path" "view.ts")
          (eq "/records/0/summary" note)
          (eq "/more" false)
        ]; };
      }
      {
        id = "show-local"; kind = "query";
        cli = [ "show" "\${local_coordinate}" ];
        tool = "backend.document"; arguments.coordinate = "\${local_coordinate}";
        expect = { exit = 0; json = [
          (eq "/answer" "page")
          (eq "/identity/name" "local")
          (eq "/source/path" "view.ts")
          (eq "/prose" [ note ])
        ]; };
      }
      {
        id = "resolve-external-max"; kind = "query";
        cli = [ "resolve" "max" "--limit" "100" ];
        tool = "backend.resolve"; arguments = { query = "max"; limit = 100; };
        bind.external_coordinate = "/records/0/identity/coordinate";
        expect = { exit = 0; json = [
          (eq "/records/0/identity/name" "max")
          (eq "/records/0/identity/shape" "external")
          { pointer = "/records/0/identity/path"; absent = true; }
          { pointer = "/records/0/summary"; absent = true; }
          { pointer = "/records/1"; absent = true; }
          (eq "/more" false)
        ]; };
      }
      {
        id = "show-external-without-source"; kind = "query";
        cli = [ "show" "\${external_coordinate}" ];
        tool = "backend.document"; arguments.coordinate = "\${external_coordinate}";
        # The page preserves an unavailable related section; CLI reports 2.
        expect = { exit = 2; json = [
          (eq "/answer" "page")
          (eq "/identity/name" "max")
          (eq "/identity/shape" "external")
          (eq "/source_fault/slug" "source-unavailable")
          (eq "/source_fault/cause" "not-captured")
          { pointer = "/source"; absent = true; }
          { pointer = "/prose"; absent = true; }
        ]; };
      }
    ];
  } ];
}
