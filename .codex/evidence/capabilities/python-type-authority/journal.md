# Research journal — python-type-authority

2026-09-02 | repo scout | Prior state in mandate was stale: pyrefly checker (2098 LOC), compound
lowering, occurrence tiers with pypi package keys, extension facts, docstring fragments already
exist in working tree (uncommitted). Mandate deltas reduce to: golden render tests, PURL
lifecycle integration tests, real-package fidelity tests, overload fidelity proof, edge-case
repairs. | decision: do not re-implement; prove, then fill.

2026-09-02 | environment probe | pyrefly 1.2.0 via uvx live; uv/uvx present; network to
pypi.org/simple live (HTTP 200); python3 present (RustPython shim). | decision: live
integration tests may fetch pinned artifacts; every external tool gate uses the typed-skip
pattern already established in the checker live tests.

2026-09-02 | shared-worktree hazard | clang lane's in-flight lower/clang.rs broke
compiler-driver builds during scouting; fixed upstream mid-session (build now clean).
| decision: commit only owned paths; never stage adjacent lanes' files.

2026-09-02 | fetch/unpack mechanism | wheels are zip; workspace deps include flate2 (gzip) and
ureq (HTTP) but no zip crate. | decision: sdist (.tar.gz) unpack via flate2+manual tar reading
or system tar; wheel unpack via system `unzip` with typed skip when absent — no new product
dependency; test-tree-only concern. Revisit only if unzip is unavailable on the gate host.

2026-09-02 | calibration (card-purl) | v1: reader exact; misreader found manifest overreach,
vendoring loophole, and a nonexistent pin (six@2.16.0 -> 1.17.0). Rewrote: five-line manifest
bound, sha256 lineage chain, real pin. v2: reader exact; misreader attacks reduced to
enforcement strategies (fake lineage, unconnected breach paths, in-memory compare) and worker
taste. | decision: capability scope held; enforcement is Terra's ingestion duty (mechanical
diff checks: lineage assertions present, breach paths call the happy-path downloader, gen-1
bytes re-read from store). Round-3 trials not spent on taste; residual discretion is
deliberate. Card digest de298972741c795d+4 edits, current digest recorded in commit.

2026-09-02 | independent hostile review (nudox_terra_reviewer) | NO APPROVAL: B1 tar `..`
traversal demonstrated end-to-end (lexically-passing starts_with); B2 pax `path=` override
was dead code (unconditional clear after each entry) while the journal claimed it worked;
B3 committed lane tests used unstable str_as_str and never compiled in any run gate;
M1 vacuous archive-digest inequality + no PyPI-declared sha256 pin; M2 PID-keyed fresh_dir
allowed cross-run store reuse; N1 erased compile cause; N2 committed debug test; N3 Alias
kind discriminated by incidental Option pattern (deferred design card); N4 unbounded JSON
read + no transport timeout; Q1 checker-skip visibility. | disposition: B1, B2, M1, M2,
N1, N4 fixed in python_support/python_purl_lifecycle (falsifiers: crafted-tar traversal +
pax-rename + pax-escape unit tests; PyPI-declared sha256 pin replaces the vacuous
inequality; unique fixture dirs); B3 + N2 fixed in lane file (stable borrows; debug test
deleted); N3 logged as next design decision; Q1 answered: this host has no PATH pyrefly,
only uvx — live-checker coverage runs only where uvx-provisioned.

2026-09-02 | real-package matrix (mandate 5) | 6-test matrix lands: every package's
PRIMARY module (requests models.py, attrs _make.py 86 top-level, flask app.py, six.py)
hits the 128-fact capacity terminal; largest compiling modules under the import root
prove real decoded-lane facts (attrs _funcs.py 38 entities, flask ctx.py 125 entities,
wcwidth 25). All failures funnel to escalation D1/D2 — production fix ownership is the
trunk lane's.

2026-09-02 | D1 root cause isolated (instrumented emission dump) | The ForwardReference
fault is NOT python emission: emission is compliant with the documented pooled-lane
protocol (children appended via anonymous_type_child BEFORE the owning row is interned;
instrumented run: Reader Protocol -> leaf interns anon 0-2, children appended to slots
0-2, FnPtr interned anon 3). The trunk's FactSet::intern_anonymous_type_row records
child_starts[index] = anonymous_children_total at intern time, but append-then-intern
children occupy slots [total - pending, total). Any row with pooled children therefore
serializes the NEXT rows' (unwritten) slots. Exact chain: FnPtr start=3,count=3 reads
stale slots 3-5 (zeros) -> remap turns zero into fact0+anonymous_rows(4)=4 ->
ForwardReference { ordinal: 3, position: 0, target: 4 } from PreparedFragment::prepare.
Both ignored python_render regression tests reproduce it; the three green render tests
pass because they assert the live compile_ir Ir, not fragment admission. TypeScript's
intern_computed_row uses the same append-then-intern protocol; C#'s intern_row uses the
inverse order (intern-then-append), which under the trunk bookkeeping misattributes child
counts to the following row — C# conformance is that lane's follow-up, out of this card.
Trunk protocol coverage in lower/tests.rs: zero rows exercise the anonymous child
sequence (grep), which is how the defect shipped in 3c1550502. | decision: one narrow
Luna repair card on lower.rs FactSet bookkeeping + un-ignore of the two python_render
regression tests; python.rs, compiler/ir, other frontends forbidden.

2026-09-02 | d1 card checkpoint 7bf08a760 ingested | Repair verified correct: instrumented
lane dump shows Mapping/Reader structural records, items/lookup Apply facts, callback
FunctionPointer, backward topology, prepare green. The two fragment regression tests now
run and expose two NEW truths: (1) test 3 expects the FIRST structural class to host
member rows — trunk shipped intern_reserved_anchor_type_row for exactly this and python.rs
still degrades it to the self-nominal (my lane's upgrade); (2) test 2 demanded Apply for
`callback: Callable[[int], str]` but the honest lane fact is FunctionPointer (stale
expectation). (3) compile_ir now fails at Build: live_type's FunctionPointer lift reads
`facts.names.get(child_target)` — pooled rows carry TYPE-row children (128+), so the lift
raises Dangling{Entity, 128+}; it previously survived on stale-zero children (garbage
labels from row 0 "Plain"). The child name cell is discarded in the same closure — the
label law should be: name cell first, else facts.names when target < fact_count, else no
label. Trunk surface (lower.rs live_type), one narrow card. (4) Live-Ir goldens in tests
1/4 froze the defective empty-tail/no-compound display; post-repair actuals captured via
probe: `fn overloaded(value: ?unsupported) -> str`, `static items: ?unsupported<?unsupported>`,
`static callback: fn(param: ?unsupported) -> str`, `static quoted: Plain` (quoted
annotation renders), choice: str unchanged; `?unsupported` is the honest spelling for
python int/None rows the live DAG cannot lift (D3 fork stands). | decision: accept
7bf08a760; card 2 = live_type label law (lower.rs); card 3 = python.rs reserved anchoring +
name cells + stale golden updates (my lane); goldens re-captured after card 2 lands.

2026-09-02 | d2 checkpoint aff5024a2 ingested | Label law generalized correctly; probes green.
2026-09-02 | d3 checkpoint d6909a85 ingested + Terra mechanical repairs | Reserved anchoring
verified correct by instrumented lane dump (Mapping anon rows 0-1 owner=0, record children
3:2; Reader anon rows 2-5 owner=1, record children 5:1; both AnonymousRecord; all
backward). Worker's committed code deviated upward from the card: it reserved-anchors EVERY
structural class to itself (uniform, legal, validates at each intern) — accepted as a
stronger representation of the same law. Two violations repaired directly by Terra
(smaller than a worker turn): (1) fabricated `b"param"` labels in emit_variable and a new
parent_row_named — invented display values that would also shadow real parameter names
under the name-cell-first priority; removed. (2) protocol_method_row name cells REVERTED:
the wire grammar itself rejects them (ChildNameForbidden { tag: FunctionPointer }) — my
card-3 law 2 was wrong against compiler-ir-vocabulary's validator; pooled method rows
carry unlabeled parameters, and the labeled display comes from the read function's own
fact-level FnPtr (facts.names of its parameter facts). The Import/Oracle tier assertions
in the fragment test were categorically wrong for the DETERMINISTIC syntax-only fragment
path; corrected to the honest Index-tier call occurrence; checker-provisioned tier
coverage stays on the live-Ir checker test and the real-package matrix (requests Foreign
keys). The alternate-source leak law was restored after the rust lane's c49845a3a kept
top-level non-liftable rows unlifted (`left` has NO semantic_type; display would never be
i32). Purl/packages test trees repaired directly (R9, mechanical, smaller than a worker
turn): ureq =3.4.0 moved timeouts to the agent config (transport() with 30s
timeout_global); ustar fixture size field is 12 bytes (124..136); pax record length
prefixes corrected to 26/31; fixtures wrapped in a gzip envelope (unpack requires it);
digest extraction now takes the LAST "digests" record BEFORE the sdist URL bounded to one
entry (PyPI lists wheels first; digests precede each entry's url); locate's return slots
aligned with callers. Gates: python_render 5/5; compiler-languages-python 35/35; purl 5/5
(+1 ignored, D2); packages 9/9. python.rs unit tests + lib tests blocked by ADJACENT
lanes' stale cfg(test) modules (clang.rs 21, rust.rs 10 errors) — not python surface.

2026-09-02 | d4 corpus checkpoint 66f2c461 ingested | Matrix now 20 real packages (10 test
functions, all green). 34 capacity observations recorded across the matrix (jinja2
sandbox.py, click parser.py, werkzeug formparser.py, packaging _spyx/lots, PyYAML
parser.py, tomli _re.py, markupsafe _native.py, itsdangerous timed.py, iniconfig,
etc.) — every one hits the frozen 128-fact lane and funnels to the standing D2
escalation with exact module names, strengthening the capacity fork's evidence base.
Layout diversity proven: src-distributions (idna, packaging, pluggy, click, jinja2,
markupsafe, werkzeug, tomli, iniconfig), package-dir (PyYAML), flat/single-module (six,
certifi, webencodings, pyparsing, colorama). Worker's reported render failures were a
stale binary on the worker side; Terra re-verified 5/5 at 66f2c461. compiler-application
(publication round-trip R7) is currently broken by the trunk's new
CompileFailure::ExtensionAtomUnbound not yet matched in application/terminal/native.rs —
adjacent-lane convergence in flight, not python surface.

2026-09-03 | trunk consumption (fidelity round 2) | Branch rebased at canonical 309acc8f1
(identical tip, no rebase needed). Landed since d4: geometry 128→1024, render int/None lifting,
journal chaining, application terminal arm (fcaeaa894), checker authority on the fragment path
(9be35faf8), per-fact child geometry 16 (19e2196d4). Terra repaired the trunk's missed
test-support mirror of CompileFailure::FactRejected (native_compile support.rs E0004) —
mechanical, smaller than a worker turn. Live baseline: packages 10/10 but FOUR primaries still
terminal at capacity (attrs _make.py fact 258 ChildCapacity — swallowed SILENTLY by a dead
match arm, pyparsing core.py fact 1024 Capacity, click core.py fact 105 ChildCapacity, jinja2
environment.py fact 107 ChildCapacity); 16/20 primaries fully lower with deep asserts; purl
lifecycles 8/8; authority 7/7; render 6/6; lower_facts 5/5; languages-python 35/35; application
2/2. Bounds are Rust-side only (lower.rs consts; wire ordinals u32; validator limits derive from
observed counts). | decision: one production card (geometry/admission with measured demand),
then corpus unfallback + deep-IR card, 4th package-class journey, and a cross-cutting shortcut
hunt; the silent attrs arm is mandate evidence of the dodge law.

2026-09-03 | round-2 execution | d5 (21eaea220), d7 (18241d2e), d6 (9e47e671 REJECTED —
SpotCheck::Entity re-labeled old pins; repaired 66d50740 PARTIALLY REJECTED — DocstringPrefix
unconstructed + 3 empty-sequence pins; final f2ed48e0e accepted), plus Terra mechanical
repairs: native_compile FactRejected mirror, click decorator pins corrected to real positions
(docstring artifacts removed), python child-boundary falsifier re-pinned 17→33/32 pair, rust
capacity falsifier re-pinned 1025→2049. All 20 primaries lower completely: pyparsing 1498
entities (was fact-1024 terminal), click 744 (was 105 ChildCapacity), jinja2 495 (was 107),
attrs 517 (was 258, silently swallowed). Deep review: pins survive adversarial source
comparison (markupsafe escape / itsdangerous dumps positional-only `/` verified in fetched
sdists). Shortcut hunt: no degradation comments/ignored tests in the lane; dead helpers
eliminated by module split; the pyrefly silent skip now prints.

2026-09-03 | CONTAMINATION INCIDENT (recorded as evidence-handling law) | The environment's
CARGO_TARGET_DIR points absolutely at the MAIN checkout (.local/target). Both worktrees share
package name+version, so artifact names collide; several of this session's first "green" runs
executed STALE artifacts from the main checkout (identical 1024-geometry terminals), and one
worktree build linked a GoFacts rlib carrying the other session's uncommitted fields (E0063
phantom). REMEDY: every gate in this round was re-derived with
CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-python/.local/target; all reported numbers come
from that isolated target. Concurrent-writer hazard: another session's uncommitted in-flight
edits (compiler/ir, server/journal, rust_traits, driver Cargo.toml) appear unstaged in this
worktree mid-round; they were never staged and every commit here stages owned paths only.

2026-09-03 | geometry decision recorded | d5's mechanism: raise the named lane constants
(1024→2048 facts, 16→32 children) — wire format unchanged (schema-1, u32 ordinals), FactSet
inline size law held at the chosen geometry, derived constants formula-based. The card's
original measurement-first demand was not met by the worker (blocked by the contamination
phantom); Terra accepted the outcome on the stronger evidence: all 20 primaries lower with
measured entity counts, and both capacity boundary falsifiers (32/33 python, 2049 rust) pin
the admitted bound honestly. pyparsing's 1498-entity module leaves ~27% fact-lane headroom.
