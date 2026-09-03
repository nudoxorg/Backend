# Card J9P — doclet reference-owner attribution for nested call arguments

## Registered role

`nudox_luna_implementer`. You implement one frozen proof card. You do not choose
product architecture, do not widen paths, do not change any proof-matrix row.

## Baseline and workspace

- Worktree: `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-lane`,
  detached HEAD at the merge commit `e29d1b73` (verify `git rev-parse HEAD`
  before the first edit; if it differs, STOP and report).
- Build environment for every gate:
  `CARGO_TARGET_DIR=/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-gate-target`
  and `NUDOX_JDK=/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  (`"$NUDOX_JDK/bin/javac" -version` must print 21.0.12.1).
- The worktree carries untracked foreign files (node_modules, package.json,
  python scratch under /tmp) and untracked java lifecycle scratch
  (`compiler/driver/tests/java_lifecycle.rs`, `compiler/driver/tests/java_lifecycle/`).
  NEVER stage, commit, revert, or reformat any of those. Stage only your three
  owned paths.

## Owned paths (exclusive write custody)

1. `compiler/languages/java/doclet/AuthorityImage.java`
2. `compiler/languages/java/tests/javac_authority.rs`
3. `compiler/languages/java/tests/fixtures/authority/src/demo/` (new fixture
   file(s) only; never edit existing fixtures)

## The law (established lane product semantics — do not re-derive)

Image reference rows attribute each method invocation to the **enclosing
declared executable** — the lexically enclosing method/constructor DECLARED in
a named type — never to a resolved callee, never to an executable of an
anonymous (nameless) class. Anonymous-class bodies have no declaration rows;
their invocations attribute to the enclosing declared executable. This is the
documented intent in `AuthorityImage.java` (visitClass comment) and the
recorded J2R law.

## The defect (observed on the real file Pair.java, commons-lang3 3.14.0)

`CallScanner.enclosingExecutable(TreePath)` walks parent tree paths and returns
the first element that is an `ExecutableElement`. A `MethodInvocationTree`
node's resolved element IS an `ExecutableElement` (the callee), so for an
invocation appearing as an ARGUMENT of another invocation, the walk returns the
outer call's resolved callee instead of the lexically enclosing method.
Ground truth (Terra-instrumented, real javac 21.0.12.1 doclet run on
`org.apache.commons.lang3.tuple.Pair`):

- `consumer.accept(getKey(), getValue())` inside `Pair.accept` emits
  `REF owner=SymbolRef(19=FailableBiConsumer.accept) target=SymbolRef(9=Pair.getKey)`.
  The `getKey()` call is owned by the FOREIGN callee `FailableBiConsumer.accept`.
- Same pattern for every argument-nested call (Objects.equals/getKey,
  CompareToBuilder.append chains, String.format args).
- Consequence in the driver: `push_occurrence` resolves `symbols.lookup(reference.owner)`
  against pushed declarations only → `ProjectionFault::OrphanOwner` → the
  whole source file folds to `NoSupportedDeclaration` and commons-lang3's
  Pair.java (and every file with an argument-nested call) cannot lower.

## Required fix direction

In `CallScanner`, the owner walk must only honor method DECLARATION nodes:
a parent path whose leaf is a `MethodTree` and whose element is an
`ExecutableElement`. Skip (continue the walk past) a candidate whose enclosing
element is a nameless (`getSimpleName().isEmpty()`) `TypeElement` — an
anonymous-class host. Keep returning null when no declared executable encloses
the call (field initializers, static/instance initializers behave exactly as
today: reference skipped). No wire-format change: the reference row stays
20 bytes, the owner cell just names the semantically correct executable.

## Proof (falsifiers you must make pass)

1. New live-javac proof in `javac_authority.rs` (JDK-gated like the existing
   test): add a fixture source under the owned fixture directory that contains
   a declared method whose invocation argument is itself a method invocation
   (e.g. `logger.log(level, format(value))` style with resolved foreign or
   local callees), plus a call inside an anonymous class body. Open the emitted
   image and assert, through public `JavaAuthorityImage` APIs only:
   - the nested argument call's `reference.owner` resolves via
     `image.symbol(..)` to the OUTER declared method (name/declaring-type atoms),
     NOT the callee;
   - the anonymous-body call's owner resolves to the enclosing declared
     executable of the named type.
   Assert exact atoms, not "any symbol".
2. Existing `javac_authority.rs` tests stay green unchanged (the first
   reference's target assertion must not regress).
3. The Rust-constructed fixture tests in `compiler/driver` (`--lib lower::java`,
   `--test java_image`) stay green — they build images directly and are the
   wire-compatibility guard.

## Semantic and resource bounds

- No new dependencies. No unsafe. No wire/geometry changes (section sizes,
  record widths, counts untouched).
- The doclet must still compile under the vendored producer's existing javac
  API usage style (no new imports beyond the tree-api types already imported;
  add imports only if unavoidable and name them in your report).
- No edits to `compiler/driver/lower/java.rs` or `compiler/driver/lower.rs`
  (the Rust-side OrphanOwner fold is the typed boundary for hostile images and
  stays).
- If the fix makes the nested-call case unrepresentable on the wire or you
  discover the owner cell cannot express the declared executable (e.g. symbol
  pool ordering), STOP and report the exact constraint.

## Exact gates (all green at your checkpoint)

1. `cargo test -p compiler-languages-java --test javac_authority`
2. `cargo test -p compiler-languages-java`
3. `cargo test -p compiler-driver --lib lower::java`
4. `cargo test -p compiler-driver --test java_image`
5. `cargo check -p compiler-driver -p compiler-languages-java --tests`
   (foreign-lane test drift in rust/typescript test targets is recorded and
   NOT yours; report it if it grows, do not fix it.)

## Commit and return

One commit on top of `e29d1b73`, message:
`fix(java): attribute nested call arguments to the enclosing declared executable`
Stage ONLY your owned paths. Return: commit hash; one-line output per gate;
diff LOC; the smallest remaining red row; any stop decision hit.
