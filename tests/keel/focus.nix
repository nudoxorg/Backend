# The receipt's profile is a stable two-byte language/edition code. Rust 2021
# is [0, 2] (semantic vocabulary Language::Rust and RustEdition::Rust2021).
let
  read = path: builtins.readFile path;
  emptySearch = name: {
    id = "absent-${name}";
    kind = "query";
    cli = [ "search" name "--limit" "20" ];
    tool = "backend.search";
    arguments = { query = name; limit = 20; };
    expect = { exit = 0; json = [ { pointer = "/records"; equals = [ ]; } ]; };
  };
in {
  schema = 1;
  cases = [
    {
      id = "active-rust-paused-source-files";
      language = "rust";
      node = "engine.lanes";
      files = {
        "Cargo.toml" = read ./fixtures/focus/Cargo.toml;
        "src/lib.rs" = read ./fixtures/focus/lib.rs;
        "src/paused.go" = read ./fixtures/focus/paused.go;
        "src/PausedJava.java" = read ./fixtures/focus/PausedJava.java;
        "src/paused.cs" = read ./fixtures/focus/paused.cs;
        "src/paused.cpp" = read ./fixtures/focus/paused.cpp;
      };
      steps = [
        {
          id = "index-active-only"; kind = "index";
          expect = { exit = 0; json = [
            { pointer = "/index_operation/detail/source_capture/profiles"; select = "/profile"; set_equals = [ [ 0 2 ] ]; }
            { pointer = "/index_operation/detail/source_capture/profiles/0/state/state"; equals = "published"; }
            { pointer = "/index_operation/detail/source_capture/profiles/0/state/detail/coverage/coverage"; equals = "complete"; }
          ]; };
        }
        {
          id = "find-active-rust"; kind = "query";
          cli = [ "search" "keel_active_rust_only" "--limit" "20" ];
          tool = "backend.search";
          arguments = { query = "keel_active_rust_only"; limit = 20; };
          expect = { exit = 0; json = [
            { pointer = "/records/0/identity/name"; equals = "keel_active_rust_only"; }
            { pointer = "/records/0/identity/path"; equals = "src/lib.rs"; }
            { pointer = "/records/0/language"; equals = "rust"; }
          ]; };
        }
        (emptySearch "KeelPausedGoOnly")
        (emptySearch "KeelPausedJavaOnly")
        (emptySearch "KeelPausedCSharpOnly")
        (emptySearch "KeelPausedCppOnly")
        { id = "restart"; kind = "restart"; }
        {
          id = "active-survives-restart"; kind = "query";
          cli = [ "search" "keel_active_rust_only" "--limit" "20" ];
          tool = "backend.search";
          arguments = { query = "keel_active_rust_only"; limit = 20; };
          expect = { exit = 0; json = [
            { pointer = "/records/0/identity/name"; equals = "keel_active_rust_only"; }
            { pointer = "/records/0/identity/path"; equals = "src/lib.rs"; }
          ]; };
        }
        ((emptySearch "KeelPausedGoOnly") // { id = "paused-go-after-restart"; })
      ];
    }
  ];
}
