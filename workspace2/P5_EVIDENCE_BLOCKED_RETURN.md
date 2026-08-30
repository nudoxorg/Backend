# P5 canonical EVIDENCE_BLOCKED return

## Custody

- isolated repository: `/private/tmp/nudox-prototype-real-compiler-ir`
- branch: `codex/prototype-real-compiler-ir`
- clean evidence head before this packet: `f3d3cfdc26e7dfdee4d66ec0ed6145bb69a5dbf2`
- shared checkout: `/private/tmp/nudox-orchestra`, branch `orchestra-shared`; observed dirty at return
  from unrelated concurrent work and never edited, staged, committed, reset, or merged by P5
- toolchain: Rust 1.97.1 / LLVM 22.1.6 / `aarch64-apple-darwin`
- merge/product-score state: none

## Canonical two-attempt R22 receipt

Attempt one successfully created a fresh explicit non-inheriting Terra manager, which reproduced the
exact canonical checkout, branch, clean commit `38662cf5`, and card SHA-256
`4993af03338d0721f0df6ff9b357d0b7d9b7c9e9bbcca8868af1427744377ed1`. Its first required child call
was:

```text
task_name="r22_reader_one"
model="gpt-5.6-luna"
fork_turns="none"
```

Literal receipt: `collab spawn failed: agent thread limit reached`.

Attempt two materially changed the transport path: Sol reactivated the original canonical Terra
manager rather than allocating a new manager thread, then requested a genuinely fresh child from that
established tree:

```text
task_name="r22_attempt2_luna_cold_reader"
model="gpt-5.6-luna"
fork_turns="none"
```

Literal receipt: `agent thread limit reached`.

Neither role ran or edited. A later fresh type-DAG Terra manager independently reproduced clean
custody, committed its card, and received the same literal failure when requesting explicit
non-inheriting Luna `type_dag_cold_reader_a`. The unavailability is therefore not specific to the R22
manager instance.

Precise external unblock: the agent-runtime owner must release or enlarge this root task's fresh child
thread capacity. The next run must prove successful explicit `gpt-5.6-luna` and separate
`gpt-5.6-terra` non-inheriting spawn/transport identities; no role nickname or Sol self-review may
substitute.

## Closed evidence and retained counterexamples

| boundary | evidence | disposition |
| --- | --- | --- |
| C0-IR branded coordinates | independently reproduced clean at `461f802f`; actual-rlib E0308 and legal mutant | `RETAIN BASELINE` for this narrow terminal |
| C0-COMPILER manual closed dispatch | independently reproduced candidate `1a3675e4`; pointer/length input and subset failure | `PROMOTE FOR FUTURE INTEGRATION REVIEW` |
| C1 borrowed two-lane fragment | independently reproduced candidate `51398c29`; golden/mutation/pointer proof | `PROMOTE FOR FUTURE INTEGRATION REVIEW` |
| C1 prepared caller writer | direct R22 behavior, actual-rlib, two mutants, 43,153-byte raw custody, and two gates at `6bfb0cf4` | local facts clear; independent role chain `EVIDENCE_BLOCKED` |
| C1 primitive/reference DAG reader | direct decoder, actual-rlib, two mutants, 64,904-byte raw custody, and two gates at `f3d3cfdc` | local facts clear; independent role chain `EVIDENCE_BLOCKED` |

The DAG work preserved two deliberately rejected checkpoints rather than compressing code or inflating
budgets: combined writer/reader 389 lines at `41fbb3ff`, and product-bearing reader 278 lines at
`f9742596`. The accepted primitive/reference reader is 218/240 production lines and proves legal self,
forward, and backward recursion with non-owning typed coordinates. Full codegen honestly retains one
bounded 16-byte proof staging copy and two optimized bounds-panic call sites; no zero-cost or
panic-free claim is made.

## Untouched ordered chain and exact next owner/action

| stage | untouched observable work | precise owner/action after runtime capacity returns |
| --- | --- | --- |
| C1-PRODUCT | add ordered left/right edge positions without reopening primitive/reference decode | one fresh Terra manager freezes the product-only card; Luna calibrates/builds; separate Terra attacks first-bad left/right evidence |
| C1-TYPE-WRITE | prepare typed nodes and atomically emit exact `0xc2` bytes into caller output | next fresh Terra manager retains manual bytes as control and proves short-output atomicity plus constant/input-removal mutants |
| C1-ATOMS | canonical borrowed atom offsets+bytes with UTF-8 policy chosen explicitly | separate Terra manager compares raw bytes versus checked text view and proves pointer containment/truncation |
| C1-LISTS | pooled typed lists with exact range proof, no per-entity owner | separate Terra manager freezes empty/one/shared/boundary list terminal and caller scratch/high-water budget |
| C1-EXTERNAL | typed cross-fragment expected-kind references, distinct from local dense IDs | separate Terra manager proves unresolved/resolved semantics and compile-fail local/external substitution |
| C1-FRONTEND-CONTROL | synthetic adversarial frontend emits recursive types, atoms, lists, and refs directly | separate Terra manager selects two honest capability rows, applies constant-body/input-removal mutants, and closes full C1 corpus/resources |
| C2 | complete recipe keys, static stage graph, concrete driver, leases, cancellation, wanted/have skips, typed diagnostics/probes | fresh C2 Terra manager starts only after full C1 terminal; first card closes recipe sensitivity/insensitivity and exact skipped stage set |
| C3 | one measured real bounded frontend borrowing its native arena | fresh C3 Terra manager measures Rust-public-interface and OXC-TS candidates, selects one bounded subset, then proves direct lowering against an independent oracle |
| C4 | owner-thread vertical admission and real bounded sandbox adapter | fresh C4 Terra manager first closes physical credit conservation under cancel/crash; later card owns the execution adapter |
| C5 | signed/content-addressed compiler bundle and acquisition boundary | fresh C5 Terra manager closes manifest/object/signature/target/protocol rejection before download lifecycle and client text proof |
| C6 | published local/remote incremental consumer journey | final Terra manager composes only closed C0–C5 artifacts, restarts every publication prefix, and proves exact invalidation with compiler failure excluded from index correctness |

These are intentionally named as untouched. Vocabulary, a small decoder, or green local tests do not
claim recipe determinism, a real frontend, cancellation, vertical admission, isolation, acquisition,
publication, or index integration.

## Reproduction and resource summary

- Final direct C1 gates used two distinct fresh targets; each passed 20 tests, formatting,
  warnings-denied Clippy, diff checks, and an empty isolated status.
- Prepared-writer generated custody: 43,153/65,536 bytes, reserve 22,383.
- Reference-reader generated custody: 64,904/65,536 bytes, reserve 632.
- No dependency, unsafe, SIMD, serde, dyn, boxed error, process JSON, testkit crate, universal AST,
  per-entity owner forest, scheduler, sandbox, bundle, publication, or shared API was added.
- UNVERIFIED: independent fresh calibration/review for the two direct slices, Miri, fuzz/endurance,
  non-AArch64 targets, and every untouched boundary above.

## Terminal

No prototype verdict is admissible for the promised C0–C6 chain because C1 is incomplete and the
mandated independent topology cannot currently be instantiated. This is not product completion, a
merge recommendation, or a score.

`EVIDENCE_BLOCKED`
