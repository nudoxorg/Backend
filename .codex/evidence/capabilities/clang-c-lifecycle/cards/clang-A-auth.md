# Card clang-A-auth — make the direct libclang authority honest for anonymous, template, and duplicated cursors

- Registered role: `nudox_luna_implementer` (this environment: `luna` subagent, effort max)
- Baseline: worktree `.local/worktrees/clang-lifecycle`, branch `luna/clang-lifecycle` @ `f1beb8b75`
- Card digest recorded in index.toml at dispatch

## Owned paths

`compiler/languages/clang/**` — collect.rs, ffi.rs, facts.rs, scratch.rs, lib.rs, tests/, Cargo.toml
is READ-ONLY (no dependency or feature change).

## Forbidden surface

`compiler/driver/**`, `compiler/ir/**`, `compiler/ir-vocabulary/**`, every other language lane.
The worktree carries other lanes' uncommitted work: never stage or commit any file outside the
owned paths; explicit paths only, never `git add -A`.

## Public terminal

The authority's fact lists are honest for three cursor classes, proved by new live-authority
falsifiers against libclang on this machine:

1. Anonymous record fixture `struct { int x; } point;`
   Required: exactly ONE record fact with `name: None` (anonymous; Definition), its Field `x`
   (owner = the record's identity), and the Variable `point`. The record must NOT be pushed
   twice (current tree pushes it twice), and no fact's `name` may hold a declaration keyword
   (current tree yields `name: Some("struct")`).
2. Template fixture `template<typename T> struct Box { T value; };\nstruct User { struct Box<int> box; };`
   Required: a Template declaration fact for Box (Definition, name `Box`); a TemplateParameter
   fact `T` owned by Box's identity; the pattern's Field `value` owned by Box's identity; the
   Record `User` with its Field `box` named exactly `box`. Required ABSENT: every implicit or
   injected phantom record (the current tree yields a phantom `Record "Box" Definition=Declaration`
   and a `Field` whose name bytes are `"struct"` with the field's extent truncated to its type
   spelling).
3. Regression guard: the existing live tests
   (`c_authority_retains_macro_include_docs_recursive_types_and_local_calls`,
   `cxx_authority_keeps_overload_identity_and_template_type_edges`,
   `cxx_authority_retains_virtuality_and_deduplicated_overrides`) stay green, as do the crate's
   boundary and unit tests.

## Mechanism constraints (hard)

- libclang only, via the existing `ffi.rs` runtime-loaded `clang_3_6` surface plus symbols
  already loaded there. `clang_Cursor_isAnonymous` is OFF the surface (`clang_3_7` feature);
  the Cargo feature set and dependency list must not change. Candidate discriminators that are
  on-surface: `clang_getCursorSpelling` emptiness, the spelling-name-range relationship to the
  cursor extent, `clang_getCanonicalCursor` (already approved) for declaration dedupe,
  `clang_getTemplateCursorKind`, closed `CXCursor_ClassTemplate*` kind constants, USR identity
  comparison. If you can prove NO on-surface mechanism distinguishes one required case, STOP
  and report the exact case — that is a stop decision, not license for a feature bump or text
  scanning.
- No source-text scanning or keyword tables: a name is honest because libclang names it, never
  because a byte pattern looks like an identifier.
- Facts stay bounded: template traversal consumes the same scratch lanes; capacity stays an
  exact typed error. Template parameter and pattern facts must not bypass `DeclarationFact`'s
  closed shape.

## Required evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-languages-clang --offline --features native-test` → all pass,
   including the three new falsifiers (one per class above).
2. `cargo test -p compiler-driver --offline --test clang_lane 2>&1 | tail -20` → report the
   pass/fail counts; the known-red set may not GROW beyond
   {mutual_recursion_collapses, enumerators_and_typedef, signatures_and_local_call,
   template_parameter_is_pooled, anonymous_struct_names, include_atoms_share,
   doxygen_ref_is_a_local_link, macro_definition_and_invocation}; any new red is a STOP.
3. `cargo fmt --check` on owned paths; zero new warnings.

## Checkpoint

One commit, explicit paths, message `fix(clang): keep the authority honest for anonymous and
template cursors`. Report: commit sha, the three falsifiers' exact assertion bodies, both
command tails, and which on-surface discriminators you used per class.

## Plan closure

Next decision after return: re-dispatch card clang-A2-repair (projection onto the frozen
harness) on top of this authority, or issue one falsifier-bound repair card here.
