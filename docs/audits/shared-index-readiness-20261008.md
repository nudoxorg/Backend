## 2026-10-09 MCP transport checkpoint: exact emitted byte accounting

MCP HTTP and stdio now admit responses using the exact escaped JSON bytes and
transport framing they emit. Structured results and continuation tokens survive
preview fitting; refusal metadata uses the same encoder. An oversized initialize
response cannot authorize an HTTP session. The default readiness frame remains
49,152 bytes; this checkpoint does not increase it.

Root independently checked all 43 raw evidence files, 18,030 producing Git blobs,
actual compiler events, four process receipts, whole-fleet admission samples,
source restoration, and the three preserved remote image hashes. The joined
`5a06a6f3d9bc4a6760a686356174cb48599ad514` source ran **143 MCP tests and
14 client tests, with zero failures or ignored tests**. Client controls cover
plain-graph and portable-query continuation; those changes are separate from
this transport checkpoint. The four transport change sequences match the
reviewed atom exactly; two whole files also contain graph changes in the tested
join. This is not an isolated transport or current canonical build result.

Evidence: [source-bound native audit](mcp-transport-native-evidence-20261009.json).
Previous CLI 47/2 and MCP 142/1 attempts remain retained. Final CLI/help changes,
genuine Docs CLI/MCP runtime, installed release acceptance and live GUI are
still pending. New builds remain held while h16001mac cannot supply a fresh
complete census; its interrupted GUI build has an unknown outcome.

# Shared publication readiness and typed absent reads, 2026-10-08

Source base: `3e466953bda333f083b1b5682350a2844008474b`, plus the independent required-path test expectation correction `90fa90b8bf`. This atom is source-only; no Cargo, daemon, CLI or MCP execution occurred for this successor.

## Observed input and output

The independently owned actual 81c Open WebUI baseline invoked `backend.outline {}` before indexing. Its tool result had `isError:true`, but reported `protocol/unproven` and “library record not found.” Source 81c is `81cde320dacde1be26ee6de80a9ccf2cdd9f6428`, tree `5bbf9298308a5241e60d7fdb3b83b70b5f74ab2a`; raw responses are `/root/evidence/luna-mcp-contract-quality-81c-20261008/raw/mcp-01/{mcp.wire.jsonl,calls.responses.json}`. Its empty status reported ready/0 rows, frontend 9/9, oracles 0/18 and unavailable semantic coverage. After a genuine compiler refusal, this same source could have structural rows and a useful outline; those facts do not prove a native semantic generation. These are retained historical actual receipts, not a successor pass.

## Cause and change

`CommandAdapter::standard` and its graph-page path converted closed `LibraryError` variants to `CommandReply::Error(String)`. The client correctly classified an unproven display string as a protocol failure. These paths now preserve `CommandFailure` through `CommandReply::Failed`. NotFound, WrongBasis, CursorMismatch and IncoherentView remain distinct through real wire decoding and caller admission.

An outline NotFound is given an exact path and `backend.index` action by shared presentation. The wording is deliberately “no outline is published … at this revision.” The catalog uses NotFound both for an absent package and an existing package with no outline symbols. This atom does not invent an absence witness, fake an empty root symbol, inspect a shelf to suppress corrupt records, or label every NotFound NotIndexed. Protocol, retained-view and cursor faults keep their original causes.

Status now exposes separate `availability`, `publication` and existing `readiness` facets in normal and summary JSON, Markdown and terminal text. Available means an admitted health reply was obtained. Empty/populated describes exact view row cardinality only; it says nothing about pending work or package membership. Readiness still describes the reported lanes; semantic coverage and native-oracle counts remain visible, independently of structural rows. No library DTO/protocol version changes.

## Source controls and pending native gates

- Real AdapterFixture dispatch for absent Outline, OutlinePage, Document, Source and GraphPage, strict reply serialization/decode and actual `admit_reply`; all preserve NotFound, stale outline preserves WrongBasis, owner cursor unchanged.
- Actual immutable catalog absence projected to exact Reindex guidance; corrupt relation and cursor failures never become absent-outline guidance.
- Empty and populated checked structural views preserve zero native-oracle readiness and exact publication cardinality in Summary/Standard/Full encoding.

Rust syntax parsing and `git diff --check` passed locally. Tests are unrun. After Root review, use an explicitly owned remote warm graph and fresh fleet admission: service filter `absent_catalog_reads_keep_typed_failure`, present `readiness_controls`, `available_health_distinguishes_empty_publication`, then the full present/CLI/MCP suites with the joined contract cohort. A synthetic projection control is not a real application acceptance pass.
