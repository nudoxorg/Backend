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
  callerIdentity = suffix: {
    id = "caller-identity-${suffix}"; kind = "query";
    cli = [ "search" "keel_fanout_${suffix}" "--limit" "20" ];
    tool = "backend.search"; arguments = { query = "keel_fanout_${suffix}"; limit = 20; };
    bind."caller_${suffix}" = { pointer = "/records/0/identity/semantic_data"; transform = "selected-symbol-key"; };
    expect = { exit = 0; json = [ (eq "/records/0/identity/name" "keel_fanout_${suffix}") ]; };
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
  unknown = {
    id = "unknown-coordinate"; kind = "cli";
    args = [ "show" "/keel-regression-absent::src/lib.rs:999::nothing" ];
    expect = { exit = 2; json = [ (eq "/answer" "fault") (eq "/slug" "not-found") ]; };
  };
  detached = id: retained: removed: {
    inherit id; kind = "cli"; args = [ "related" "\${coordinate}" ];
    expect = { exit = 0; json = [
      { pointer = "/records"; select = "/identity/name"; contains = retained; }
      { pointer = "/records"; select = "/identity/name"; not_contains = removed; }
    ]; };
  };
  index = id: { inherit id; kind = "index"; };
  restart = id: { inherit id; kind = "restart"; };
in {
  schema = 1;
  cases = [
    {
      id = "rust-fanout-and-diamond";
      language = "rust";
      node = "engine.lower.rust";
      files = {
        "Cargo.toml" = read ./fixtures/rust/Cargo.toml;
        "src/lib.rs" = read ./fixtures/rust/fanout.rs;
      };
      steps = [
        (index "index-graph")
        ((bindSearch "find-target" "keel_fanout_target") // { bind = {
          coordinate = "/records/0/identity/coordinate";
          center_id = { pointer = "/records/0/identity/semantic_data"; transform = "selected-symbol-key"; };
        }; })
        (callerIdentity "a") (callerIdentity "b") (callerIdentity "c") (callerIdentity "d")
        (callerIdentity "e") (callerIdentity "f") (callerIdentity "g") (callerIdentity "h")
        {
          id = "pin-related-source-root"; kind = "cli"; args = [ "status" ];
          expect = { exit = 0; json = [ (eq "/answer" "status") ]; };
          bind.source_root = { pointer = "/revision"; transform = "hex-root"; };
        }
        {
          id = "typed-related-call-edges"; kind = "pages"; tool = "backend.related";
          arguments = { coordinate = "\${coordinate}"; limit = 20; };
          minimum_pages = 1; maximum_pages = 16;
          expect = { exit = 0; json = [
            (eq "/first/related_basis/schema" 1)
            (eq "/first/related_basis/center_id" "\${center_id}")
            (eq "/first/related_basis/coordinate" "\${coordinate}")
            (eq "/first/related_basis/source_view_root" "\${source_root}")
            (eq "/first/related_basis/relation_coverage" "unknown")
            { pointer = "/first/edges"; select = "/from_id"; set_equals = [
              "\${caller_a}" "\${caller_b}" "\${caller_c}" "\${caller_d}"
              "\${caller_e}" "\${caller_f}" "\${caller_g}" "\${caller_h}"
            ]; }
            { pointer = "/first/edges"; select = "/to_id"; set_equals = builtins.genList (_: "\${center_id}") 8; }
          ]; };
        }
        {
          id = "eight-callers"; kind = "cli"; args = [ "related" "\${coordinate}" ];
          expect = { exit = 0; json = [
            { pointer = "/records"; select = "/identity/name"; set_equals = [
              "keel_fanout_target" "keel_fanout_a" "keel_fanout_b" "keel_fanout_c"
              "keel_fanout_d" "keel_fanout_e" "keel_fanout_f" "keel_fanout_g" "keel_fanout_h"
            ]; }
            { pointer = "/records"; select = "/identity/name"; not_contains = "keel_fanout_decoy"; }
          ]; };
        }
        (bindSearch "find-diamond-junction" "keel_fanout_a")
        {
          id = "diamond-incoming"; kind = "cli"; args = [ "related" "\${coordinate}" ];
          expect = { exit = 0; json = [
            { pointer = "/records"; select = "/identity/name"; set_equals = [
              "keel_fanout_a" "keel_fanout_target" "keel_diamond_left" "keel_diamond_right"
            ]; }
            { pointer = "/records"; select = "/identity/name"; not_contains = "keel_diamond_top"; }
          ]; };
        }
        (restart "restart-graph")
        (bindSearch "find-target-after-restart" "keel_fanout_target")
        {
          id = "fanout-after-restart"; kind = "cli"; args = [ "related" "\${coordinate}" ];
          expect = { exit = 0; json = [
            { pointer = "/records"; select = "/identity/name"; set_equals = [
              "keel_fanout_target" "keel_fanout_a" "keel_fanout_b" "keel_fanout_c"
              "keel_fanout_d" "keel_fanout_e" "keel_fanout_f" "keel_fanout_g" "keel_fanout_h"
            ]; }
          ]; };
        }
      ];
    }
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
        {
          id = "default-cli-summary-identity"; kind = "cli";
          args = [ "search" "keel_rust_leaf" "--limit" "20" ];
          expect = { exit = 0; json = [
            (eq "/records/0/identity/name" "keel_rust_leaf")
            (eq "/records/0/identity/coordinate" "\${coordinate}")
            (eq "/records/0/identity/path" "src/lib.rs")
          ]; };
        }
        {
          id = "default-mcp-summary-identity"; kind = "pages";
          tool = "backend.search";
          arguments = { query = "keel_rust_leaf"; limit = 20; };
          minimum_pages = 1; maximum_pages = 1;
          expect = { exit = 0; json = [
            (eq "/first/records/0/identity/name" "keel_rust_leaf")
            (eq "/first/records/0/identity/coordinate" "\${coordinate}")
            (eq "/first/records/0/identity/path" "src/lib.rs")
          ]; };
        }
        (show "show-leaf" "keel_rust_leaf" "src/lib.rs")
        (related "initial-caller" "keel_rust_middle" "keel_rust_decoy")
        { id = "edit"; kind = "write"; files."src/lib.rs" = read ./fixtures/rust/after.rs; }
        (index "reindex-edit")
        (search "find-added" "keel_rust_added")
        (bindSearch "find-revised-leaf" "keel_rust_leaf")
        (related "added-caller" "keel_rust_added" "keel_rust_decoy")
        { id = "detach-caller"; kind = "write"; files."src/lib.rs" = read ./fixtures/rust/detached.rs; }
        (index "reindex-detach")
        (bindSearch "find-detached-leaf" "keel_rust_leaf")
        (detached "stale-edge-removed" "keel_rust_middle" "keel_rust_added")
        unknown
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
        { id = "detach-caller"; kind = "write"; files."src/history.py" = read ./fixtures/python/detached.py; }
        (index "reindex-detach")
        (bindSearch "find-detached-leaf" "keel_python_leaf")
        (detached "stale-edge-removed" "keel_python_entry" "keel_python_added")
        unknown
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
        { id = "detach-caller"; kind = "write"; files."src/history.js" = read ./fixtures/javascript/detached.js; }
        (index "reindex-detach")
        (bindSearch "find-detached-leaf" "keelJavaScriptLeaf")
        (detached "stale-edge-removed" "keelJavaScriptEntry" "keelJavaScriptAdded")
        unknown
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
