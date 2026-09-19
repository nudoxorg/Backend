# Axum MCP probe (2026-09-13)

Fresh corpus: `/tmp/backend-axum-probe-20260913/project` (Rust
`ProbeWidget`, methods, and `find_probe_widget`). Fresh state/socket:
`/tmp/backend-axum-probe-state-20260913d`. Matching binaries were
`/tmp/backend-axum-demo-target/debug/backend-mcp` and `backend-locald`.

Startup command (compiler variables were explicitly unset):

```sh
env -u COMPILER_GO_COMPILER -u NUDOX_GO -u NUDOX_GO_ORACLE -u NUDOX_GO_ORACLE_BIN BACKEND_LOCALD_BIN=/tmp/backend-axum-demo-target/debug/backend-locald BACKEND_MCP_TOKEN=axum-probe-token-20260913 /tmp/backend-axum-demo-target/debug/backend-mcp --project /tmp/backend-axum-probe-20260913/project --workspace /tmp/backend-axum-probe-state-20260913d --endpoint /tmp/backend-axum-probe-state-20260913d/locald.sock --http 127.0.0.1:0
```

Readiness: `http://127.0.0.1:61289/mcp`, `transport: streamable-http`,
`maxSessions:64`, `maxInFlight:64`, `maxRequestBytes:4194304`.

## Results

The exact direct requests were `curl --http1.1 -sS -i -X POST URL` with the
shown headers/body. Missing or wrong `Authorization: Bearer ...` returned
`401`, `WWW-Authenticate: Bearer realm="backend-mcp"`, and
`{"error":{"code":-32001,"message":"missing or invalid bearer token"},"id":null,"jsonrpc":"2.0"}`.
Initialization returned `200` plus
`Mcp-Session-Id: ca5d47eec4f190ad7bf446b515642a47`; initialized notification
returned `202`; ping returned `200 {"id":2,"jsonrpc":"2.0","result":{}}`.
Unknown session returned `404`/`-32001`. `GET /mcp` returned `405`
(`Allow: POST,DELETE`); `/other` returned `404`.

With a session, `{not-json`, an empty body, and a JSON array returned HTTP
`200` JSON-RPC errors `-32700 Parse error`, `-32700 Parse error`, and `-32600
Invalid Request`. Unknown method returned `200`/`-32601` with its request ID.

`backend.status` initially returned `isError:false`, `rows:0`, `readiness:
"ready"`. `backend.index` returned accepted/pending intent
`c64bb54133f76412c130cb88e5426ce68f7d346f887afb9fcab0b7760dca7f51`.
After 400 ms, status showed `rows:8`, `sequence:1`, exact complete and
semantic partial `0/1`, `readiness:"indexing"`. Search request
`backend.search(query:"ProbeWidget",limit:50)` returned `isError:false`, two
captured rows: `...::src/lib.rs:4::ProbeWidget` and
`...::src/lib.rs:22::find_probe_widget`.

## Issues

1. **Transport bug — media negotiation ignored.** `Content-Type: text/plain`
   and `Accept: text/plain` both still returned `200` ping responses. A
   Streamable HTTP endpoint should enforce JSON request/acceptable response
   media types.
2. **Transport bug — session gate masks parse errors.** Valid-token malformed
   JSON without `Mcp-Session-Id` returned `400` `-32001 session required`; the
   same body with a session returned `200` `-32700 Parse error`.
3. **Transport bug — retained local stream can break.** After index, search on
   two sessions returned HTTP `200` tool results with
   `isError:true, structuredContent.error.kind:"transport", message:"local endpoint: local control I/O failed: BrokenPipe"`; a fresh session restored access.
4. **Build issue — binary pairing.** Repository `target/debug/backend-mcp`
   with its older `backend-locald` returned `-32603` detail `unsupported reply
   DTO version`; the matching `/tmp` pair worked. Ship/rebuild the pair
   together.

Product limitations: the tiny corpus has no oracle manifests, so semantic
coverage remains partial while structural search works; no GET/SSE handler is
provided, limiting server-initiated Streamable HTTP events. Probe processes
were terminated.
