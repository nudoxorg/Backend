# CLI/MCP reliability checkpoint, 2026-10-08

This checkpoint preserves typed domain failures through CLI and MCP, retains complete partial-index receipts, accepts exact job tickets as objects or copied JSON strings, and makes plain graph continuations portable across a fresh owner and intervening paged queries. Readiness distinguishes observed owner availability, publication cardinality and semantic coverage. Suggested commands quote exact operands as single shell arguments.

Root reviewed the complete production and test diff from `e42bfc4e7c719f7fab60d02fded3342cfd62e6b6` to `146c430f842df6042d62dc82c3eea4e9d77cf006`, then cherry-picked only its reviewed atoms onto canonical `07dd35c9e0157ff4f4375bb6ca16549db769ac5a`. The oversized-query borrow correction was already present and its empty cherry-pick was skipped. Every one of the 30 reviewed changed paths is byte-identical to the reviewed final head. Canonical's other changes and Cargo.lock are preserved.

Root independently verified the raw archive, every receipt/log hash, clean before/after source identities and lock hashes, fleet admission freshness, actual individual test results, Cargo artifact records, and unchanged tested crate scopes at the final reviewed head:

| Gate | Tested source | Passed | Failed / ignored |
| --- | --- | ---: | --- |
| Client continuation controls | `6d0d5be127` | 18 | 0 / 0 |
| Full presentation library | `ae3d47d7a2` | 109 | 0 / 0 |
| Full CLI library | `d00c6cb805` | 45 | 0 / 0 |
| Full MCP library | `e3991e5b1e` | 118 | 0 / 0 |
| Real adapter/wire absent-catalog control | `146c430f84` | 1 | 0 / 0 |

These are 291 test passes across successive source cohorts, not a same-final-head combined run or real-project acceptance count. The selected-ID golden retains all historical fields after removing only the 200 exact selected-symbol IDs and derived budget metadata. Earlier atom-level audit documents retain their original source-only status; this ledger adds the later native results.

Proofs and integration correspondence are sealed in [the evidence directory](../operations/evidence/cli-mcp-reliability-20261008/integration-seal.json). Kernel-wait retirement is recorded by each native supervisor. Root audited artifact records but did not independently rehash the remote test executables in this checkpoint.

## Remaining acceptance gates

Rebuild a matched CLI/MCP/local-service trio and replay the actual Docs and Excalidraw plain-graph restart and intervening-query failures. Verify real CLI terminal JSON and its nonzero exit together, copied-ticket progress/cancellation, cold restart and every requested public surface. Existing installed binaries are older cohorts and are not proof of this source. Byte-packed search pages and the newer protocol work remain separate; this checkpoint does not fix oversized full-record pages or alter their bounds. The existing oversized JSON-RPC ID fallback can still emit a null ID when even a refusal envelope cannot fit the wire limit.
