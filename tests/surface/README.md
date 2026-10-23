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
T2 runs as an isolated parallel consumer in CI and as the explicit integration
command locally. Smoke and integration have separate verdicts. Consumers verify and
import the exact artifact runtime before entering the pinned surface shell;
their checkout provides harness/configuration only, with no product build.

## Bounded case parallelism (W3.3)

One invocation runs two independent cases concurrently by default. Use `--jobs 1`
for a serial diagnostic or explicitly choose up to four workers, reusing the same
published binaries and existing task resource limits:

```sh
python3 .config/ci/test.py integration --bin-dir DIR --jobs 2
```

Each indexed case keeps its own project, owner, workspace and credentials.
Duplicate scratch names fail before any case launches. The CLI-contract runner
uses shared scratch paths, so its small group is serialized. A failed case does
not cancel or hide successful peers; the tier passes only if every selected
case passes. Each completed case is compared and reported immediately; a slower
earlier catalog entry cannot hold back a completed peer verdict.
Reported case wall time measures execution, excluding time waiting for a worker.

In CI, after acquiring the pinned runtime/tool shell and before starting cases,
the runner drains pending writes on the task filesystem with Linux `sync -f`.
This is infrastructure setup, reported as `CI-TIMING ... filesystem-drain`, and
remains inside the overall T2 deadline. It attempts to clear runtime-import journal writes before the first owner's
30-second startup budget. Build1840 exceeded the drain bound, so this has not
resolved shared-filesystem contention. Product fsync
calls, startup bounds, snapshots and performance limits remain unchanged.
A failed or timed-out drain fails the run; it cannot produce a passing case.

`SURFACE-PERFORMANCE` lines in the retained CI event log carry schema1 JSON with
the case name and full measured timing/CPU/memory data. The report file remains
available while the task container exists. Answer snapshots and performance
limits are unchanged by selecting a worker count. Robert's October16 milestone is
built, wired and fast with honest case verdicts; green and cross-host performance
calibration are the next phase. Contention failures remain failures.

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

The `cli-usage` case runs 18 selectable scenarios against the exact published
CLI: root/add help, 14 typed refusal cases (including misspelled command,
unknown option, missing coordinate and alias coordinate, out-of-range and
unsupported page bounds, and invalid passive search/compile intent), plus
absent-owner passive health/status. Assertions retain
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
and structured usage refusals respectively; successful responses, endpoint failures
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

### Real background operation resumption (W1.3)

Polyglot setup runs the installed `index` alias with background execution intent
and `--wait --wait-timeout 0`. The first caller must return exit 75 with a known
pending operation. A fresh `index_progress --operation-key` process observes that
same request, then a fresh `add --execution-intent background --wait` resumes it.
The key, request digest, exact package and execution intent must remain unchanged;
all seven native profiles must publish complete coverage. Fresh CLI lookup,
explicit same-key `index_start` replay and MCP `backend.index_progress` must preserve the full terminal
receipt and source capture. A completed Add acknowledges its journal claim, so a
new unkeyed Add is a new request; idempotent replay uses the explicit key.
Existing health checks bind the visible product to
that receipt, and existing search/outline snapshots remain the correctness oracle.

The `cli add` setup timer includes the initial start, progress and resumed wait;
individual phases and terminal lookups are also reported. Golden answers and
performance limits are unchanged. This proves caller deadline/resumption with a
real owner, not owner crash recovery, ticket cancellation, or process-local ticket
awaiting. The targeted scripted transport-retirement test remains valuable.

### CLI wait-option admission (W1.3, CLI-02.01)

Four additional independently selectable usage cases run against the published
CLI: timeout without wait, timeout above 900, negative timeout, and wait on search.
Each requires exit64 and the typed usage fault naming the offending option and
its actionable diagnostic. The existing sentinel and isolated paths also require
no owner launch, workspace state or endpoint directory. Add help must advertise
wait, its timeout operand and terminal-receipt semantics. Together with #233's
real index-alias/zero-deadline/resumption proof this covers the CLI-facing portion
of CLI-02.01; MCP argument rejection and the targeted Rust boundary test remain.

### MCP rejects CLI-only wait options (W1.3, CLI-02.01)

After native indexing completes, the shipped MCP process receives three invalid
`backend.index` calls: `wait`, `wait_timeout`, and `wait-timeout`, each supplied
separately alongside the valid project path. Each must return an error tool
result with a structured usage/malformed fault naming that exact argument and
explaining that it is unknown. Transport errors, missing replies and successful
indexing responses fail the case. A valid progress call in each same session
must still return the original complete operation identity and native receipt.
These checks add per-session timings without changing the reviewed golden or
performance limits. They do not prove absence of all filesystem writes or replace
the retained targeted failure, disconnect and journal laws.

### First pinned whole upstream project (W2.3)

`go-xxhash` indexes the complete `github.com/cespare/xxhash/v2` v2.3.0 module,
including its original Go module and source layout. The surface shell exposes
`NUDOX_SURFACE_GO_XXHASH` from a fixed-output Nix fetch using the existing fleet
pin in `.config/nix/corpus-pins.json`; it does not fetch the rest of the fleet,
vendor a rewritten fixture, run upstream package scripts, or choose a moving tag.
The archive hash is `sha256-FFomzcfEnbVmAXuAfEmJ7n993rVpQj6cuZ+ZX6w2IdM=`.

The case waits for complete native publication, then snapshots real CLI/MCP
search, outline and package answers. Before admitting a new snapshot, each surface
must find upstream-authored `Sum64String` in `xxhash_unsafe.go` and `Digest` in
`xxhash.go`, with Go language identity and ready state. Unrelated/nonempty results cannot satisfy
those witnesses. Missing pinned project authority fails rather than skipping.
The existing resumable seven-language case remains unchanged; this smaller real
project uses a direct blocking wait because a zero-deadline call could legitimately
finish such a small project before the first observation.

Run only this case with:

```sh
python3 .config/ci/test.py integration --bin-dir DIR --case go-xxhash
```

This case caught active `Sum64String` losing its source location beside its
build-excluded namesake. Source pairing now prefers the compiler's primary-source
entities when they cover the entire structural group; ambiguous incomplete groups
still keep no guessed location. The authored source witnesses remain unchanged.

The first golden requires explicit source/result review and a second exact-head
comparison. This is the first real-project slice, not the planned full per-language
representative corpus.

Initial source/result review (Concourse 480531 / pr-t1/597, `cf1aad6ef`):
`Sum64String` is a ready Go function at `xxhash_unsafe.go:38`, and `Digest` a
ready Go struct at `xxhash.go:29`, on both CLI and MCP. Search records agree;
continuation tokens keep their distinct surface formats. Default outline pages
are explicitly truncated (CLI 200 declarations, MCP 100); this snapshot does
not claim exhaustive outline traversal or removal of source-less package-context
rows. Initial indexing took 189.73s, owner CPU 187.26s and owner peak RSS
379,308KiB. This is a first baseline, not a calibrated fleet-wide performance
claim; representative-corpus scale and indexing efficiency remain open.

The first comparison (pr-t1/599, `06daafdca`) kept the exact native view root
and declaration answers, but opaque continuation proof bytes varied between
fresh owners. Snapshot admission consumes every offered cursor through the
appropriate public client before replacing its spelling with
`<live-validated>`. Real CLI/MCP search and outline chains are walked through the
terminal page before snapshot comparison. The harness rejects refused tokens,
cycles, empty/non-progressing pages, repeated emitted identities, scope drift,
outline basis changes and dishonest continuation flags. Outline identity uses
the full selected-symbol ID carried by the DTO; a repeated ancestor is allowed
only when it contains newly emitted descendants. MCP outline continuations stay
in their issuing session, while search cursors use the public clients. The
bounded CI evidence retains each page response without its opaque cursor bytes,
linking pages with cursor SHA-256 values. A proof is bound to its CLI answer or
MCP call ordinal, so an identical token on another request must be traversed
independently. The reviewed answer golden remains the first page.

The general outline proof cap remains 256 pages. `cpp-pugixml` explicitly allows
512 because build 599439 consumed 256 valid pages and 7,876 unique declarations
while the admitted page still honestly offered a cursor. Its issued CLI limit is
200, but output admission deliberately narrows large encoded frames to 25
declarations while retaining the original page credit and immutable basis. The
case-specific cap corrects proof capacity for that admitted delivery size; the
unchanged 300-second query deadline still bounds the complete walk.

For token-bearing answers only, the normalizer validates the reported byte/token
arithmetic, then records `bytesExcludingCursor` and
`estimatedTokensExcludingCursor`. It subtracts exactly the opaque token's UTF-8
length; payload size, token ratio and all semantic/basis/budget fields elsewhere
remain compared. Raw encoded token length is intentionally not a stable golden
contract. Refusal and normalization controls run in `test_surface.py`.

MCP outline can shrink its first page to fit the response budget. Continuation
copies the returned `page.limit`, package project path and detail, as instructed
by the public response; reusing an omitted/original limit correctly gets refused.
The admission controls cover that client behavior too.

Search continuations are consumed by fresh CLI/MCP clients. Outline continuation
certificates are retained by the issuing MCP session, so the interactive JSON-RPC
driver keeps that session open, reads the real first response, then sends the
returned token and effective operands on the same process. It does not claim
cross-session portability for outline. A stateful subprocess control verifies
that exchange independently of the native binary; real T2 verifies the product.

The MCP `Digest` request explicitly uses limit 3, so its complete readable preview
fits comfortably alongside the portable proof. CLI keeps its larger default
page. The full preview text remains snapshotted; the harness does not discard
it to hide adaptive clipping noise. Native pr-t1/606 proved all three offered
continuations, but its default-limit preview varied by one row as proof lengths
changed, motivating this bounded request.

The interactive driver validates MCP whole-line byte accounting against the
actual received JSON-RPC line, plus reported estimate arithmetic and byte limits.
For token-bearing replies, snapshots compare canonical normalized frame bytes
and estimates with their own counters omitted; all limits/policy fields remain.
The `estimatedTokens` field is a bytes-per-token estimate, distinct from the
product's cl100k token-policy admission. Changed-candidate logging uses the
existing `--print-missing` flag for review and still fails the comparison.

Revised review (pr-t1/609, `6b05a3c81`): all CLI answers and the other MCP
payloads match the initial reviewed result. MCP Digest is exactly the CLI's
first three records, with all three coordinates in its complete readable
preview. All three offered continuations passed their live checks and wire
accounting passed. Indexing was 189.84s, owner CPU 187.16s and owner peak RSS
381,068KiB. The independent comparison against this reviewed candidate is the
merge gate; no tolerance was changed.

## Whole Python project: Blinker 1.9.0 (W2.3)

The second upstream project is the complete MIT-licensed PyPI source distribution
of Blinker 1.9.0, pinned by the existing fleet archive URL and SHA-256 in
`.config/nix/corpus-pins.json`. The selected Nix fetcher extracts the full archive,
including tests, docs and packaging metadata; it does not install the package,
execute upstream scripts, vendor sources or fetch the rest of the legacy corpus.

Blinker exercises a Python `src/` package, classes and inheritance, type-annotated
functions, weak references, async methods and test/documentation sources.
CLI and MCP must find ready Python `NamedSignal` in `src/blinker/base.py` and
`make_id` in `src/blinker/_utilities.py` before snapshot admission. Full native
profile completion is required; structural-only or missing-source results fail.
The shared harness records search/outline/packages answers, validates offered
continuations, and captures indexing/query wall time plus owner CPU and peak RSS.

```sh
python3 .config/ci/test.py integration --bin-dir DIR --case python-blinker
```

Initial native source/result review and an independent exact-head comparison are
required before accepting the new golden. This is a second real-project slice;
it does not complete the representative per-language corpus or GUI coverage.

Initial source/result review (483413 / pr-t1/663, `9db7f5e91`): full native Python
publication root `9cf9af16cf6c39eb75f46e066b1b78d6c0ca7d3d202dc5c7db782df9606dbadd`.
CLI/MCP both find ready Python `NamedSignal` as a struct at
`src/blinker/base.py:464` and `make_id` as a function at
`src/blinker/_utilities.py:42`, verified against the pinned source. MCP search
records equal the first three CLI records. Packages names the ready project
`blinker`. Source-less context/import rows remain in the golden; embedding-based
search remains explicitly unconfigured. CLI/MCP outlines are honestly truncated
at 200/100 declarations; this case does not claim exhaustive outline traversal.
Both offered MCP continuations (NamedSignal search and same-session outline)
passed real native consumption before normalization, with full wire accounting
and readable previews retained.

Initial indexing was 3.30s, owner CPU 1.82s and owner peak 279,160KiB. This first
performance sample is provisional. The independent exact-head comparison must
match the reviewed answers and the existing performance guards before merge.

### Hybrid-core performance calibration (W2.5, 2026-10-14)

After #249, normal artifact-backed runs measured the same Go answers/root with
indexing 23.18–42.11s and owner CPU 16.36–31.27s. The first two fast samples alone
were too narrow for a stable baseline. Same-artifact diagnostics in the failed
663 task isolated logical CPU 0 (max 4.8 GHz) versus 12 (max 3.5 GHz): owner CPU 16.24s
versus27.55s with identical answers/root. Only these diagnostic subprocesses were
pinned; production scheduling and worker capabilities remain unchanged. Their
single-CPU wall times 37.81/62.58s also constrain the Go compiler children and
are not normal unconstrained job timings.

Use the slower **normal** 663 sample for Go index 42.11s, owner CPU 31.27s and
first source search 3.65s. Keep the 1.5x time/CPU ratios, existing floors, memory
baseline and every answer unchanged. This still rejects the pre-optimization
189.84s/187.16s behavior; it accommodates the CPU-class sensitivity observed with
identical artifacts. Cross-host variability remains uncalibrated, and the tiers
remain advisory until the separately approved required-check cutover.

The same Python artifact's independent CPU 12 run matched every reviewed answer,
including normalized continuation accounting. It measured index 4.56s and owner
CPU 3.14s (versus normal 663 sample 3.30s/1.82s). Use 4.56s/3.14s as the initial
Python index/CPU baseline; keep its original memory and query baselines. No
ratio, floor, source witness or public answer was changed. Final ordinary
exact-head CI must still pass both real projects and the unchanged polyglot case.

Two-worker experiment (484083 / pr-t1/679, `5ea548357`) reduced T2 consumer
wall time from128s (serial676) to85s. All public answers, memory and owner CPU
matched, but the Go cold query exceeded its existing wall limit. A second run
(484136 / pr-t1/681, `afe2be041`) also exceeded polyglot/Python indexing wall
limits (45.42s/10.18s), despite CPU21.41s/2.11s remaining within limits.
The experimental Go query baseline increase was reverted. These historical trials
precede Robert's October16 two-worker rollout: all performance gates remain
unchanged, and contention failures stay visible for the get-it-green phase.
The historical timings are not a current 16-case speed guarantee.

## Whole Rust source project (W2.3)

`rust-percent-encoding` fetches the complete MIT OR Apache-2.0 crates.io source
archive for percent-encoding2.3.2 using the existing reviewed fleet pin. It keeps
Cargo manifests, all source files, tests and docs. No upstream scripts are run
and sources are not rewritten to remove dependencies. Independent empty-cache
`cargo metadata --offline --format-version 1` succeeds with the full resolve
graph, so this project does not require installing upstream dependencies.

The native public contract must publish one complete Rust profile and expose
ready authored structs `PercentEncode` (`src/lib.rs:142`) and `AsciiSet`
(`src/ascii_set.rs:28`). CLI searches, packages, bounded outline and corresponding
MCP search/outline replies are compared using the same reviewed projection and
live continuation checks as the existing real projects. A missing native symbol
or partial/refused publication cannot become a new golden. Native review and an
independent exact-head comparison are required before this slice is complete.

Initial Rust snapshot review: Concourse484294/pr-t1/686, source5d4999f8b.
The unchanged synthetic/Go/Python cases matched; the new case deliberately failed
for a missing reviewed golden after all native/source/transport guards passed.
Native complete Rust root is
`df9e741237da30ee5a2f758eeece5c8bbb7f6404664c6d284af70bb78b0875fd`.
Reviewed ready structs exactly match upstream PercentEncode at src/lib.rs:142
and AsciiSet at src/ascii_set.rs:28. CLI returned4/8 search records; MCP's bounded
first3 match CLI and both offered search continuations were consumed by fresh
MCP clients. Complete public outline contains125 declarations and the ready
package is percent-encoding. Source-less semantic instantiations/imports remain
in the full compared result as returned (some language=unknown); no claim that
every returned record is an authored source declaration.

Initial native baseline: index19.38s, owner CPU19.46s, peak799588KiB; first
PercentEncode query1.07s, AsciiSet0.04s, outline0.07s, packages0.02s, MCP0.21s.
Existing1.5 time/CPU and1.25 memory ratios/floors stay unchanged. Independent
exact-head comparison of this golden and every old case is required before merge.

Rust repeatability uncovered a product ranking bug (484403/pr-t1/689 on
5b408366b): unchanged complete native roots/rows but tied source-less types
changed rank and MCP page composition. Local lexical tie order used provider
EntityId, which is namespaced by the workspace. Both controlled and ordinary
query paths now break equal placement/relevance ties by immutable canonical
RowId, keeping provider identity and provenance/semantic ordering intact.
No sorting/score normalization was added to the harness.

Native fix review:484553/pr-t1/692 on e70b1e52f. All existing polyglot/Python
answers and every performance guard passed. Rust's complete4/8 search row sets
are unchanged; canonical tie order/ordinal scores and readable MCP previews
change. Go Sum64String's complete18 rows are unchanged; Go Digest remains an
honest bounded25-row page with continuation. All sourced Digest records,
packages and complete outline are identical; four source-less ready/unknown
function instances exchange at the25-row boundary as the tie order changes.
They are not a claim of authored declarations or exhaustive Digest coverage.
Fresh continuations/CLI–MCP prefix checks still pass. Revised Go/Rust answer
snapshots retain every prior performance baseline and tolerance. Rust measured
20.91s index/20.10CPU/825204KiB; a fresh exact-head comparison is required.

## Real-project expansion candidate (W2.3/W2.4)

The catalog now proposes ten whole pinned upstream projects: existing xxhash,
Blinker and percent-encoding, plus the seven below. Native execution, reviewed
snapshots and performance calibration are required before this expansion merges.
New cases must fail when their snapshot is absent; CI cannot accept its own output.
TypeScript expansion is deferred while #259's product bug is open.

| Project | Existing archive pin | Independent source checks / selection reason |
|---|---|---|
| cachetools | PyPI7.2.0 | TTLCache and cached in src/cachetools/__init__.py: inheritance, cache policies and decorators. |
| tomli | PyPI2.2.1 | loads and TOMLDecodeError in src/tomli/_parser.py: parser functions, exceptions and typed inputs. |
| itsdangerous | PyPI2.2.0 | URLSafeSerializer and Signer in separate source modules: class hierarchies and module boundaries. |
| itoa | Cargo1.0.14 | Buffer and format in src/lib.rs: public types, methods and integer formatting. |
| ryu | Cargo1.0.18 | Buffer and format_finite in src/buffer/mod.rs: module layout and floating-point formatting. |
| uuid | Go1.6.0 | NewString in version4.go and ParseBytes in uuid.go: exported declarations across files. |
| x/sync | Go0.11.0 | Weighted in semaphore/semaphore.go and DoChan in singleflight/singleflight.go: packages and concurrent APIs. |

Archives and byte hashes come from .config/nix/corpus-pins.json; fetchers preserve
the complete original project. Required manifests/source files are checked while
unpacking. Archive LICENSE files remain with each project, including itoa/ryu's
multiple license files. No generated module is inserted to make an old upstream
archive appear compatible. The initial Go errors candidate was rejected because
its archive lacks go.mod; x/sync replaces it.

The tomli case also proposes one GUI content proof on Linux. The same indexed
owner and published desktop binary open the real project; actual X11 keyboard
input searches TOMLDecodeError and opens its code page. Native window pixels are captured
and read by pinned English OCR. The observer requires the project name and
original-source text `class TOMLDecodeError` and `ValueError`, then a clean Quit.
A typed search query alone cannot pass. The current window PNG/text remain under
/tmp/nudox-surface/python-tomli/gui; CI emits its source/artifact-bound image digest.
No in-memory fixture renderer or extra product build is used.

GUI wall time, process CPU and peak RSS are recorded separately from the owner;
existing tolerance ratios/floors also apply to the GUI measurements. OCR/Xvfb
observer cost is outside those product process metrics. Linux native execution
and screenshot review are pending; parsing/unit admission controls alone do not
prove the GUI works.


The first expansion run489752/814 produced source-reviewed CLI/MCP answers for
cachetools, tomli, itsdangerous and GoSync. Their first measured samples seed
candidate baselines without changing any existing band; repeat calibration is
required before merge. See reviews/2026-10-15-new-projects.json. Rust itoa/ryu
project admission and GoUUID connection failures remain selected and unresolved.
The GUI probe now uses the independently working tomli index; its new GUI answer
and performance snapshot still need native execution and screenshot review.


Rust cases retain offline owner policy. Their original Cargo.lock checksums feed
Nix importCargoLock; per-case writable Cargo homes point to immutable vendored
sources outside the unchanged upstream project. This supplies pinned dependencies
without permitting network fallback or editing project source/lockfiles.


Four further whole published-source candidates cover C (rxi/map), C++ (pugixml),
Java (JSR305 sources jar) and C# (TinyIoC source NuGet). All reuse fixed corpus
pins and preserve complete published archives; original declarations/files were
inspected and native Nix unpack checks pass. These bring the candidate catalog
to14 real projects across7 languages; TypeScript remains last. Native answer
review, snapshots and performance calibration are still required for these four.
A fetched source archive alone is not passing integration coverage.


Native GUI observation490171 showed the admitted fixture as `project` (the
actual copied folder name), with281 declarations from4/4 source files. The
observer now expects that real UI label, then still requires the distinct
TOMLDecodeError result filename and original class/ValueError source text. The
initial wrong-label timeout is retained as a harness failure, not a product pass.


Native replay490240 proved itoa CLI/MCP after pinned offline dependency setup,
but its snapshot is still a candidate. Ryu now passes Cargo resolution and fails
source-scope admission for its bench; C map publishes but lacks authored source
locations; pugixml partially publishes; JSR305 fails native authority startup;
TinyIoC lacks complete-profile proof. These failures stay selected for diagnosis.
GUI490240 showed an empty Ask field after window-only keyboard focus. The
observer now clicks the actual Ask field and checks that text input arrived
before checking results/source, preserving the original source-content proof.


### Java JSR305 initial Linux snapshot (W2.3/W2.4, 2026-10-15)

Focused Linux build495175/30 at94cfc4d9674dec41af0f1f04f0ffab8a2f6abe0c
completed all31-source native indexing and CLI/MCP functional checks. The task
failed only for its missing reviewed snapshot. An independent source/result
review confirms the qualified annotation names, exact source paths/lines,
Java/Ready rows,126-declaration complete outline and31non-module roots. Both
offered MCP continuations were consumed by fresh sessions. The snapshot
honestly retains unavailable/unconfigured semantic-search coverage and lexical
pathless name-lane records; those are not fabricated native source hits.

See reviews/2026-10-15-java-jsr305.json. Initial owner measurements are
67.25s indexing,5.87CPU seconds and276252KiB peak RSS. These are a first Linux
sample, not calibrated bands or official T2 acceptance. A repeat exact-head
comparison and calibration are required before merge. No existing snapshot,
tolerance or floor was changed. Prior Linux28 had a compiler-owner refusal,
so a single successful run does not establish stability.

The Linux34 exact-head comparison (build495627, source `a453e3ff`) returned the
same reviewed Java answers on both repeats. Indexing measured68.28s and71.69s;
owner CPU was5.69s on each, with peak RSS278052KiB and280980KiB. MCP measured
2.23s on repeat1, failing the unchanged1.29s baseline ×1.5 (1.935s limit), then
1.32s on repeat2, which passes. This is one failure and one pass on the same
head and binaries, so Java performance remains unstable. Build34 failed overall;
official T2 distinct-head passing streak remains0. No snapshot, band, tolerance,
or floor changed. Raw measurements and task status are in
`docs/testing/runtime/2026-10-15-java-blinker-linux34.json`
and summarized in `reviews/2026-10-15-java-jsr305.json`.

### Blinker external identity review (W2.4, 2026-10-15)

Focused Linux34 at a453e3ff976da05cd951747de3d678a5dd8fd342 captures the
canonical external-target identity change also observed in automatic851/855.
External keys are scoped to package and the exact semantic image digest;
source/image changes can change those keys and derived revision/count fields.
Independent recursive review confirms the same answer set, authored names,
source paths, readiness, coverage, kinds, row order and MCP response content
apart from those exact IDs and mechanically derived byte/token counts.

The golden retains the actual new IDs and counts. No runtime normalizer changed.
Its existing performance suffix is preserved byte for byte; the new14.86s index
and5.37s NamedSignal search remain rejected against unchanged4.56s/.74s
baselines. See reviews/2026-10-15-blinker-external-identities.json. This is a
reviewed functional answer update, not performance acceptance or T2 green.

### W2 Python image-identity refresh (2026-10-15, Linux40)

On exact c36f478da, two fresh-owner rounds return identical Cachetools and
ItsDangerous answers. Recursive comparison with the source-reviewed goldens
changes only image revision/source-root; Cachetools also changes one external
TTLCache image identity and its derived byte/token counts. Authored source
locations, names, kinds, readiness, coverage and ordering remain identical.
The answers-only refresh retains each existing performance suffix verbatim;
current timing failures still fail. See
`reviews/2026-10-15-coverage40-python-identities.json`. This is functional review,
not accepted performance calibration or full T2 green.

## TypeScript whole-project case (W2.3)

`typescript-query-core` selects the pinned MIT `@tanstack/query-core@5.62.8`
archive from `.config/nix/corpus-pins.json`. The Nix projection retains the
complete tarball and checks its package manifest, the authored `QueryClient` and
`CancelledError` source files, and license. CLI and MCP cases exercise search,
outline, and package views through the existing surface harness. The case covers
this published package, not the full TanStack monorepo. Each authored-symbol
assertion requires the expected name and exact source path to be ready, so a
same-named result from another file cannot satisfy the search. The case uses the
existing direct TSZ default; `COMPILER_TYPESCRIPT_COMPILER` remains a separate
oracle. Exact-head indexing,
answers, and performance remain unvalidated by this source candidate.

### C rxi/map initial Linux snapshot (W2, 2026-10-23)

Reviewed build1807/global594865 on exactee8d0c27 against the complete pinned
map.c/map.h sources and an independent Clang syntax-only AST. Both transports
retain the authored cleanup/lookup functions and header declarations/macros.
The outline also retains compiler result carriers, explicitly distinguished
from authored local variables; no source location is invented for them.
The25-row CLI and50-row MCP pages remain bounded with validated continuation.
See reviews/2026-10-23-c-rxi-map-initial-linux.json for scope and evidence.
The first Linux sample (add2.47s, CPU2.37s, RSS335836KiB) seeds this new case's
baseline; no existing baseline or tolerance changes. Fresh exact-head CI
comparison is still required; this review alone is not a passing T2 verdict.

### Go answer reviews (W2, 2026-10-23)

The UUID and Sync answer-only updates retain every byte of their existing
performance sections. Exact answers from Linux T2/1807 repeat in diagnostic
build596340 using the frozen ad0cde836 T1 artifact. Independent Go AST review
checked all queried source anchors and every name across the complete diagnostic
outlines (455 UUID rows, 371 Sync rows), including real module filenames and
documented positional signature carriers. These are source/result reviews;
official exact-head T2 and its unchanged performance guards still must pass.
See the two dated Go review JSON files for provenance and scope.

### Java annotation answer review (W2, 2026-10-23)

The repaired Java annotation capture produces identical answers in official
T2/1844 and diagnostic596497 on4fdf93137. The answer-only golden update was
reviewed against all31verified source-jar files using an independent Javac AST
reader; visible source sites, qualification and semantic carriers are explained
in reviews/2026-10-23-java-jsr305-linux-answers.json. The old performance section
is retained byte-for-byte. This is not an official passing T2 comparison.

### TinyIoC initial Linux snapshot (W2, 2026-10-23)

The first C# golden uses T2/1835's measured answers and performance, after
source-site/native publication checks succeeded. T2/1844 repeats its answers
exactly. An independent Roslyn parse of the verified pinned source confirms
all12visible source anchors, including conditional class-modifier start lines.
The dated TinyIoC review records scope, generated semantic carriers, both
measurements and the remaining fresh exact-head comparison requirement.

### Itoa answer review (W2, 2026-10-23)

Itoa answers repeat exactly in T2/1807 and1844. The answer-only review checks
queried sites against the verified crate and an independent Rust AST, and
checks generated benchmark/test names against actual pinned-rustc macro
expansion. The dated review distinguishes source declarations from compiler
carriers. Original performance bytes remain unchanged; fresh T2 is required.

### Polyglot method-kind contract (W2, 2026-10-23)

Diagnostic596768 on frozen4fdf93137 returns `language` as a C++ method at
`src/main.cpp:4` and Java method at`src/Duke.java:2`, exactly matching the
authored fixtures. The bounded-search precondition now requires `method`;
its former `function` filter rejected both real methods. The two required
source paths/languages, exact lowercase name and all ranking/completeness,
continuation and live-health guards remain in place.

### Corrected C-family stability and xxhash reviews (W2, 2026-10-23)

Fixed artifact9819cef0c produces identical C++ answers across two diagnostic
597028runs and officialT2/1851. The first C++ golden uses1851's actual baseline;
26source-backed sites were independently checked with Clang ASTs. The C-map
followup changes only three derived root/revision fields after corrected macro
storage facts; all other answer fields and original performance bytes match.
xxhash answers also repeat exactly; eleven source anchors and all visible names
were independently checked against the verified Go module. The xxhash baseline
is retained byte-for-byte. Dated reviews record scope and proof; fresh official
exact-head comparison remains required.

### Tomli answer and GUI content review (W2, 2026-10-23)

The dated Tomli review checks all5visible source anchors against an independent
AST from the verified pinned archive and records1851's actual GUI pixel/OCR
source-content evidence. The original performance suffix remains byte-for-byte
unchanged.1844's intermittent GUI source-content failure remains unresolved and
the full content guard/deadline remains enabled; accepting truthful1851answers
does not count as fixing that failure or as an official green comparison.

### Polyglot methods and retained GUI failure pixels (W2, 2026-10-23)

Official1862 polyglot answers match frozen981 diagnostic597289 exactly.
All10queried authored declaration/method sites match the seven tiny fixtures;
independent compiler/syntax AST checks and semantic-carrier qualifications are
recorded in the dated review. Existing performance bytes remain unchanged.
The diagnostic had different TS environment overrides, so equality was checked
against the actual official answer before adopting this candidate.
GUI failures now retain bounded already-captured native pixels and OCR in
CI-GUI-FAILURE; they neither extend the deadline nor replace the original error.
