# Card L6a-r2: bounded type lowering with retained diagnostics + honest foreign references

registered role: nudox_luna_implementer (expected `luna`/max; resume of the
L6a session)
baseline: a7845f839 (your L6a result) on top of 309acc8f1. The lane-local
target dir is now MANDATORY for every cargo command:
  export PATH=/opt/homebrew/bin:$PATH
  export NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules
  export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-typescript/.local/target
Rationale recorded in index.toml: the ambient CARGO_TARGET_DIR is shared
across lane worktrees; a sibling lane's stale compiler_ir artifacts produced
phantom-field compile errors (GoFacts/constant_flags) and one false
NoSupportedDeclaration. Never build this lane against the shared target.

## Terra diagnosis (evidence, reproduced in the worktree — verify, don't trust)

1. **Unbounded mutual recursion.** `Projector::lower_type` /
   `child_target` (compiler/driver/lower/typescript.rs ~886-990): both
   forward `depth` UNCHANGED; only `lower_type`'s transparent-wrapper arms
   increment it. On real declaration files a span that resolves back to an
   ancestor (self-referential alias/interface shapes — e.g.
   `type ParseResult<R> = R & { errors: ParseError[] }` cycles) makes the
   depth guard unreachable and the thread overflows. Reproduced: the
   lifecycle compile of @babel/parser's entry dies in `lower_type` ↔
   `child_target` frames even under a 512 MiB thread stack (sampled).
2. **Quadratic span probing.** `lower_type` linearly scans ALL semantic
   nodes per call (`for node in semantic.nodes().iter()`), so every child
   lookup rescans the whole table — quadratic in the file's node count.
   Babel-parser-sized files make this pathological. Fix the lookup
   mechanism (e.g. a span→node index built once per projection), not by
   capping work silently.
3. **Diagnostic-erasing fold.** The collect's error conversion
   (typescript.rs ~line 60) folds per-fact causes into
   `LoweringUnsupported::NoSupportedDeclaration`. Reproduced: @babel/parser's
   `type Plugin = "asyncDoExpressions" | ...` (~40 members) exceeds the
   per-row child geometry and surfaces ONLY as NoSupportedDeclaration —
   the exact construct, member count, and row are erased. That violates the
   exact-operand law (deliver-reviewed-rust-slice: errors retain the exact
   original operands).
4. **Foreign declared references dropped.** `intern_computed_reference`
   (~2784-2846) lowers any module-qualified reference to
   `unknown_record(TypeReason::NoIrRepresentation)` claiming the lattice has
   no foreign row form — but `NominalRef::External(ExternalEntityRef)` and
   `ConcreteType::External`/`Applied { constructor }` exist, so local
   `Holder<number>` keeps Apply while `Map<string, number>` decays to
   Unknown(Unsupported). Verified by probe: `table`'s declared annotation
   decodes Unknown(Unsupported); the transcript HAS declared
   reference(Map<string,number>).

owned paths: compiler/driver/lower/typescript.rs, compiler/driver/lower.rs
(TypeScript surface only), compiler/driver/tests/typescript_lower.rs.
Everything else forbidden (checker.rs, main.cjs, render tests, lifecycle
files, compiler/ir/**, publication/**).

## Required behavior

1. **Monotone-depth recursion.** Every recursive edge that re-enters
   `lower_type` must strictly increase the depth (pass `next_depth`
   everywhere), so `MAX_TYPE_DEPTH` is reachable on every path; at the
   bound the existing honest `TruncatedAtDepthLimit` record applies.
   Falsifier (new test): at least one self-referential shape
   (`type A = A | false; export const x: A = ...;` and a
   `ParseResult`-style alias-parameter cycle) must lower successfully with
   a typed truncation record at the cycle — run the test body inside
   `std::thread::Builder::new().stack_size(2 * 1024 * 1024)` (default-class
   stack) to prove no overflow WITHOUT env help.
2. **Member-overflow truth.** `TypeCells::push_child` overflow (union/
   intersection member count beyond the per-row child geometry) must
   produce a typed rejection retaining: the construct kind, the exact
   member count, and the construct's source span — and that typed cause
   must survive to the public CompileFailure (never folded into
   NoSupportedDeclaration). Fix the fold: NoSupportedDeclaration stays
   ONLY for the genuine zero-supported-declarations case.
3. **Member census honesty for oversized unions.** Choose and implement
   ONE mechanism so every member of an oversized union survives in the
   decoded IR: either (a) nested union rows (union of unions; associativity
   preserves semantics; document that render/display may flatten) or
   (b) a raised geometry with the exact new memory bound named in the
   constant's doc comment. Falsifier: the `Plugin`-shaped source
   (`type Plugin = "a" | "b" | ... ` with 40 literal members) must decode
   with 40 members reachable in the type DAG (count them in the test).
   Whichever mechanism: no member silently dropped, no fabricated rows.
4. **Foreign references are typed facts.** Module-qualified references
   lower to `Nominal` rows carrying `NominalRef::External` (bare) or
   `Applied` over an external constructor row (with arguments), retaining
   the module-qualified spelling via ExternalTarget's atoms; `NoIrRepresentation`
   stays only for constructs the closed lattice genuinely cannot express.
   The Array/ReadonlyArray special case keeps its behavior. Update the
   frozen table to source truth: row 4 `table` declared = Apply shape 3
   (constructor + 2 args) — assert the constructor resolves to an external
   target whose display names Map; row 14 `term` computed updates to the
   reconstructed external nominal per the fragment half's assertions.
   Keep schema-1 transcript decode green.
5. **Span lookup cost.** Replace the per-call full node scan with a
   mechanism that does not rescan all nodes per child (build the index once
   per projection pass). The perf truth shows up in the corpus card (R9);
   here, just don't leave the quadratic scan behind.

## Exact focused commands (with the lane-local CARGO_TARGET_DIR above)

- cargo test -p compiler-driver --test typescript_lower
- cargo test -p compiler-driver --test typescript_render  (14/14)
- cargo test -p compiler-driver --test typescript_authority
- cargo test -p compiler-driver --test typescript_package
- cargo test -p compiler-languages-typescript
- cargo check -p compiler-driver --lib

## Bounds and stop decisions

- No compiler/ir edits (ExternalTarget/NominalRef/Applied APIs as they
  exist are sufficient; if you find they are NOT, STOP and report the exact
  gap — Terra escalates).
- No new dependencies, no vocabulary changes, checker.rs/main.cjs untouched.
- rustfmt clean; no unwrap/expect in production paths; the bounded
  subprocess discipline (R11) stays green.
- Two checkpoints allowed: (1) recursion bound + typed member overflow +
  span-index mechanism; (2) foreign references + frozen-table source-truth
  updates. Commit each after its focused falsifiers, prefix
  `fix(typescript):`.

## Checkpoint protocol

Return: commit shas, focused command outputs, the self-referential
falsifier's decoded record (tag + reason), the Plugin-shaped falsifier's
member count as decoded, the row-4/row-14 updated assertions verbatim, and
the smallest remaining red row.
