# Independently authored source and expected facts. The runner supplies isolated
# project/workspace paths and performs each index against the real product.
let
  read = path: builtins.readFile path;
  eq = pointer: equals: { inherit pointer equals; };
  search = id: name: {
    inherit id;
    kind = "query";
    cli = [ "search" name "--limit" "20" ];
    tool = "backend.search";
    arguments = { query = name; limit = 20; };
    expect = { exit = 0; json = [ (eq "/records/0/identity/name" name) ]; };
  };
  bindSearch = id: name: (search id name) // {
    bind = { coordinate = "/records/0/identity/coordinate"; };
  };
  show = id: name: path: {
    inherit id;
    kind = "cli";
    args = [ "show" "\${coordinate}" ];
    expect = { exit = 0; json = [
      (eq "/answer" "page")
      (eq "/identity/name" name)
      (eq "/identity/path" path)
    ]; };
  };
  related = id: caller: decoy: {
    inherit id;
    kind = "cli";
    args = [ "related" "\${coordinate}" ];
    expect = { exit = 0; json = [
      { pointer = "/records"; select = "/identity/name"; contains = caller; }
      { pointer = "/records"; select = "/identity/name"; not_contains = decoy; }
    ]; };
  };
  index = id: { inherit id; kind = "index"; };
  restart = id: { inherit id; kind = "restart"; };
in {
  schema = 1;
  cases = [
    {
      id = "rust-call-history";
      language = "rust";
      node = "engine.lower.rust";
      files = {
        "Cargo.toml" = read ./fixtures/rust/Cargo.toml;
        "src/lib.rs" = read ./fixtures/rust/before.rs;
      };
      steps = [
        (index "initial-index")
        (bindSearch "find-leaf" "keel_rust_leaf")
        (show "show-leaf" "keel_rust_leaf" "src/lib.rs")
        (related "initial-caller" "keel_rust_middle" "keel_rust_decoy")
        { id = "edit"; kind = "write"; files."src/lib.rs" = read ./fixtures/rust/after.rs; }
        (index "reindex-edit")
        (search "find-added" "keel_rust_added")
        (bindSearch "find-revised-leaf" "keel_rust_leaf")
        (related "added-caller" "keel_rust_added" "keel_rust_decoy")
        (restart "cold-restart")
        (search "added-survives-restart" "keel_rust_added")
      ];
    }
    {
      id = "python-call-history";
      language = "python";
      node = "engine.lower.python";
      files = {
        "pyproject.toml" = read ./fixtures/python/pyproject.toml;
        "src/history.py" = read ./fixtures/python/before.py;
      };
      steps = [
        (index "initial-index")
        (bindSearch "find-leaf" "keel_python_leaf")
        (show "show-leaf" "keel_python_leaf" "src/history.py")
        (related "initial-caller" "keel_python_entry" "keel_python_decoy")
        { id = "edit"; kind = "write"; files."src/history.py" = read ./fixtures/python/after.py; }
        (index "reindex-edit")
        (search "find-added" "keel_python_added")
        (bindSearch "find-revised-leaf" "keel_python_leaf")
        (related "added-caller" "keel_python_added" "keel_python_decoy")
        (restart "cold-restart")
        (search "added-survives-restart" "keel_python_added")
      ];
    }
    {
      id = "javascript-file-history";
      language = "javascript";
      node = "engine.lower.typescript";
      files = {
        "package.json" = read ./fixtures/javascript/package.json;
        "src/history.js" = read ./fixtures/javascript/before.js;
      };
      steps = [
        (index "initial-index")
        (bindSearch "find-leaf" "keelJavaScriptLeaf")
        (show "show-leaf" "keelJavaScriptLeaf" "src/history.js")
        (related "initial-caller" "keelJavaScriptEntry" "keelJavaScriptDecoy")
        { id = "edit"; kind = "write"; files."src/history.js" = read ./fixtures/javascript/after.js; }
        (index "reindex-edit")
        (search "find-added" "keelJavaScriptAdded")
        (bindSearch "find-revised-leaf" "keelJavaScriptLeaf")
        (related "added-caller" "keelJavaScriptAdded" "keelJavaScriptDecoy")
        { id = "delete-source"; kind = "write"; remove = [ "src/history.js" ]; }
        (index "reindex-deletion")
        {
          id = "deleted-is-absent"; kind = "query";
          cli = [ "search" "keelJavaScriptAdded" "--limit" "20" ];
          tool = "backend.search"; arguments = { query = "keelJavaScriptAdded"; limit = 20; };
          expect = { exit = 0; json = [ (eq "/records" [ ]) ]; };
        }
        { id = "readd-source"; kind = "write"; files."src/history.js" = read ./fixtures/javascript/after.js; }
        (index "reindex-readd")
        (restart "cold-restart")
        (search "readded-survives-restart" "keelJavaScriptAdded")
      ];
    }
  ];
}
