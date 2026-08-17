# docs/AGENTS-DOCTRINE.md — the standing contract for every agent on this program

You are working inside `backend`, a multi-language code-intelligence system.
Read this file **before** touching anything. It is short on purpose. Everything
in it is enforced at review.

---

## 0. The one-line mission

A local-first GUI (`lindsey`, GPUI) renders documentation for real packages in
any of seven languages, streamed from a local engine over a typed wire protocol,
with working hyperlinks, real search, and real version lineage — and it must be
*visibly better* than docs.rs.

---

## 1. The dependency law (never violated, no exceptions)

```
lindsey (GUI, gpui)
   ├─→ nudox-engine
   │      ├─→ nudox-graph ─┐
   │      └─→ nudox-store ─┴─→ nudox-ir
   │             └─→ nudox-producer ─→ nudox-producer-{rust,go,java,python,typescript,clang,csharp}
   ├─→ nudox-mcp ─→ {nudox-engine, nudox-graph}
   ├─→ nudox-embed ─→ nudox-engine (capability port: Embedder)
   └─→ heart[client] ─→ NudoxClient (remote index::server, additive to nudox-engine)
```

- `lindsey` may **depend on `nudox-engine`, `nudox-mcp`, `nudox-embed`, and
  `heart`, and no other backend crate**. Its *source* may `use` neither
  `nudox-ir`, `nudox-store`, nor `nudox-graph`; doing so is a review-blocking
  finding, because it would let a *view* depend on the shape of the *IR*
  instead of on the protocol.

  **This rule previously read "may name `nudox-engine` and nothing else", and the
  word "name" was doing work it could not support.** `lindsey` has always had a
  transitive Cargo edge to `nudox-ir` and `nudox-store` — they sit at depth 2
  under `nudox-engine`, which is unavoidable and was never the concern. So the
  rule could never have meant "no edge in the dependency graph"; it only ever
  meant "no direct dependency, therefore no import". Stated that way it is also
  *checkable*, which the old phrasing was not.

  `nudox-mcp` is admitted because it sits **beside** `lindsey` as a second view
  of the same `EngineHandle`, not beneath it: the surface `lindsey` touches is
  `McpHost`/`McpStatus`, whose vocabulary is `EngineHandle`/`SocketAddr`/`String`
  — no IR type crosses the seam. It is what lets the application actually host
  the MCP server (docs/LIMITATIONS.md L35); without it that server exists and is never
  started.

  **Enforced by** `workspace/gui/tests/dependency_law.rs`, not by review. It
  parses `workspace/gui/Cargo.toml` and fails if any dependency section names a
  backend crate outside the allow-list, and separately scans every `.rs` under
  `workspace/gui/src` — with comments stripped, because the GUI legitimately
  *discusses* `nudox_ir` in doc comments where it mirrors a frozen wire enum —
  for a real import. A prose rule that only a reviewer can apply is the shape
  this codebase keeps finding rotted claims in (§8).
- **Capability ports: the engine names a trait, the host supplies the runtime.**
  Decided 2026-08-09, when semantic search needed an embedding model and the
  obvious move was an edge `nudox-engine → workspace/registry`.

  That edge is **not** taken, and the graph above is unchanged: `registry` is
  still not in it. What was added instead is a second *port* on `nudox-engine`,
  beside the one that already existed:

  | port | trait | host supplies | `None` means |
  |---|---|---|---|
  | highlighting | `highlight::Highlighter` | a tree-sitter runtime | uncoloured code |
  | embedding | `semantic::Embedder` | an ONNX/API model | `SectionState::Unavailable` |

  The rule to apply when the next one comes up: **if the engine needs a
  capability whose implementation would drag a runtime into its dependency
  graph, the engine owns the trait and the decision of *when* to call it; the
  host owns *how*.** The trait must be object-safe and must speak only in `std`
  vocabulary, so that satisfying it never requires naming the implementor's
  types. `Embedder` traffics in `Vec<f32>`, `usize` and `bool` for exactly that
  reason.

  Three things made the direct edge worse than it looks, and they are worth
  recording because each one is invisible until you try it:

  1. **`registry::vector::core::embed::Embedder` is not object-safe.** It has
     `type Model: EmbeddingModel`, so there is no `dyn Embedder`. The engine
     would have had to be *generic over the model*, putting a registry type
     parameter into `EngineConfig` and `EngineHandle` — and therefore into
     `lindsey`, which is the one crate §1 exists to keep clean.
  2. **Cost, measured**: `registry` resolves to 325 crates on its default
     features and **483** with `onnx`. `nudox-engine` is the single crate
     `lindsey` depends on; a 483-crate graph beneath it is not a detail.
  3. **`ort-sys` downloads a prebuilt runtime during `cargo build`** unless
     `ORT_LIB_LOCATION` points at a local install — and as of 2026-08-09
     nothing in this tree sets it (zero hits in `flake.nix`, `.cargo/config.toml`
     and every `package.nix`). An unsolved provisioning problem must not be able
     to break `cargo check -p nudox-engine`.

  **Where an adapter lives.** Between the port and a runtime there is always a
  ten-line adapter, and it belongs to neither crate. The one that exists today
  is in `workspace/registry/tests/vector/engine_relevance.rs`, reached by a
  **dev**-dependency `registry[dev] → nudox-engine`. A dev-dependency appears in
  no consumer's graph — `driver` and `index` are untouched, and so is
  `nudox-engine` in both directions — so it creates no edge that ships and needs
  no amendment here. A *production* adapter should be a crate that sits
  **beside** the engine on the `nudox-mcp` argument (its seam vocabulary is the
  port's, and no IR type crosses it), not beneath it.

  **Update 2026-08-12: the production adapter now exists — `crates/nudox-embed`.**
  It promotes that ten-line bridge (`FastembedOrt` → `nudox_engine::semantic::
  Embedder`) to a shipping crate that sits beside the engine, exactly where the
  paragraph above said one should. Its public surface is `SharedEmbedder`
  (`Option<Arc<dyn Embedder>>`) plus a `&str` env-var name — the port's own
  vocabulary — so no IR type and no vector-plane type crosses it, which is the
  same test `nudox-mcp` passes. `lindsey`'s `main.rs` now calls
  `nudox_embed::load_from_env()` and hands the result to `EngineConfig::embedder`,
  so `nudox-embed` joins `nudox-engine`/`nudox-mcp` on the §1 allow-list
  (`workspace/gui/tests/dependency_law.rs`, amended in the same commit). The
  `registry`/`onnx` graph enters a build **only** behind `nudox-embed`'s
  default-off `onnx` feature (and `lindsey`'s `-F semantic-onnx`), so a default
  or CI build pulls neither the 483-crate graph nor `ort-sys`'s build-time
  fetch, and `cargo check -p nudox-engine` is untouched in both directions.

  **What this deliberately still defers**: the `ort-sys` runtime is provisioned
  by its own build-time download unless externally supplied, and the fp32 model
  (~641 MB) is operator-supplied via `NUDOX_EMBED_MODEL_DIR` and verified
  against a pinned sha256 before load. Both are gated off by default rather than
  solved in-tree — an honestly-scoped deferral, not a paper-over. A default
  build's semantic section still reports `Unavailable`, and that remains an
  honest state visible in a type rather than a zero-hit result — see
  docs/LIMITATIONS.md L41.
- **`heart` — a fourth allowed backend dependency, one-line justification:
  transport-free client seam.** `heart` is the shared wire vocabulary
  `index::server` and `heart::client::http::NudoxClient` both speak; `lindsey`
  depends on it (with `features = ["client"]`, which is what pulls `reqwest`
  in — bare `heart` carries no transport) to reach a configured `nudox-serve`
  instance for the omni-search remote-results section
  (`src/views/omni_search.rs`), additive to `nudox-engine`'s local-first
  results. The surface crossing the seam — `NudoxClient`, `heart::query::Query`,
  `heart::{Scored, Symbol}` — is the server's own already-lowered wire
  projection, not an IR type, so this is the same argument `nudox-mcp` and
  `nudox-embed` already establish above. Allow-listed in
  `workspace/gui/tests/dependency_law.rs`'s `ALLOWED_BACKEND_DEPENDENCIES`.
- **None** of engine/graph/store/ir/producer may link `gpui`. The whole protocol
  must be testable without a window.
- `workspace/gui` is a standalone package with its own lockfile. That is
  deliberate — it keeps gpui out of the backend dependency graph. Do not add it
  to the root workspace `members`.

---

## 2. Errors are design feedback, not chores

This is the single most important rule on this program.

When you hit a compile error or a test failure, you must first ask:
**"what weak abstraction let this be expressible at all?"** Fix *that*, then the
error disappears as a consequence.

Worked example from this repo — `<P as Producer>::Id cannot be sent between threads`:

- ✘ **Bad fix**: add `where <P as Producer>::Id: Send + Sync` to the one function
  that failed. The next producer author hits the identical error somewhere else.
- ✔ **Good fix**: `Producer::Id` gains `Send + Sync + 'static` on the *trait*,
  with a doc comment explaining that producers run on `spawn_blocking` threads so
  their errors must be able to travel home. Now the failure lands on the producer
  *definition*, with a clear reason, instead of on a distant call site.

Apply that test to every fix you make. If your fix is a cast, an `unwrap`, an
`allow(...)`, a `.clone()` to dodge a borrow, or a bound bolted onto one call
site, stop and look one level up.

**Never** silence a diagnostic you do not understand. `#[allow]`, `_ =`, and
`unwrap()` in non-test code all require a comment justifying why the thing they
suppress cannot happen.

---

## 3. Typed contracts

- Illegal states must be unrepresentable. Prefer an enum over a `bool` + comment;
  prefer a newtype over a bare `String`; prefer a typestate over a runtime check.
- No stringly-typed anything crossing a module boundary. IDs are newtypes.
- Every `pub` item carries a doc comment that says *why it exists*, not what it
  literally does. `/// Gets the name.` is noise; delete it or replace it.
- Errors are `thiserror` enums with one variant per *distinct recoverable
  situation*, each carrying the context needed to act. A variant whose message is
  `"failed"` is a bug.
- `#[non_exhaustive]` on every public enum that a future producer or language
  could extend — **with one deliberate exception, decided 2026-08-05.**

  `ProducerError` is intentionally **exhaustive**. It is internal to this
  workspace, not a semver-stable public API, and the whole value of that is that
  adding a variant *breaks every downstream match at compile time* and forces
  each one to decide what the new failure means. `#[non_exhaustive]` would
  require a `_` arm in `nudox-store`'s mapping — which §6 lists as a weakening,
  and which would silently route a future failure mode into whatever generic
  bucket happened to be there.

  The rule to apply: `#[non_exhaustive]` protects *external* callers you cannot
  recompile. Inside one workspace, an exhaustive enum is the stronger contract.
  Do not "fix" `ProducerError` by adding the attribute.

---

## 4. Tests

- Test names state the *invariant*, not the mechanics:
  `search_returns_local_hits_before_semantic_work_starts`, not `test_search_2`.
- Prefer `rstest` cases/fixtures over hand-rolled loops when a test varies one
  axis. Prefer `proptest` when the invariant is universally quantified.
- **Every integration test is also a benchmark.** Wrap the measured region in
  `nudox_test_support::measured(case, dir, || …)`. The emitted `cost case=… ` line
  is parsed into the report — a test that does not emit one is invisible.
- Adversarial tests are required for every protocol: empty input, one element,
  duplicate keys, cancellation mid-stream, out-of-order events, unicode,
  pathological nesting. A protocol with only happy-path coverage is untested.
- Never assert on a message string where you can assert on a typed variant.
- A test that would pass against a stub is not a test. Assert on *content*
  (a symbol that really exists, a link that really resolves), never just on
  `is_ok()` or a count being non-zero.
- **A hand-authored fixture tests the fixture author's imagination, not the
  code.** The Go producer had 47 passing tests over embedded JSON fixtures and
  crashed on the *first* real package — and on every real package, because its
  type lowering called `refer()` unconditionally for named types while
  `Lowering::finish` requires every `refer`'d id to be declared. Any package
  importing `sync`, `io`, or `time`, or merely using bare `error`, was
  unlowerable. No fixture had ever contained a reference to a package outside
  itself, so the producer was 100% green and 0% usable. If a producer's tests
  are all fixtures, you do not know whether it works — prefer one real package
  over fifty synthetic ones, and treat "all our tests pass" from a fixture-only
  suite as an unmeasured claim.

---

## 5. Scope discipline

- Touch **only** the files you were assigned. If a fix requires editing a file
  outside your scope, say so in your report and stop — do not reach across.
- Do not reformat, re-order imports, or "tidy" code you were not asked to change.
  It destroys review signal and causes conflicts with parallel agents.
- Do not add dependencies without saying so explicitly in your report. New
  workspace deps go in the root `[workspace.dependencies]` and are referenced as
  `{ workspace = true }`.

---

## 6. Reporting

Your final message is consumed by a program, not read by a human. Return exactly
what you were asked for, and:

- State what you changed, file by file, one line each.
- State what you could **not** fix and why. An honest blocker is worth more than
  a plausible guess.
- If you disabled, skipped, `#[ignore]`d, or stubbed anything, say so on its own
  line prefixed `WEAKENED:`. Hiding this is the one unforgivable failure.

  This is under-reported in practice, so here is the test: **would a reader who
  trusted your summary be surprised by what the code now does?** If yes, it is a
  weakening. Concretely, all of these count and have all been missed at least
  once on this program:
  - a build script that returns early instead of building something
  - a guard, assertion, or check made more permissive so a case passes
  - a wildcard match arm added to something `#[non_exhaustive]`
  - a fabricated fixture, asset, or default standing in for real data
  - a test narrowed, an assertion deleted, a timeout raised
  - anything you described in a README that you did not actually do

- Do not describe intended behaviour as completed behaviour. If your summary and
  the working tree disagree, the summary is the bug.
- Never claim a test passes unless you ran it and saw it pass. Paste the command.

---

## 7. Build commands that actually work here

```bash
# Backend workspace (nightly bootstrap needed: nudox-ir uses unstable macro decls)
#
# The bare --workspace form is now correct and is what you should run.
# Re-measured 2026-08-07: `RUSTC_BOOTSTRAP=1 cargo check --workspace
# --all-targets` exits 0, zero errors, ~2 min. The four-crate exclude list
# that used to live here (index/registry/ir-vcs/driver, citing L6) is gone;
# L6 was resolved 2026-08-05 and the list outlived it by two days.
RUSTC_BOOTSTRAP=1 cargo check --workspace --all-targets

# The GUI is a SEPARATE package with its own lockfile
cargo check  --manifest-path workspace/gui/Cargo.toml --all-targets
cargo test   --manifest-path workspace/gui/Cargo.toml --test shell_flow

# WHAT IS ACTUALLY BROKEN, PRECISELY (re-measured 2026-08-07). The previous
# text here said "the ENTIRE workspace compiles ... the --exclude list is now a
# speed optimisation, not a necessity." That was imprecise in the one direction
# that mattered: `driver` does not build, and no amount of `cargo check` will
# tell you so.
#
# Everything COMPILES. `driver` still does not reliably produce artifacts, and
# `cargo check` cannot tell you that. The cause class is docs/LIMITATIONS.md **L7**
# — vendored C sources under `workspace/vendor/` that `driver` transitively
# needs (qdrant-edge's SIMD kernels, doltlite's generated amalgamation).
#
# Three runs of `cargo build -p driver --all-targets` within 25 minutes on
# 2026-08-07 returned three different results, because a concurrent track was
# restoring those sources while the check ran. Recorded, not averaged:
#   - FAILS: `error[E0433]: cannot find type Path` in
#     `workspace/vendor/qdrant-edge/build_segment.rs:22` (a mid-edit tree)
#   - SUCCEEDS: exit 0, real `target/debug/driver`; and a bare
#     `cargo nextest list -P default --workspace` with NO --exclude at all
#     exits 0 and enumerates 2470 tests, 199 of them in `driver`
#   - FAILS: `rusqdoltlite`'s build script, once `workspace/vendor/doltlite/
#     doltlite.c` was restored — cc-rs + this nix cc-wrapper reject the target
#     triple ("unable to create target: 'Unable to find target for this
#     triple'"). An L7 mitigation that works by REMOVING a source stops
#     working when the source returns.
#
# The widely-quoted link diagnostic — undefined `_dotProduct_half_4x4`,
# `_euclideanDist_half_4x4`, `_impl_score_dot_neon`, `_impl_score_l1_neon`,
# `_impl_xor_popcnt_neon_uint128`, `_manhattanDist_half_4x4` from
# libqdrant_edge, on bin `driver` and tests `live_download`, `live_socket`,
# `pipeline_end_to_end` — matches the same L7 cause and the exact kernels
# `build_segment.rs` names in its own text. Treat it as REPORTED, not
# reproduced: by 2026-08-07 the failure had moved. What is NOT true either
# way: it is not L6, and it is not the "duplicate symbol between
# libzstd_seekable and libzstd_sys" several comments around this repo assert —
# nothing in any observed diagnostic mentions zstd.
#
# So `--exclude driver` is a NECESSITY today, on the honest grounds that this
# package's build is currently non-deterministic — not on a specific symbol
# list. The other three exclusions were the speed optimisation, and they were
# costing ~1040 real tests (731 in `index`, 216 in `ir-vcs`).
#
# THE RE-CHECK LOOP THAT USED TO BE HERE WAS THE BUG. It read:
#   for p in index registry ir-vcs driver; do cargo check -p $p --lib; done
# Both flags are wrong for this job, and together they are exactly why the
# claim above drifted for two days without anyone noticing:
#   - `cargo check` NEVER LINKS. It cannot see a link error by construction
#     (doctrine §8 already says this about FFI: "`cargo check` is not a
#     sufficient gate for anything with C FFI — it never links"). `driver`
#     passes `cargo check` today and still cannot produce a binary.
#   - `--lib` cannot see failing bin/test/bench targets. All four of `driver`'s
#     failures are bins and integration tests; its lib is fine.
# The loop was therefore green on a package that does not build, and green on
# `index`/`ir-vcs` long after they had stopped needing to be excluded. Use:
#
#   RUSTC_BOOTSTRAP=1 cargo build --workspace --all-targets --exclude driver \
#     && RUSTC_BOOTSTRAP=1 cargo build -p driver --all-targets
#
# `build --all-targets` (not `check`, not `--lib`) is the only form that
# actually answers "does this produce artifacts?". Run the second half TWICE
# on a settled tree before believing either answer — see the three-runs record
# above. The day it passes twice, drop `--exclude driver` from
# `.config/scripts/nextest-suite.nu` and from `.config/nextest.toml`'s header,
# and understand that doing so starts running `driver`'s 199 tests, which have
# never executed here.
#
# Why this matters more than it sounds. `workspace/index` had NEVER compiled, so
# it was excluded from every gate — and being excluded meant nothing ever
# re-checked whether that was still true. 16 of its 17 errors turned out to be a
# module file misnamed `lib.rs` instead of `mod.rs`, a `zip` line missing from
# Cargo.toml (13 errors alone), and four `format_args!` that needed `format!`.
# `ir-vcs` was 115 errors of which 110 were ONE missing `use` — `pijul_err`
# already existed three lines away in a module the file already imported from.
# Its 731 tests had never executed once.
#
# A crate outside the gate stops being measured, and a one-character typo then
# survives indefinitely behind a reputation for being unfinished. An exclude
# list is a tourniquet, not a diagnosis.

# NEW as of 2026-08-05: building `nudox-store` now requires a JDK.
# `nudox-producer-java`'s build.rs runs `javac` unconditionally to compile the
# javadoc doclet, and nudox-store depends on it. You need JDK 17+ `javac` on
# PATH (or `NUDOX_JAVAC` set) to build the crate AT ALL — not merely to document
# Java packages. nix provides zulu-ca-jdk-21. If nudox-store suddenly fails to
# build on a machine that used to be fine, check `javac -version` first.
#
# The C# producer does NOT have this property: it resolves its oracle at runtime,
# so registry construction succeeds without `dotnet publish` having run. A
# missing oracle surfaces at invoke as a typed error naming the path and the
# publish command.

# One crate
cargo check -p nudox-producer-rust

# `nudox-store` alone needs its non-default `fixtures` feature, or
# tests/corpus_contract.rs fails to compile with "could not find fixtures in
# source". Under --workspace it is unified on by nudox-engine's dev-dependency,
# which is why this only bites when you test the crate on its own:
RUSTC_BOOTSTRAP=1 cargo test -p nudox-store --features fixtures
```

`workspace/vendor/rusqdoltlite` is excluded from the workspace: it compiles a
~20 MB generated C amalgamation that is deliberately not committed. Do not
re-add it to `members`.

---

## 8. Hard-won facts (each of these cost someone an hour)

**Cargo auto-promotes in-tree path dependencies to workspace members.** A comment
saying "this is not a member" does not make it so; only `exclude` does. This is
why `cargo check --workspace` used to die compiling a C file that is not in the
repo.

**A fixture checkout inside the repo is captured by the repo's workspace.** Every
crate checkout under `result/` must have an empty `[workspace]` table
appended to its `Cargo.toml`, or `cargo metadata` fails with "current package
believes it's in a workspace when it's not" — and rust-analyzer reports that as a
*load* failure, which reads like a producer bug.

**The producer's package name is a key, not a label.** `PackageSource::name` is
matched against cargo metadata by `documented_package_names` to select which
package to document. Pointing a checkout of `log` at the name `"axum"` yields
zero documented packages and fails as `RustProducerError::Load`. If a lowering
fails on a crate that obviously exists, check the name before you check the code.

**Print the whole `#[source]` chain when a producer fails.** `ProducerError`'s
top-level `Display` is deliberately terse (`"workspace load failed"`). Reading
only it is how a five-second diagnosis becomes an hour. Walk
`std::error::Error::source` and print every link.

**`map_err(|_| SomeVariant)` is almost always a bug.** It throws away the thing
you will need. `ra::lower_workspace` had one that relabelled *every* error as
"rust-analyzer analysis cancelled", including errors that had nothing to do with
cancellation, under a comment describing an upstream API contract that did not
exist. If you write `|_|`, justify it in a comment or don't write it.

### Running the suite

**`cargo nextest` hides passing tests' stdout by default, which silently
deletes every benchmark line.** nextest's default is `success-output = "never"`,
so the `cost case=` lines that doctrine §4 requires never reach the report and
the report renders empty — looking like nothing was measured rather than like
output was dropped. The `ci` and `perf` profiles set `success-output` /
`failure-output` / `store-success-output` explicitly. If a perf report comes back
with no rows, check this before you go looking for a broken test.

**One `harness = false` binary poisons the *entire* nextest invocation.**
`cargo nextest list` exits 104 with zero tests enumerated **for every binary**,
not just the offending one, because the screenshot suite's output does not end in
`": test"`. The GUI config filters `binary(=screenshots)` and `binary(=shot_probe)`
out of `default-filter`; they are run separately by `cargo test`. A nextest run
that reports no tests is far more likely to be this than an empty workspace.

**In Nushell, `... | append $file` is a list operation, not a file write, and
it fails silently.** `nix build .#checks.corpus`'s code to append the mandatory
`[workspace]` table to each fixture's `Cargo.toml` had never once run — it built
a list and discarded it. Every fixture that has the table got it from
`flake.nix`'s separate shell-based append. The correct form is `save --append`.
Nothing errored, nothing warned, and the script had looked correct for as long as
it existed. When a Nushell step "works" but its effect is invisible, check that
you used a command that writes.

**Piping into `nu script.nu` does not forward stdin.** `cargo test … | nu
.config/scripts/perf-report.nu` silently produces **zero rows** — `$in` is empty
inside `def main` when the script runs as a subprocess (nushell 0.114.1). The
form that works is `nu --stdin script.nu`. This was wrong in the script's own
documented usage for a while, so any perf report taken before 2026-08-05 that
looked empty probably was not.

**The whole suite has one entry point:** `nu .config/scripts/nextest-suite.nu`
(`--profile default|ci|perf`, `--gui`, `--screenshots`, `--all`). It bakes in
`RUSTC_BOOTSTRAP=1`, the `--exclude` list, libclang env detection, and runs
root → GUI → screenshots strictly sequentially, because concurrent GUI builds
corrupt the build-script cache. Use it rather than reassembling the flags.

### GPUI

**A view's `render` must produce exactly ONE root element. A second root
silently breaks actions on *other* views.**

This cost a day. `cmd-W` stopped working — an action bound in the `Pane` context
and handled on `Pane`'s own div, two levels above the view that broke it.

The mechanism: GPUI attaches a view's `FocusHandle` to the dispatch tree only
where `track_focus` is called. `Window::focus_node_id_in_rendered_frame`
**silently falls back to the window root** when the focused handle is not present
in the last rendered frame — and the window root carries no key context at all.
So `SymbolPage`'s `ColdError` early-return, which built a second bare root
without `key_context` or `track_focus`, did not merely disable that page's own
actions. It deleted the **entire ancestor context stack** for the focused
element, `Pane` included.

Three things make this especially nasty:

- It is a *leaf* view breaking an *ancestor's* bindings, so every instinct sends
  you to the wrong file. Focus-context and keymap-context investigations both
  come back clean.
- It only fires on one render branch. Here, only the cold-error path was
  untracked, so it was invisible until a document failed to stream.
- It stays latent until something focuses the view. Making `Pane::activate_ix`
  focus the active item is what turned a dormant inconsistency into an active
  regression — the fix did not cause the bug, it revealed it.

**The shape that prevents it**: split `render` into one function that is the
*sole* producer of the root element and applies `id`, `key_context`,
`track_focus`, and every `.on_action` exactly once — and a second that returns
only children and is handed no root. Then no future `PageState` branch can drop
the attachment, because no branch is in a position to. Do not fix an instance of
this by adding `track_focus` to the offending branch; fix it so the branch cannot
express a root.

**GPUI has no z-index. Later siblings paint over earlier ones, full stop.**
The version picker popover rendered correctly and invisibly for the entire life
of the project — later siblings painted straight over it, so its screenshot came
out byte-identical to the frame before and it read as "the picker does nothing".
Two agents investigated it as a dispatch or focus problem. The fix is
`gpui::deferred`. If an overlay, popover, tooltip, or menu appears not to open,
check paint order **before** you check whether its action fired.

**A `relative()` size with no definite containing block is silently dropped.**
`max_h(relative(0.60))` on the search panel did nothing, so an animation
spring's 9999 px sentinel became the panel's real layout height. Because the
panel is `.occlude()`d, its hitbox then covered the whole window and the scrim
underneath became unclickable. This presented as a 20–25% flaky test and was a
real user-facing bug. When a percentage-based constraint appears not to apply,
check that its parent has a definite size before assuming the value is wrong.

### GPUI testing

**Two clocks have to move, and they are not the same clock.**
`cx.advance_clock()` drives the `TestDispatcher`'s timer wheel — that is what
fires debounces (e.g. the search store's 24 ms input debounce). GPUI's
declarative `Animation` instead takes its phase from *wall-clock* elapsed time
off the frame timestamp. Drive only the simulated clock and entrance fades are
photographed mid-flight, so a working UI renders as ghostly half-transparent
text. Drive only real time and debounced work never fires at all. Settle loops
must do both, and must re-draw between them.

**`capture_screenshot` reads back the last *rendered* frame.** A capture without
a preceding draw photographs stale pixels.

**Screenshot tests need `harness = false`.** libtest runs `#[test]` bodies on
spawned worker threads; constructing the macOS platform touches AppKit and aborts
off the main thread. With no harness the test file is its own `fn main`.

**Real pixels need three things wired together**: `HeadlessAppContext::with_platform`,
a real `PlatformTextSystem` (`gpui_platform::current_platform(true).text_system()`
— otherwise glyph metrics are stubbed and you are photographing a layout no user
will ever see), and `gpui_platform::current_headless_renderer()` behind that
crate's `test-support` feature. No display, no window server, and no
screen-recording permission is involved.

**`ThemeMode::default()` is `Light`.** `main.rs` does not choose, so lindsey's
appearance is currently a property of the host. Anything that needs a specific
look must set it explicitly, *before* `NudoxThemeExt::init`, which picks its
palette by asking `cx.theme().is_dark()`.

**`cargo check` is not a sufficient gate for anything with C FFI — it never
links.** `nudox-producer-clang` passes `cargo check` cleanly and then aborts every
binary it is linked into: the result references `@rpath/libclang.dylib` with zero
`LC_RPATH` load commands, so dyld kills the process at load, before any code runs.
`otool -L <bin>` and `otool -l <bin> | grep LC_RPATH` are how you see it. Before
adding an FFI crate as a dependency, build **and run** something that links it.

**A comment describing behaviour is a claim, and claims rot.** Three separate
times this session a comment asserted something the code did not do: `attach_db`
was documented as returning panic payloads (it returns `R`); `nudox-producer-clang`
documents a lazy-`dlopen` failure (it is a hard link-time one); `nudox-producer-csharp`'s
module docs describe a `Producer` impl "coded against the published contract
signature" that does not exist in the crate. When a comment and the code disagree,
the comment is the bug — and it is the more expensive one, because it sends the
next reader somewhere real evidence will not.

**`result/` is gitignored, so deleting a fixture is irreversible.** An agent
ran `rm -rf result/log-0.4.33` while composing a shell comment and destroyed
it; git could not restore it because the directory is ignored. It was rebuilt from
crates.io and verified against its known baseline (419 entries), and the agent
disclosed the whole thing unprompted — which is exactly right, and is why it cost
twenty minutes rather than a day of confusion.

Two rules follow. Never construct an `rm -rf` whose target you have not just
`ls`-ed. And when a fixture is destroyed, restoring it is not enough: you must
re-derive its known-good measurement (entry count, hash) and state that it matches,
because a silently-different fixture invalidates every number taken against it.

**Do not run two agents that both build `workspace/gui` at the same time.** They
share one `target/` directory, and concurrent cargo invocations against it corrupt
the build-script cache. The observed symptom is
`dyld: Library not loaded: @rpath/libclang.dylib` plus dozens of half-populated
`gpui-*` directories under `target/debug/build/`. Recovery is
`cargo clean -p clang-sys -p bindgen`. A second symptom is timing noise: a
real-crate lowering has been measured at 21.9 s idle and 44.6 s under load —
a 2× swing with no code change, which is larger than most optimisations you will
be trying to measure. Serialise anything that builds the GUI, and take
benchmarks on a quiet machine or not at all.

**A number that three different inputs agree on is a bug report, not a
result.** Three generations of memchr each lowered to *exactly* 11,329 entries.
That was investigated and closed as innocent — the API delta between them really
is tiny — and the reasoning was wrong. The true cause was that `cargo metadata`
was failing in 20 of 22 fixtures and upstream `ra_ap_project_model` silently
substitutes `--no-deps` metadata on failure, returning `Ok((metadata,
Some(error)))`. With no `resolve` section every dependency vanishes and every
Cargo feature — *including `default`* — evaluates false, so all three versions
were being measured as the same feature-less shell. Once dependencies resolved,
the three split to 902 / 11,361 / 11,361.

Two rules follow. **Suspicious agreement deserves the same scrutiny as
suspicious disagreement** — if a measurement is invariant under an input that
should move it, find out why before explaining it away. And **before trusting
any measurement, verify the thing being measured actually loaded**: run
`cargo metadata --offline` in the fixture yourself. Every cost-per-entry number
in `docs/LIMITATIONS.md` L1, and the 66,983-entry corpus total, were taken through
this fallback and are being re-derived.

That upstream fallback is also the canonical `map_err(|_|)` shape from the rule
above, wearing a different hat: a real failure converted into a success with the
cause parked somewhere a caller need not look. When you meet one, fix the *type*
so the degraded case cannot be mistaken for the good one. A `warn!` does not do
that — nothing in a log line stops the next caller.

**A silent repair is the same defect class as a silent failure.** Both present a
degraded case as the good one, and the rule against the second (`map_err(|_|)`,
the `--no-deps` fallback above) applies unchanged to the first.

A *repair* is anything we render differently from what the source says, because
we judged the source wrong: a malformed link resolved anyway, a broken encoding
guessed at, a missing field defaulted to something plausible. Repairs are often
the right product call — a transposed backtick in someone else's crate should not
be the reader's problem. What is never acceptable is doing it invisibly.

**A repair is permitted only when it is typed, counted, bounded and visible.**

- **Typed** — the output carries *which* repair happened, in-band, on the thing
  repaired. Not a log line, not a side-channel keyed by an id that does not
  exist. A mandatory field with no `Default` on the value that crosses the seam,
  so the repaired case cannot be constructed without answering the question.
- **Bounded** — the set of repairs is a closed enum with no wildcard match
  anywhere, so "what else do we silently repair?" has a finite, readable answer.
  Take the `ProducerError` exception from §3: exhaustive beats
  `#[non_exhaustive]` here, because the whole value is that a new variant breaks
  every match and forces each one to decide.
- **Counted** — the count is *derived from* the repaired values, never
  accumulated alongside them. A tally kept in parallel with the thing it counts
  can drift from it; one computed from it cannot. And the number must be pinned
  by a test against real input, not asserted to be non-zero.
- **Visible** — the reader can see which output we changed, and ask what the
  original said. A repair the reader cannot distinguish from an authored value
  is a lie told politely.

The worked example is `docs/LIMITATIONS.md` L46 and `docs/DOCSRS-COMPARISON.md` §2. We
render a working link for `` `[foo`] ``, which rustdoc and docs.rs render as
dead text. For months that was real product value that could be credited as
neither parity nor differentiation, because nobody had chosen it and nothing
recorded it — the repaired link and an authored one produced byte-identical
output. Owning it cost one enum, one mandatory field, a derived tally and a wavy
underline; a *second* repair now costs five compile errors in five files before
it can render once. That asymmetry is the design: the first repair is cheap, and
every one after it has to be argued for in public.

Two corollaries. **A test that proves the repair happens is not a test that
proves it is recorded** — the state where the repair was silent was fully green.
Assert on the record, not the effect. And **verify the guard by mutation**:
break the repair verdict deliberately and confirm the suite goes red. A guard
nobody has watched fail is a guard nobody has tested.

**A screenshot suite that cannot fail is decoration.** Assert that the frame is
opaque, that it has more than a handful of distinct colours (a flat fill means
nothing painted), and — after any step that should have changed the screen — that
the frame actually differs from the previous one. Two byte-identical frames with
different captions is the exact failure this catches.
