# Exact MCP payload budget gate

`backend-token-budget` constructs payloads through the public typed
`backend-present` encoder and the MCP `tools/list` projection. It is a
development/test binary; the MCP and CLI request paths do not depend on a
runtime tokenizer. `measure.py` then runs the pinned `tiktoken==0.11.0`
`cl100k_base` encoding, verifies its merge table/pattern artifact hash, and
compares the exact UTF-8 bytes against the checked-in fixtures.

`backend-token-budget --tools` additionally enumerates the live MCP tool
schema, selects the detail modes it actually advertises, and builds bounded
worst-case rows for every tool. Each matrix row is passed through the
production MCP `tool_result` and whole JSON-RPC response gates, so its wire
bytes cannot drift from a live `tools/call`. `mcp_matrix.py` computes exact
model tokens for those serialized responses and writes
`crates/present/fixtures/mcp-tool-budgets.json`; the
`estimated_tokens_upper_bound` column remains the conservative byte heuristic
used by runtime admission. Matrix labels use `compact` for the schema's
`summary` projection, `default` for its advertised default, and `full` for
the `full` projection; a tool without a detail property receives its one
`default` row.

The `model_policy` in `tokenizer.json` is the MCP `cl100k_base` policy. A
changed encoding, package version, merge table, or regex fails before fixture
counts are accepted. The generator covers every shared answer family (page,
records, shelf, outline, status, and product), empty/common and 200-record
pages, Unicode and RTL, long identifiers/documentation/source, continuation
envelopes, typed errors, the Trustfall query-page envelope, the generated MCP
tool schema, and atomic oversized refusal.

Run the audit with the pinned Nix shell toolchain (do not use `nix develop`):

```sh
nix shell 'git+file:///Users/mileswirht/Downloads/backend#luna-tools' \
  --command cargo build --offline -p backend-token-budget
TOKEN_BUDGET_TIKTOKEN_ROOT=/path/to/pinned/tiktoken/site-packages \
  /opt/homebrew/bin/python3 tools/token-budget/measure.py

TOKEN_BUDGET_TIKTOKEN_ROOT=/path/to/pinned/tiktoken/site-packages \
  /opt/homebrew/bin/python3 tools/token-budget/mcp_matrix.py --write
```

Use `--write` only when the canonical Rust projection intentionally changed:

```sh
TOKEN_BUDGET_TIKTOKEN_ROOT=/path/to/pinned/tiktoken/site-packages \
  /opt/homebrew/bin/python3 tools/token-budget/measure.py --write
```

The default output is one compact machine-readable JSON object. The checked-in
`payload-budgets.json` additionally records byte counts, exact token counts,
allocation observations, fixture hashes, and default/hard byte and token caps.
