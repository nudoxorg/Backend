# Payload token evidence

These fixtures are emitted by the canonical typed projection generator at
`tools/token-budget/src/main.rs`; no fixture JSON is hand-authored. The
generator constructs domain values, calls `backend_present::encode_answer` (or
the typed fault/schema projection), and emits the exact UTF-8 bytes that CLI
and MCP share. It covers every shared answer family (page, records, shelf,
outline, status, and product). `tools/token-budget/measure.py` compares those
bytes with the checked-in files and recomputes their exact token counts. The
query-page fixture exercises the generic graph result envelope through the
same typed serializer as MCP.

The exact-token audit is pinned to `tiktoken==0.11.0`, encoding
`cl100k_base`, under the MCP policy `mcp-cl100k-base-v1`. It hashes the
encoding's pattern, special-token map, and mergeable-rank map before counting:

```text
8613f818e5af318d868379772ce5234e3a3e8519b6c611d94cf708fdfc580fbc
```

Set `TOKEN_BUDGET_TIKTOKEN_ROOT` to the pinned Python environment and run:

```sh
TOKEN_BUDGET_TIKTOKEN_ROOT=/path/to/tiktoken/site-packages \
  /opt/homebrew/bin/python3 tools/token-budget/measure.py
```

The gate fails on tokenizer version/hash drift, canonical DTO byte drift,
invalid UTF-8, missing payloads, or byte/token cap violations. The refusal
fixture has no payload and asserts `partial_bytes: 0`, proving oversized
responses are admitted atomically. `payload-budgets.json` is the compact
machine-readable report: it records bytes, the repository's estimate, exact
tokens, allocation observations, fixture hashes, and default/hard caps.
