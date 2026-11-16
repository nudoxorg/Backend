# The 15 expected call sites and byte spans are independently authored from
# fixtures/references/*.rs. A page check aggregates all rows across the real
# MCP continuation_call chain and verifies exact path/span evidence.
let
  read = path: builtins.readFile path;
  source = file: start: end: { inherit file start end; };
  exactSources = [
    (source "src/a.rs" 28 43)
    (source "src/a.rs" 77 92)
    (source "src/b.rs" 28 43)
    (source "src/b.rs" 77 92)
    (source "src/c.rs" 28 43)
    (source "src/c.rs" 77 92)
    (source "src/d.rs" 28 43)
    (source "src/d.rs" 77 92)
    (source "src/e.rs" 28 43)
    (source "src/e.rs" 77 92)
    (source "src/f.rs" 28 43)
    (source "src/f.rs" 77 92)
    (source "src/g.rs" 28 43)
    (source "src/g.rs" 77 92)
    (source "src/g.rs" 126 141)
  ];
  project = {
    "Cargo.toml" = read ./fixtures/references/Cargo.toml;
    "src/lib.rs" = read ./fixtures/references/lib.rs;
    "src/a.rs" = read ./fixtures/references/a.rs;
    "src/b.rs" = read ./fixtures/references/b.rs;
    "src/c.rs" = read ./fixtures/references/c.rs;
    "src/d.rs" = read ./fixtures/references/d.rs;
    "src/e.rs" = read ./fixtures/references/e.rs;
    "src/f.rs" = read ./fixtures/references/f.rs;
    "src/g.rs" = read ./fixtures/references/g.rs;
  };
  findTarget = {
    id = "find-reference-target";
    kind = "query";
    cli = [ "search" "keel_ref_target" "--limit" "20" ];
    tool = "backend.search";
    arguments = { query = "keel_ref_target"; limit = 20; };
    bind = { coordinate = "/records/0/identity/coordinate"; };
    expect = {
      exit = 0;
      json = [
        { pointer = "/records/0/identity/name"; equals = "keel_ref_target"; }
      ];
    };
  };
  allSourceSpans = {
    pointer = "/records";
    select = "/reference_details/evidence/source";
    set_equals = exactSources;
  };
in {
  schema = 1;
  cases = [ {
    id = "rust-reference-sites";
    language = "rust";
    node = "engine.lower.rust";
    files = project;
    steps = [
      { id = "index-reference-fixture"; kind = "index"; }
      findTarget
      {
        id = "references-default-page";
        kind = "pages";
        tool = "backend.references";
        arguments = { coordinate = "\${coordinate}"; };
        minimum_pages = 1;
        maximum_pages = 16;
        expect = { exit = 0; json = [ allSourceSpans ]; };
      }
      {
        id = "references-continuations-at-three";
        kind = "pages";
        tool = "backend.references";
        arguments = { coordinate = "\${coordinate}"; limit = 3; };
        minimum_pages = 5;
        maximum_pages = 16;
        expect = { exit = 0; json = [ allSourceSpans ]; };
      }
    ];
  } ];
}
