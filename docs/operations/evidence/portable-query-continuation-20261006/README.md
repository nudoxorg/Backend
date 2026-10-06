# Portable query continuation checkpoint

Base: `039c360d286962a6cf19488fb86caabd1d8c93de` (DTO20).

The client exports a bounded `pc2-` representation of the existing next `CommandDto` and its canonical predecessor certificate. Import uses the existing strict command decoder, then obtains an authenticated current Revision before sending the exact next Search or Name query. Imported claims do not mint a capability. Query family, text, limit, read manifest, selected root, producer scope/context/evidence, branch, log, schema, and cursor frontier remain bound. A no-op sequence advance is allowed; a changed root or authority is refused. Resume performs one Revision RPC and one query RPC, with no prior-page replay.

The issuing scope is compared with the query response even on terminal pages. Export measures the existing DTO serializer against the 32 KiB token budget before allocating the encoded body; overflow is a typed size refusal. CLI JSON emits `nextCursor`, and `--cursor` routes through the shared presentation driver. MCP uses the same inner query proof but retains its existing signing-key, parameter-context, and expiry boundaries.

At this checkpoint the initial five focused tests passed. The combined suite reached CLI 36/36 and client 95/96. The failing budget test incorrectly assumed that a 200-row page must produce an oversized predecessor proof; it was replaced with a legitimately long query/label fixture and a separate compact full-credit test. Those replacement tests still require execution. This checkpoint is not a final green receipt or actual fresh-process CLI acceptance.

The first combined run also exposed pre-existing test fixtures using arbitrary query projection recipes. The CLI fixture now uses the actual Library producer, and the authenticated remote fixture uses the shared QueryPageRecipe; admission was not weakened.

Further local compilation was paused at the shared disk threshold (15.60 GiB available). `probe-real-portable-cli.py` is prepared for the preserved Mac039 37-coordinate fixture, separate CLI processes per page, cold owner reopen, query and proof tamper negatives, and CLI/persistent-MCP stream parity. It preserves the original project/state and only uses a private state copy. No actual result from that observer is claimed yet.

## Reviewed follow-up

The revised focused client controls passed **10/10**. A maximum-length admitted query exercises the typed export refusal; a full-credit 200-row compact predecessor successfully exports. The new nonquery regression uses an actual Library-produced graph projection through injected Session transport: six parent/child rows with credit two. It failed in the unconditional query exporter, then passed after the guard, preserving two records and `more:true` without inventing a portable nonquery token. The CLI now exports only continuations carrying an admitted portable query packet. A test-only workspace blake3 dependency supports explicit owned fixture evidence; no registry dependency was added.

Running already compiled present/MCP test binaries also exposed baseline fixture drift: present 90/91 (old TypeScript tooling-unavailable assumption), MCP 94/96 (arbitrary query recipe fixture and obsolete line-style rendering of byte spans). These failures are preserved and coordinated with the separate public-shape repair; this slice does not weaken their contracts or claim whole suites green. `validation-receipts.tar.gz` stores byte/hash-indexed before failures and focused passes. Actual separate CLI-process runtime proof remains pending a warm candidate binary.

The portable issuer boundary is logical capability/source/root/branch/log/schema/frontier plus exact query intent. A fresh authenticated transport re-admits it. An identically admitted mirrored or reopened state is intentionally portable; PID and endpoint strings are not authority and are not added as unsigned cursor fields. Differing producer capability or inadmissible stream remains refused. Endpoint/tenant access stays in the transport policy.

The actual observer captures independent single-page limit-200 references with the candidate client and checks their 37-coordinate membership against the preserved source witness. This permits the separately reviewed relevance-order repair to change Search display order while still requiring every limit-three traversal to equal the candidate's independent ordered result and the unchanged source-coordinate set.

Signer qualification: normal MCP startup reads the durable workspace authority secret. Only failed private workspace initialization falls back to a process-only random signer. The observer therefore measures same-workspace fresh MCP processes as well as persistent-process parity; it does not infer outer signer rotation from the older pc1 session-map refusal. MAC, family, credit, query text, and valid detail changes are negative controls against the unchanged outer policy.
