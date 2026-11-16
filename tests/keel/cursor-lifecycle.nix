# A long owner page cursor is an mcp3 server-side handle. Copy the whole
# continuation_call: selectors, limit, detail and the opaque handle are never
# reconstructed. After the source root moves, the same live process must not
# stitch revisions; after MCP restart the handle table itself is gone.
let
  read = path: builtins.readFile path;
  eq = pointer: equals: { inherit pointer equals; };
in {
  schema = 1;
  cases = [ {
    id = "rust-mcp-cursor-lifecycle";
    language = "rust";
    node = "engine.lanes";
    files = {
      "Cargo.toml" = read ./fixtures/cursor-lifecycle/Cargo.toml;
      "src/lib.rs" = read ./fixtures/cursor-lifecycle/before.rs;
    };
    steps = [
      { id = "index-initial"; kind = "index"; }
      { id = "start-mcp"; kind = "mcp-start"; session = "reader"; }
      {
        id = "first-page"; kind = "mcp-call"; session = "reader";
        tool = "backend.search";
        arguments = { query = "KeelCursorBeacon"; limit = 1; detail = "standard"; };
        bind.continuation = "/continuation_call";
        expect = { exit = 0; json = [
          (eq "/answer" "records")
          { pointer = "/records/0/identity/name"; contains = "KeelCursorBeacon"; }
          (eq "/records/0/identity/path" "src/lib.rs")
          (eq "/more" true)
          { pointer = "/nextCursor"; contains = "mcp3-"; }
          (eq "/continuation_call/name" "backend.search")
          (eq "/continuation_call/arguments/limit" 1)
          (eq "/continuation_call/arguments/detail" "standard")
        ]; };
      }
      { id = "edit-source"; kind = "write"; files."src/lib.rs" = read ./fixtures/cursor-lifecycle/after.rs; }
      { id = "reindex-new-root"; kind = "index"; }
      {
        id = "reject-moved-root"; kind = "mcp-call"; session = "reader";
        call = "\${continuation}"; mcp_error = true;
        expect = { exit = 0; json = [
          (eq "/answer" "fault")
          (eq "/slug" "cursor-mismatch")
        ]; };
      }
      { id = "stop-mcp"; kind = "mcp-stop"; session = "reader"; }
      { id = "restart-mcp"; kind = "mcp-start"; session = "reader"; }
      {
        id = "reject-lost-handle"; kind = "mcp-call"; session = "reader";
        call = "\${continuation}"; mcp_error = true;
        expect = { exit = 0; json = [
          (eq "/answer" "fault")
          (eq "/slug" "usage")
          { pointer = "/detail"; contains = "short-lived server-side handle"; }
        ]; };
      }
      {
        id = "fresh-query-sees-edit"; kind = "mcp-call"; session = "reader";
        tool = "backend.search";
        arguments = { query = "KeelCursorBeaconAdded"; limit = 20; detail = "standard"; };
        expect = { exit = 0; json = [
          (eq "/answer" "records")
          (eq "/records/0/identity/name" "KeelCursorBeaconAdded")
          (eq "/records/0/identity/path" "src/lib.rs")
        ]; };
      }
      { id = "stop-restarted-mcp"; kind = "mcp-stop"; session = "reader"; }
    ];
  } ];
}
