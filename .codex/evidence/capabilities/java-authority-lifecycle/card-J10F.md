# Card J10-F — emission fact-lane width 1024 -> 2048

registered role: nudox_luna_implementer
baseline: the current tip of branch codex/fidelity-java in
/private/tmp/nudox-fidelity-java (Terra updates this line at dispatch; do not
rebase onto anything else). Create your own detached worktree from it.
expected config: max effort; house style (`deliver-reviewed-rust-slice`).

## Baseline facts (Terra-scouted, backtrace- and corpus-verified)

With the doc-lane raise (J10-D) integrated, Terra's full corpus journey gets
past every earlier fold and ArrayUtils.java now fails with the typed,
operand-bearing fact-lane rejection:

`... ArrayUtils.java: compile failed: CompileRecipeFact {...} rejected emission
fact Fact...` (the `CompileFailure::FactRejected` arm — `RejectedFact` with
exact ordinal, name, and `FactFault::Capacity`).

Measured fact demand at the 1024 lane (Terra source census + live image
declarations):
- `ArrayUtils.java`: 394 image declarations + ~750 parameter carriers +
  fields/package/class roots ≈ 1,140+ facts > 1024.
- `StringUtils.java`: 258 declarations + ~533 carriers ≈ 790 facts — fits.
- Every other commons-lang3 file fits.

`compiler/driver/lower.rs`: `MAX_EMISSION_FACTS: usize = 1024` documents
itself as the emission lane bound ("a source with more declarations is a
typed lane rejection, never a truncated emission") and every derived budget
(`MAX_EMISSION_ATOMS`, `MAX_TYPE_ROWS`, `ANONYMOUS_ROW_BASE`,
`COMPUTED_ROW_BASE`) is an expression over it — no other production constant
embeds 1024. The wire side (`compiler/ir`) carries no 1024 constant; the
trunk itself raised this lane 256 -> 1024 in c8a24c743 with the same
re-derivation discipline.

`Compiler/driver/lower/tests.rs` capacity laws are written against the
CONSTANT (exact-bound admit, one-past rejection), so they adapt
automatically. The pre-existing trunk-owned red in that file
(`bounded_fact_and_child_lanes...`, `EmptyPath`) predates this card — Terra
reproduced it at this card's baseline; do not fix it, do not touch
`lower/tests.rs` except, if and only if that specific test starts failing
DIFFERENTLY because of the raise, an exact re-pin with a why-comment.

## Public terminal

`MAX_EMISSION_FACTS` becomes 2048. Every derived budget scales; sources up to
2,048 admitted facts lower completely; fact 2049 is still the exact typed
`RejectedFact { fact, name, cause: Capacity }` with the would-be ordinal.

## Owned paths

- `compiler/driver/lower.rs` — the constant and its doc comment (state the
  measured 1,140-fact maximum and the rationale in one line each).
- `compiler/driver/lower/java.rs` — ONLY if a java lane test pins a
  fact-count literal; every pinned count must become constant-derived with a
  why-comment (the lane's existing capacity falsifiers already use the
  constant).
- No other file. Forbidden: occurrence lane (1024), anonymous rows (2048),
  computed rows (1024), doc lane (16384), reference lists (64), wire crates,
  the doclet, vocabulary.

## Proof matrix rows bound to this card

- P32 (new): "A source demanding between 1,025 and 2,048 facts lowers
  completely." Falsifier (RED at baseline — verify and record): a
  programmatically generated java fixture image with 1,100 single-parameter
  methods (distinct names) lowers to a validated fragment with exactly 1,100
  admitted executable facts plus their carriers; at the baseline it fails
  with the typed `RejectedFact { fact: 1024, ... }`.
- P33 (existing laws stay exact): the java capacity falsifier still rejects
  fact 2048 (one past the new bound) with exact operands; every existing
  `lower::java` test stays green; golden/fixture bytes for small fixtures
  are unchanged (if any bytes change, stop and report — that would falsify
  the "identical under the old bound" law).

## Environment

- `CARGO_TARGET_DIR=<your-worktree>/target-wf` (dedicated; never share).
- `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  for the lifecycle leg.
- The machine is under heavy load from concurrent lanes: expect slow builds;
  use generous timeouts and never kill a gate for slowness — report instead.

## Exact commands (all must be green before commit)

1. `cargo test -p compiler-driver --lib lower::java`
2. `cargo test -p compiler-driver --test java_lifecycle -- --test-threads=1`
3. `cargo test -p compiler-driver --test java_render --test java_image`
4. `cargo check -p compiler-driver --tests`
5. `cargo fmt -p compiler-driver && git diff --check`

(The shared `lower` suite retains the known pre-existing trunk-owned red in
`lower/tests.rs` named in the card header; outside your custody.)

## Budgets and house laws

- Production delta <= 10 LOC; test delta <= 90 LOC.
- No unwrap/expect/panic; no ignored tests.

## Checkpoint and return

Commit once, coherent, on a branch `j10f-fact-lane` in your worktree. Return
exactly: commit hash + branch; files changed with net LOC; P32/P33 outputs
(test names + pass lines + the recorded baseline-red line for P32); whether
any golden/fixture bytes changed; smallest remaining red or "none"; any
deviation with one-line justification.
