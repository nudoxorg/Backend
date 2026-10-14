# T2 surface prototype (W2.3–W2.5)

Build once with T1, then run the exact binaries with the pinned semantic tools:

```sh
nix develop .#surface --command python3 .config/ci/surface.py \
  --bin-dir /absolute/path/to/t1/bin --case polyglot --report /tmp/surface-report.json
```

`--update` is a separate, explicit review action. Inspect the changed JSON and
explain why the behavior is correct in the PR before committing it. CI prints
missing snapshots, but never accepts them. Indexing must succeed before any
snapshot can be admitted; every configured fixture search must return records.
MCP failures and missing replies fail the case even when the process exits zero.

The polyglot fixture is a synthetic bootstrap for the harness, containing seven
named declarations. It does not complete the representative real-project corpus
in W2.3. That corpus must use pinned Nix fetchers and record revisions, licenses
and selection reasons. GUI integration and parallel artifact consumers remain
separate work in W3.3.

CLI and MCP share one isolated owner, workspace and credential. Fixed scratch
paths keep source locations stable, and an exclusive lock prevents simultaneous
local runs from replacing that state. Normal exit waits for the owner's public
idle retirement. Logs remain under `/tmp/nudox-surface/<case>/locald.log`.
Indexing setup records its successful exit instead of its random caller-journal
receipts; subsequent query answers retain their product fields. Only explicitly
listed timestamp/timing/process fields are normalized.

Wall time is measured separately for each CLI operation and for the MCP session.
Peak RSS and CPU cover the owner over the whole case, including indexing; they
exclude compiler subprocesses and CLI/MCP resource use. Linux `/proc` is required.
The initial ratios and noise floors in `surface.py` are provisional and need
repeated measurements before this becomes a required performance gate. Compiler
shell acquisition is infrastructure setup, outside those operation measurements.
CI_T2_EXTERNAL=1 delegates T2 to an isolated parallel consumer. Local T1
commands retain the advisory integration step by default. Consumers verify and
import the exact artifact runtime before entering the pinned surface shell;
their checkout provides harness/configuration only, with no product build.

## Initial snapshot review

Codex reviewed the first successful output from Concourse **457894**, source
`adabb13b4`, on 2026-10-12. Indexing exits zero. Each search returns exactly one
ready declaration at its authored location: ferris (Rust, src/lib.rs:2), monty
(Python, src/main.py:1), turing (TypeScript, src/main.ts:2), Gopher (Go,
go/main.go:4), Duke (Java, src/Duke.java:1), Anders (C#, src/Main.cs:1), and
Bjarne (C++, src/main.cpp:3). The published API groups classes under `struct`.
The complete outline contains those seven declarations plus their members,
implicit nodes and the included C++ namespace (19 declarations total). Packages
reports the indexed t2-polyglot project ready. MCP structured search answers for
ferris/Gopher and the outline equal the corresponding CLI JSON exactly.

The embedding-based semantic search lane remains explicitly unconfigured; this
fixture exercises native language indexing and name queries, and preserves that
diagnostic rather than pretending embedding coverage exists. Actionable public
coordinates and native operands remain in the golden for review.

The same run measured indexing at 46.22 s, owner CPU at 41.81 s and owner peak RSS
at 788,808 KiB; individual queries were 0.02–0.69 s. The advisory step including
compiler-shell setup took 67 s. These are the initial provisional baselines;
they do not establish worker-independent performance tolerances.

### MCP preview review after canonical update

Concourse 459597, source `2ac2f0a1f`, verified the artifact-backed case after
canonical `fd856a290`. The only golden differences are the two MCP search text
previews and their wire budgets. Canonical now prints the returned declaration's
name/kind/language and exact actionable coordinate in bounded human-readable
text, rather than only saying a row exists. ferris and Gopher are the authored
fixture declarations; both structured answers and every CLI/outline/package
answer remain byte-for-byte equal to the reviewed contract. The text reports one
row shown and zero omitted. Actual wire sizes grow to 1,731/1,732 bytes (433
estimated tokens each), safely within the unchanged 49,152-byte limit.

Accept those visible preview changes; no fields are normalized away, fixture
expectations weakened, or performance baselines widened. A fresh consumer run
must match this revised golden before merge.

### MCP token and owner-page review (2026-10-13, W2.4)

Concourse **473058** rejected the old golden for exact source
`d9c0f1e2a83bf5636e03a90b73e97e3d6d5eb284`. This is the observable contract
introduced by canonical `db5c7238f`: MCP replies name the pinned cl100k token
policy/vocabulary and token limit, and outline uses an owner-certified page.
Both independent cases in that build produced byte-identical answer diffs.

Reviewed every changed field against the product change. All CLI answers and
both MCP search structured answers are unchanged. The MCP outline retains all
19 declarations, their exact coordinates/keys/kinds/names and parent-child
relationships; its roots and Java members now follow native page order rather
than the previous presentation order. The human-readable text follows that
same order. The new page is complete (`more: false`, limit 200), retains exact
source/branch/log identities, and reports exact/name/graph coverage complete
with embedding-based semantic coverage explicitly unconfigured.

Accept that documented paging and token-metadata contract. Search wire sizes
are 1,942/1,943 bytes and outline is 5,832 bytes, below the unchanged 49,152-byte
limit; 12,000-token admission and its pinned vocabulary are now also visible.
`estimatedTokens` remains the product's byte-derived estimate, not a measured
token count. No semantic IDs, page identities or ordering are normalized away;
no performance baselines or tolerance bands are changed. A fresh exact-head
artifact consumer must match this reviewed candidate before merge.

## Public CLI contract cases (W1.3)

The `cli-usage` case runs 14 selectable assertions against the exact published
CLI: root/add help, misspelled command, unknown option, missing coordinate and
alias coordinate, out-of-range and unsupported page bounds, invalid passive
search/compile intent, and absent-owner passive health/status. Assertions retain
exit codes, offending operands, actionable diagnostics and JSON/human/Markdown
stream contracts. Passive recovery is parsed and executed only inside a shell
function that prints its arguments, verifying hostile literal paths without
starting a product process. A launch sentinel, workspace and endpoint checks
reject unwanted state creation or owner startup. HOME/XDG/TMP are private and
ambient product configuration/credentials are excluded; every call is bounded.

Run all cases with T2, or just these without acquiring compiler tools:

```sh
python3 .config/ci/cli_contract.py --bin-dir /absolute/path/to/t1/bin
python3 .config/ci/cli_contract.py --bin-dir /absolute/path/to/t1/bin --case unknown-command
```

The T2 runner also selects this group with `--case cli-usage`. These explicit
assertions need no generated golden and report per-case wall time without
claiming owner CPU/RSS or a calibrated performance baseline. They replace the
public proofs recommended in the root W1.3 ledger; old Rust tests remain until a
reviewed deletion batch is approved. Valid indexing intents, live-owner health,
job tickets, bounded multi-match paging and the full command matrix remain
separate pending work. Negative admission checks reject a fake successful exit,
missing suggestion or changed operand; the product run must still pass in CI.

## Live owner and bounded-search assertions (W1.3)

The polyglot case additionally reads passive health in summary and full detail
from the exact published locald. Both must be available/populated and carry exact revision/source identities
matching an independent full MCP `backend.status` read of the same whole owner.
Readiness, coverage and visible row counts must agree. Setup requires a published
operation with all seven native profiles complete; health must then certify that
exact receipt root as ready. Failed/partial terminal receipts cannot pass on exit
zero. Coverage transitions must survive publication alongside changed rows. Summary omits full-only source fields and all short tags must
match their exact identities. This uses public cross-interface evidence instead
of an embedded echo owner or internal request/frame counters.

Search for the fixture's authored Java/C++ `language` methods at bound 200, then
at global bounds 1 and 3. The reference must be complete and multi-result;
bounded results must be its ordered prefix with a truthful `more` flag. These
checks catch ignored bounds, lost/reordered results and false completeness.
Both CLI and MCP also traverse that reference one result per page, using the
returned opaque cursor in a fresh client process on every request. CLI resumes as
`nudox --limit 1 search language --cursor TOKEN`; cursor is a command option. Combined rows
must exhaust the same complete reference in order, with unchanged query,
coverage and readiness. Missing/repeated cursors, duplicate/missing/reordered
rows and false completeness fail. The last page must stop offering a cursor.
Traversal has a 30-second deadline and a reference-sized page cap; individual
client calls are bounded to 10 seconds. Cursor values are never snapshotted.
Mutation checks exercise loss, repetition, query/coverage drift and false
exhaustion. Malformed CLI and MCP cursor strings must receive typed protocol
and invalid-params refusals respectively; successful responses, endpoint failures
and unrelated errors cannot satisfy the guard. A fresh passive health read then
confirms the owner remains ready at the same published revision. These malformed
strings do not prove tamper rejection for an otherwise valid signed token.
Cross-query, changed-limit, expired/stale and signed-token tamper refusal remain
separate work.

The checks run after existing CLI/MCP snapshots and add operation wall times to
the report. Volatile health statistics are not snapshotted; exact identities are
asserted against independently obtained public reads. Existing goldens, timing
baselines and tolerance bands stay unchanged. Admission mutations reject wrong
revision/source, ignored bounds, reordered rows and false completeness. Old
Rust tests and their Windows fixtures remain until reviewed replacement and
platform coverage supports an approved deletion batch.
