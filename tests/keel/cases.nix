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
    bind = {
      "caller_${suffix}" = { pointer = "/records/0/identity/semantic_data"; transform = "selected-symbol-key"; };
      "caller_${suffix}_coordinate" = "/records/0/identity/coordinate";
    };
    expect = { exit = 0; json = [ (eq "/records/0/identity/name" "keel_fanout_${suffix}") ]; };
  };
  diamondIdentity = suffix: (search "diamond-identity-${suffix}" "keel_diamond_${suffix}") // {
    bind = {
      "diamond_${suffix}" = { pointer = "/records/0/identity/semantic_data"; transform = "selected-symbol-key"; };
      "diamond_${suffix}_coordinate" = "/records/0/identity/coordinate";
    };
  };
  targetNode = { id = "\${center_id}"; coordinate = "\${coordinate}"; };
  callerNode = suffix: { id = "\${caller_${suffix}}"; coordinate = "\${caller_${suffix}_coordinate}"; };
  diamondNode = suffix: { id = "\${diamond_${suffix}}"; coordinate = "\${diamond_${suffix}_coordinate}"; };
  u32Node = { id = "\${u32_id}"; coordinate = "\${u32_coordinate}"; };
  compilerEdge = from: to: relation: {
    from = from.coordinate; from_id = from.id;
    to = to.coordinate; to_id = to.id;
    inherit relation; confidence = "compiler";
  };
  relatedCapture = id: center: edges: {
    inherit id; kind = "pages"; tool = "backend.related";
    arguments = { coordinate = center.coordinate; limit = 20; };
    minimum_pages = 1; maximum_pages = 1;
    expect = { exit = 0; json = [
      (eq "/first/related_basis/schema" 1)
      (eq "/first/related_basis/center_id" center.id)
      (eq "/first/related_basis/coordinate" center.coordinate)
      (eq "/first/related_basis/source_view_root" "\${source_root}")
      (eq "/first/related_basis/relation_coverage" "unknown")
      (eq "/first/related_evidence_withheld" false)
      { pointer = "/first/edges"; set_equals = edges; }
    ]; };
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
        (diamondIdentity "left") (diamondIdentity "right") (diamondIdentity "top")
        {
          id = "resolve-return-type"; kind = "query";
          cli = [ "resolve" "u32" "--limit" "20" ];
          tool = "backend.resolve"; arguments = { query = "u32"; limit = 20; };
          bind = {
            u32_coordinate = "/records/0/identity/coordinate";
            u32_id = { pointer = "/records/0/identity/semantic_data"; transform = "selected-symbol-key"; };
          };
          expect = { exit = 0; json = [
            (eq "/records/0/identity/name" "u32")
            (eq "/records/0/identity/shape" "external")
            { pointer = "/records/1"; absent = true; }
          ]; };
        }
        {
          id = "pin-related-source-root"; kind = "cli"; args = [ "status" ];
          expect = { exit = 0; json = [ (eq "/answer" "status") ]; };
          bind.source_root = { pointer = "/revision"; transform = "hex-root"; };
        }
        {
          id = "guidance-status-before"; kind = "query"; tool = "backend.status";
          cli = [ "status" ];
          arguments.detail = "full";
          expect = { exit = 0; json = [
            (eq "/answer" "status")
            (eq "/revision" "\${source_root}")
          ]; };
        }
        {
          id = "guidance-graph"; kind = "query"; tool = "backend.graph";
          cli = [ "graph" "\${coordinate}" "--limit" "200" ];
          arguments = { coordinate = "\${coordinate}"; limit = 200; detail = "full"; };
          expect = { exit = 0; json = [
            (eq "/answer" "records")
            (eq "/query" "\${coordinate}")
            (eq "/more" false)
          ]; };
        }
        {
          id = "guidance-source"; kind = "query"; tool = "backend.source";
          cli = [ "source" "\${coordinate}" ];
          arguments = { coordinate = "\${coordinate}"; detail = "full"; };
          expect = { exit = 0; json = [
            (eq "/answer" "page")
            (eq "/identity/coordinate" "\${coordinate}")
            (eq "/source/extent" "complete")
          ]; };
        }
        (relatedCapture "typed-related-call-edges" targetNode (
          (map (suffix: compilerEdge (callerNode suffix) targetNode "calls") [ "a" "b" "c" "d" "e" "f" "g" "h" ])
          ++ [ (compilerEdge targetNode u32Node "type-reference") ]
        ))
        (relatedCapture "typed-diamond-junction" (callerNode "a") [
          (compilerEdge (callerNode "a") targetNode "calls")
          (compilerEdge (diamondNode "left") (callerNode "a") "calls")
          (compilerEdge (diamondNode "right") (callerNode "a") "calls")
          (compilerEdge (callerNode "a") u32Node "type-reference")
        ])
        (relatedCapture "typed-diamond-left" (diamondNode "left") [
          (compilerEdge (diamondNode "left") (callerNode "a") "calls")
          (compilerEdge (diamondNode "top") (diamondNode "left") "calls")
          (compilerEdge (diamondNode "left") u32Node "type-reference")
        ])
        (relatedCapture "typed-diamond-right" (diamondNode "right") [
          (compilerEdge (diamondNode "right") (callerNode "a") "calls")
          (compilerEdge (diamondNode "top") (diamondNode "right") "calls")
          (compilerEdge (diamondNode "right") u32Node "type-reference")
        ])
        (relatedCapture "typed-diamond-top" (diamondNode "top") [
          (compilerEdge (diamondNode "top") (diamondNode "left") "calls")
          (compilerEdge (diamondNode "top") (diamondNode "right") "calls")
          (compilerEdge (diamondNode "top") u32Node "type-reference")
        ])
        {
          id = "guidance-status-after"; kind = "query"; tool = "backend.status";
          cli = [ "status" ];
          arguments.detail = "full";
          expect = { exit = 0; json = [
            (eq "/answer" "status")
            (eq "/revision" "\${source_root}")
          ]; };
        }
        {
          id = "eight-callers"; kind = "cli"; args = [ "related" "\${coordinate}" ];
          expect = { exit = 0; json = [
            { pointer = "/records"; select = "/identity/name"; set_equals = [
              "keel_fanout_target" "keel_fanout_a" "keel_fanout_b" "keel_fanout_c"
              "keel_fanout_d" "keel_fanout_e" "keel_fanout_f" "keel_fanout_g" "keel_fanout_h" "u32"
            ]; }
            { pointer = "/records"; select = "/identity/name"; not_contains = "keel_fanout_decoy"; }
          ]; };
        }
        (bindSearch "find-diamond-junction" "keel_fanout_a")
        {
          id = "diamond-incoming"; kind = "cli"; args = [ "related" "\${coordinate}" ];
          expect = { exit = 0; json = [
            { pointer = "/records"; select = "/identity/name"; set_equals = [
              "keel_fanout_a" "keel_fanout_target" "keel_diamond_left" "keel_diamond_right" "u32"
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
              "keel_fanout_d" "keel_fanout_e" "keel_fanout_f" "keel_fanout_g" "keel_fanout_h" "u32"
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
        {
          id = "index-alias-background-intent"; kind = "cli";
          args = [ "index" "." "--execution-intent" "background" "--wait" "--wait-timeout" "180" ];
          expect = { exit = 0; json = [
            (eq "/index_operation/state" "known")
            (eq "/index_operation/detail/state/state" "published")
          ]; };
        }
        {
          id = "package-graph-at-public-maximum"; kind = "cli";
          args = [ "package-graph" "pkg:cargo/keel-rust-history@0.1.0" "dependencies" "--limit" "128" ];
          expect = { exit = 0; json = [
            (eq "/package_graph_page/limit" 128)
          ]; };
        }
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
