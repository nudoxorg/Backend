# Keep the empty-project deletion case intact. This independent control proves
# deleting one of two sources retires only that source's declaration evidence.
let
  eq = pointer: equals: { inherit pointer equals; };
  find = id: name: path: {
    inherit id; kind = "query";
    cli = [ "search" name "--limit" "20" ];
    tool = "backend.search"; arguments = { query = name; limit = 20; };
    expect = { exit = 0; json = [
      (eq "/records/0/identity/name" name)
      (eq "/records/0/identity/path" path)
      { pointer = "/records/1"; absent = true; }
    ]; };
  };
  absent = id: {
    inherit id; kind = "query";
    cli = [ "search" "keelRemovedWitness" "--limit" "20" ];
    tool = "backend.search"; arguments = { query = "keelRemovedWitness"; limit = 20; };
    expect = { exit = 0; json = [ (eq "/records" []) (eq "/more" false) ]; };
  };
in {
  schema = 1;
  cases = [ {
    id = "javascript-partial-deletion-retains-source";
    language = "javascript"; node = "engine.lower.typescript";
    files = {
      "package.json" = builtins.readFile ./fixtures/javascript/package.json;
      "src/deleted.js" = "export function keelRemovedWitness() { return 1; }\n";
      "src/retained.js" = "export function keelRetainedWitness() { return 2; }\n";
    };
    steps = [
      { id = "index-two-sources"; kind = "index"; }
      (find "find-retained" "keelRetainedWitness" "src/retained.js")
      (find "find-before-delete" "keelRemovedWitness" "src/deleted.js")
      { id = "delete-one-source"; kind = "write"; remove = [ "src/deleted.js" ]; }
      { id = "reindex-retained-source"; kind = "index"; }
      (find "retained-after-delete" "keelRetainedWitness" "src/retained.js")
      (absent "deleted-evidence-absent")
      { id = "restart-owner"; kind = "restart"; }
      (find "retained-after-restart" "keelRetainedWitness" "src/retained.js")
      (absent "deleted-evidence-absent-after-restart")
    ];
  } ];
}
