Default `backend.graph` now uses the owner's existing `GraphPage` path even when the caller omits `limit` and `cursor`. The default remains 25 rows. Every returned continuation comes from the owner and passes through the existing workspace, request, detail, expiry, and authority authentication.

The MCP response also measures the escaped JSON bytes of its structured answer and readable duplicate together. If the duplicate makes a complete typed page exceed the response budget, it retains a shorter UTF-8 preview with an explicit marker. All structured rows, coverage, and signed continuation bytes remain intact. A typed page that cannot fit still receives the atomic oversize refusal. The final JSON-RPC gate checks the actual response, including the caller's ID.

Production change: `c9c50b4b8343229a58571faf7c689a0608b6855a`. The test-only followup `055195b596667ac934c80c1c8a4efde77aad9324` uses the application's ranked search producer projection. Root's canonical index-path error fixture `0cdad50e03` was joined before the full gate. Its resulting exact test cohort is `bbbb06ee62e02ceb6dd6559d72b95508a0ac0f94`, tree `d1a066d8fe7990fa014ccfacb6c72e5da3a22237`.

| Native gate | Result | Admission age at launch | Kernel wait |
| --- | --- | --- | --- |
| Original c9 targeted run | Preview test passed; ordering fixture failed | 19.83 s | 101 |
| Corrected ranked-producer targeted run | Both tests passed | 24.45 s | 0 |
| Exact joined full MCP library suite | 105 passed, none filtered | 35.02 s | 0 |

The original ordering failure is retained in full. Compatibility `Library::search` deliberately drops separate relevance ordering when it converts a ranked result to canonical snapshot storage. Comparing its 200-credit snapshot with concatenated 25-credit pages was an incorrect presentation oracle. The corrected test uses `search_from_ranked_ids`, which retains absolute rank and validates the predecessor's scored rows, matching the application service's producer seam.

The default collection test traverses three owner pages of an incomplete 61-declaration catalog through the actual MCP JSON-RPC handler. It checks search, resolve, and graph order, exact-once membership, ordinary default credit, signed continuations, bounded complete replies, and refusal after the selected owner snapshot changes. The preview test exercises JSON escaping and UTF-8 boundaries while retaining the complete typed value, and separately checks a genuinely oversized typed-value refusal.

Each native Cargo job had a new successful complete three-host census and retained the allocated private `compiler-index.lock` for its entire lifetime. The final permit PID was 57672 and its Cargo launcher PID was 57679. It retired at 19:55:41 UTC. The post-gate process check found none of the three owned permit or launcher pairs still alive. No runtime owner was launched. Commands, raw admissions, stdout, stderr, source cohort, executable hash, and completed wait receipts are retained beside this file and enumerated in `manifest.json`.

The captured Requests 0584 failures at 52,969 / 53,499 / 59,112 bytes explicitly requested `detail=full`; they are not evidence about omitted-detail defaults. The final matched runtime still needs real Requests default calls. A 53 KiB module document can remain too large because Standard and Full share its complete document/members/relations DTO, which has no section continuation. That requires explicit section paging or an admitted deferred-section affordance. Search continuation recipes also commit to page credit and predecessor rows, so the MCP layer cannot change credit mid-chain or slice rows and invent a cursor. These native fixture tests claim no archive acquisition, compiler publication, whole-project indexing, or live Requests acceptance.
