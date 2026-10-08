# Typed MCP tool failure contract, 2026-10-08

Source-only successor to `37299addae` / `b2c43280ea`, based on reviewed `3e466953bda333f083b1b5682350a2844008474b`. No native job, daemon or live MCP child was launched for this atom. The retained 81c and 5f2 application receipts are historical baselines, not proof of this successor.

## Contract

Root approved the negotiated 2025-11-25 tools error contract after reviewing the official specification: https://raw.githubusercontent.com/modelcontextprotocol/modelcontextprotocol/main/docs/specification/2025-11-25/server/tools.mdx . A recognized, well-formed tools/call envelope reports input-validation and domain failures in a structured tool result with `isError:true`. Invalid JSON-RPC/CallToolRequest envelopes, unknown tools and actual server proof/protocol failures remain JSON-RPC errors.

The route boundary now retains the original typed shared Fault until making this decision. Its closed FaultSlug match is exhaustive: adding a shared failure class requires a deliberate contract choice. It never classifies display text, parses strings as compiler authority, suppresses a cause, or returns isError:false for a refusal. Protocol, wrong-request, freshness-proof and retained-view integrity failures remain RPC errors across direct and generic routes. Compiler refusals, partial publication, missing records, stale cursors, invalid input and bounded transport refusals are tool results. Resources/prompts retain their existing RPC error contracts.

The direct graph adapter handles `CommandReply::Failed` before its ProjectionPage shape check and uses the same `probe_fault` lowering as CLI and shared paged reads. Generic graph errors use this boundary as well. A missing graph coordinate can therefore offer the shared exact-name search; corrupt graph state remains a server failure. Verified continuation decoding no longer flattens every non-stale typed failure into “unknown cursor”: malformed external tokens fail input validation before decoding, while an authenticated retained token's genuine protocol/transport cause survives.

The generic surface adapter retains its actual SurfaceCommand and calls `product_view_for_command`, including the exact requested dependency package/action. It serializes the shared fault alongside the full raw SurfaceReply and existing typed index_job projection. Partial publication therefore retains the complete receipt/source-profile/refusal partition together with isError:true; it does not collapse to the first compiler refusal or lose the useful published languages. No library wire version, owner recipe, signed cursor limit, response byte ceiling or compiler budget changes.

## Focused behavioral matrix (source controls; unrun)

| Case | Expected result | Preserved checks |
| --- | --- | --- |
| Each advertised tool with required operands omitted; catalog-only query/surface omitted inputs | structured usage, malformed, isError:true | exact response ID, no probe/index/surface/graph owner calls |
| Object versus copied-string exact ticket; malformed/edited ticket | valid exact owner ticket or structured usage | no malformed ticket reaches owner |
| Wrong argument container/null, missing/wrong name, unknown tool | RPC -32602 | no tool result or owner admission |
| Direct Failed(NotFound/WrongBasis), generic graph typed equivalents | typed tool failure | coordinate, search action and closed cause |
| Direct graph IncoherentView or valid-looking compiler/protocol display strings | RPC protocol/integrity fault | no forged compiler facts or shape substitution |
| Stale actual catalog cursor; signed context/project/authority/query/limit/detail replay negatives | typed cursor/usage tool failure | exact owner ordering, no skipped/duplicated rows and no refused-cursor page execution |
| Authenticated cursor decode Protocol/RequestMismatch/Freshness/IncoherentView versus StaleCursor/CursorMismatch/Transport | RPC for proof failures; structured domain tool refusal otherwise | original typed failure and no second catalog page |
| Native compiler refusal via graph/continuation/job routes | structured tool failure; prompt remains RPC | exact compiler facts, actionable SDK hint, bounded sanitized explanation |
| Generic dependency refusal | isError:true + full SurfaceReply + shared Fault | actual requested package, not-captured cause, exact inspect-package action |
| PartiallyPublished progress, Summary and Full | one MCP wire result contains isError:true, exact index_job, ticket, receipt, source tuple, all profiles/refusals | actual backend DTO encode/decode/admit; no Complete claim for unavailable TypeScript |

Existing cursor ownership/replay assertions remain intact; only their deliberate MCP error-envelope expectations change. The earlier independent `90fa90b8bf` fixture correction remains in history; this successor changes the final expectation to the approved tool-result shape.

## Validation

Changed Rust files parse with rustfmt and `git diff --check` passes. This is syntax/source review only, not a compile/test/native pass. After Root review, run the full backend-present, CLI and MCP suites on an explicitly owned remote warm graph with fresh v5 admission, then real empty-shelf/first-index/cancel/refusal/partial scenarios with a matched CLI/MCP/locald trio. The pure fixture controls exercise routing, serialization and admission; they do not establish a native compiler generation or application semantic correctness.
