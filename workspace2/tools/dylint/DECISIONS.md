# Semantic lint boundary

Status: implemented; exact UI fixtures and repository integration are mandatory gates.

## Toolchain authority

The lint library follows Dylint 6.0.4's generated library template exactly:

- Dylint crates are pinned to `6.0.4`;
- rustc internals are pinned to `nightly-2026-05-28` with `rustc-dev` and
  `llvm-tools-preview`;
- `clippy_utils` is pinned to the template's matching Rust-Clippy revision,
  `9fca3bc9fc2bc83c60bde26d18ed68f11564b228`.

This is intentionally separate from workspace2's stable shipping toolchain. The dynamic lint ABI is
not stable across rustc versions, so updating any one of these pins requires updating all of them and
re-recording every UI diagnostic.

The runner also installs `rustfmt` for that pinned nightly to check the isolated lint workspace. It is
kept out of the library's `rust-toolchain.toml` because Dylint's 6.0.4 build script requires the
template's exact two-component declaration; the additional installed component does not affect the
dynamic lint ABI.

Sources:

- <https://github.com/trailofbits/dylint/tree/v6.0.4/internal/template>
- <https://github.com/trailofbits/dylint#writing-lints>
- <https://rustc-dev-guide.rust-lang.org/diagnostics.html>

## First semantic bundle

The first bundle chooses four narrow rules whose meaning can be proven from HIR and type checking.

1. `NUDOX_ERASED_MAP_ERR` finds `Result::map_err` closures whose argument is `_` and whose body
   directly constructs or names the replacement error. It does not guess about an arbitrary method
   named `map_err`, does not inspect macro-generated code, and permits a helper call because packed
   validators may deliberately rescan canonical bytes to recover an exact cold diagnostic after a
   typed cast rejects. That call exclusion is a documented false-negative boundary; the helper's
   exact error behavior remains an integration-test and review obligation.
2. `NUDOX_STRINGLY_STATE_FIELD` finds fields named `step`, `expected`, or `observed` whose resolved
   type is an immutable string slice. It does not reject diagnostic message text or unrelated string
   data.
3. `NUDOX_REDUNDANT_PUBLIC_ACCESSOR` finds a public inherent zero-argument receiver method whose body
   only returns an already-public field of the same receiver. `ImplItemImplKind` supplies the
   compiler-owned inherent-versus-trait distinction, and rustc's associated-item metadata proves
   that the function has a `self` parameter. Required trait methods and associated functions are
   therefore excluded regardless of method or trait name. Private fields and transformed or
   validated projections are also excluded.
4. `NUDOX_DYNAMIC_DISPATCH` finds explicit trait-object types. Generic parameters, associated types,
   and closed enum dispatch are not flagged. An earned cold adapter or plugin boundary can use a
   narrow item-level `#[allow(nudox_dynamic_dispatch, reason = "...")]`. The reason must identify
   the erased boundary and its ownership or latency justification. The UI suite includes this
   intentional escape hatch next to a rejected unannotated trait object. This first bundle does not
   semantically reject broader suppression scope; module- or crate-wide suppression remains an
   explicit review tripwire until a separate attribute-scope lint has pass/fail fixtures.

Each rule ignores external macro expansions. Unit fixtures include a failing pattern and the nearest
allowed pattern so broadening a rule becomes an explicit review decision.

The repository invocation deliberately checks shipping library and binary targets, not test targets.
The UI suite owns lint behavior in isolation; ordinary test code remains free to use concise test-only
representations without weakening shipping policy. `shipping-workspaces.sh` is the single manifest
inventory used by both semantic linting and resolved normal-dependency checks. It names every current
workspace root:

- the root `crates/*` workspace;
- both adapter workspaces;
- the IR, compiler, and index workspaces;
- the layout laboratory.

Adding a nested workspace requires adding its manifest to this inventory. The lint implementation
workspace is tested by its exact UI suite and is not recursively linted by itself.

## Deliberate exclusions

- `panic!`, `expect`, `unwrap`, and `unreachable!` already have precise Clippy lints. Workspace policy
  should enable those rather than fork their semantic implementation.
- Public unit structs can be namespace objects, proof markers, domain markers, or inhabited values.
  A local lint cannot infer the intended role reliably without a declared annotation or whole-program
  consumer model.
- Manual `Display`/`Debug` can be boilerplate or deliberate formatting. A name suffix such as `Error`
  is not semantic proof.
- Tuple projections and numeric literals are meaningful in some algorithms and accidental in others.
  Linting them without a domain declaration would reproduce the regex false positives.
- An unbounded constructor token is not proof that a queue participates in a shipping hot path, nor
  can a spelling scan find re-exports or aliases. The mandatory law is owned by typed admission and
  budget APIs plus Loom transition tests that prove capacity is returned on every terminal path.
  Every inventoried shipping workspace currently has no `SegQueue` or `unbounded` constructor use;
  adding one therefore requires its concurrency proof to change visibly. A future semantic lint must
  identify a reviewed set of constructor `DefId`s and include bounded, adapter-only neighbors before
  it can become a reliable gate.
- `.ok()?` and `filter_map(Result::ok)` are not intrinsically silent corruption: both are legitimate
  while projecting optional diagnostics, but forbidden while validating canonical data. Exact
  validator and projector integration/property tests own the mandatory no-omission law by asserting
  every malformed element produces its typed error and index. Review owns the boundary declaration.
  Until validator roles are represented in types or attributes, a source or HIR spelling lint would
  silently confuse optional projections with integrity validation. Removing the regex therefore
  removes false confidence, not the exact behavioral obligation.
- Source length, word count, and parameter count are not design properties and receive no replacement
  gate. Control/data complexity remains under Clippy's cognitive-complexity lint and human review.

Repository scans remain appropriate for resolved dependency bans and the explicitly reviewed unsafe-file
allowlist: those are cross-file repository facts. Rust semantic patterns belong to the compiler-backed
lint bundle.

## Quality-gate migration

The old shell rules were classified by the evidence they actually supplied:

| Former check | Disposition | Reason |
| --- | --- | --- |
| trait-object spelling | Dylint | HIR identifies trait-object types through aliases and formatting |
| `map_err(\|_\| ...)` spelling | Dylint | type checking distinguishes `Result::map_err`; direct replacement is separated from a cold diagnostic helper |
| `step/expected/observed: &'static str` | Dylint | resolved field type and semantic field name are available |
| public getter-name list | Dylint, narrowed | only a method returning an already-public field is provably redundant |
| panic/expect/unwrap/unreachable spellings | Clippy | the existing compiler lints are more complete; `expect_used` and `unreachable` are now enabled |
| serde spelling | resolved dependency graph | the product constraint is presence in normal shipping dependencies, including renamed imports |
| unsafe spelling | retained repository scan | rustc denies unsafe by default; the scan additionally proves that local exceptions remain in the two reviewed files and carry written safety evidence |
| forbidden normal dependencies | retained `cargo tree` scan | only resolution can prove package presence on normal edges |
| unit structs, manual formatting, constructors, numeric literals, and tuple projections | deleted | the text patterns conflated valid roles with violations; each needs a future semantic rule with pass/fail neighbors before enforcement |
| `SegQueue` / `unbounded` constructor spellings | deleted; boundedness proof retained | current shipping sources contain no such constructor; admission-budget APIs and Loom transition tests own bounded progress, while any future lint must resolve reviewed constructor definitions rather than tokens |
| `.ok()?` / `filter_map(Result::ok)` spellings | deleted; no-omission proof retained | exact validator/projector integration and property tests own malformed-element reporting; syntax alone cannot distinguish integrity validation from a legitimate optional projection |
| `todo!`/`unimplemented!` spelling | Clippy | both compiler-backed lints are already denied |
| complexity-lint suppression spelling | deleted | `allow_attributes_without_reason` remains compiler-backed; a future suppression policy must inspect resolved attributes, not source text |
| crate-root line count | deleted without replacement | source volume is not architecture; module ownership and control/data complexity are reviewed directly |

There is intentionally no line, word, or parameter-count gate. Clippy cognitive complexity remains as
control-flow evidence. `too_many_arguments` remains explicitly allowed because arity alone is not
coupling evidence.

## Gate custody is not lint inference

`.codex/skills/steward-greenfield-rust-program/scripts/run_clean_gates.py` remains because it answers a
different question: whether the exact Git commit and tree stayed clean while a named command vector ran,
and whether the immutable logs and summary retain hashes, timestamps, exit status, and candidate identity.
It does not infer Rust quality, count source, or replace Dylint. The Python layer is evidence custody;
the compiler and Dylint own semantic diagnostics.
