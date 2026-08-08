# LIMITATIONS.md — the running ledger

Every known limitation of this system, with evidence and a verdict. Append, do
not rewrite history: when something is fixed, mark it `RESOLVED` with the commit
or the evidence, and leave the entry.

Ordered by how much it costs us, not by how easy it is to fix.

Legend — **Blast radius**: how much of the product it breaks.
**Evidence**: how we know. An entry without evidence is a rumour and must be
marked as such.

---

## L1 — IR generation cost is ~95% fixed overhead, not proportional to output

> ### ⚠ Entry counts below are SUPERSEDED. True corpus total: **55,449**.
>
> Three figures have been in play this session. Only the last is real:
>
> | | |
> |---|---|
> | **66,983** | degraded (`--no-deps`, L28) *and* phantom-inflated (L38). Void. |
> | **87,347** | resolved, still phantom-inflated. What the corpus actually measured before 2026-08-05. |
> | **55,449** | both defects fixed. **Use this.** |
>
> **38.9% of the entire pre-fix corpus was `core`/`std` names wearing a
> fixture's name.** 33,961 phantom re-export entries removed — 77% of every
> `Reference` in the corpus — while non-`Reference` entries *rose* 43,041 →
> 45,104, because the feature fix recovered **+2,063 real entries**.
>
> Selected per-fixture (full table in L38):
>
> | fixture | before | after | Δ |
> |---|---:|---:|---:|
> | memchr 2.8.3 | 11,361 | 1,325 | −88.3% |
> | memchr 2.7.6 | 902 | 1,322 | **+46.6%** |
> | syn 1.0.109 | 22,679 | 12,242 | −46.0% |
> | serde 1.0.196 | 7,505 | 5,680 | −24.3% |
> | **libc 0.2.161** | 15,309 | **16,922** | **+10.5%** |
>
> **libc went up** — it had memchr 2.7.6's shim-shadowing defect undetected,
> milder because it pins the non-empty 1.0.1 shim. 38 hand-written trait impls
> that had degraded to inherent impls were replaced by 603 correctly-attributed
> ones.
>
> **Cost-per-entry was understated ~1.58× on average, but the error is wildly
> non-uniform** — memchr 8.6×, syn 1.85×, itoa 1.04×. **It cannot be corrected
> by scaling.** Use the per-fixture numbers or re-measure.
>
> **The µs/entry column below is wrong in both terms**: the denominator was
> inflated, and the numerator was taken under load contention (doctrine §8
> records a 4.6× swing on identical work). The *qualitative* diagnosis survives
> — `itoa` costing more than `libc` while emitting far fewer entries is too
> large a gap to be an artifact — but re-derive on a quiet machine before
> optimising against it.

**Blast radius:** every producer run, every package, forever. This is the
dominant term in the entire pipeline.

**Evidence (SUPERSEDED — see banner):** `CORPUS-REPORT.md`, 19 crates lowered:

| Crate | Entries | Wall (s) | µs/entry |
|---|---:|---:|---:|
| unicode-width 0.1.11 | 34 | 21.4 | 629 000 |
| once_cell 1.20.2 | 79 | 41.4 | 524 000 |
| itoa 1.0.18 | 153 | 53.2 | 348 000 |
| thiserror 1.0.40 | 53 | 28.4 | 535 000 |
| libc 0.2.161 | 14 710 | 40.5 | 2 752 |
| memchr 2.8.3 | 11 329 | 33.7 | 2 974 |

`itoa` emits 153 entries and costs 53 s. `libc` emits 14 710 entries — 96× more —
and costs *less*. Peak RSS is 517–713 MB across the whole range, including for the
34-entry crate.

**Diagnosis:** the cost is `ProjectWorkspace::load` + `run_build_scripts` +
`parallel_prime_caches` in `workspace/compiler/languages/rust/src/ra/loaded.rs`.
The actual HIR walk is a rounding error. We are paying to boot rust-analyzer, once
per package, serially.

**Vectors, in order of expected payoff:**
1. **Amortise the boot.** One rust-analyzer instance can hold many crates. Loading
   N packages that share a dependency graph should not pay N× workspace load.
2. **`run_build_scripts` is on by default** (`ExtractConfig::default`) and is pure
   cost for packages with no build script. Make it conditional on the manifest
   actually declaring one.
3. **`parallel_prime_caches` primes everything.** We walk only documented items;
   priming the whole graph is speculative work we then throw away.
4. **Parallelise across packages** (rayon / a bounded pool). Currently each
   `spawn_blocking` handles one package and the corpus run is serial: 669 s wall
   for 21 crates that could overlap.

**Status:** OPEN. Instrumentation in progress.

**Measurement hygiene — read before trusting any number in this section.** The
`CORPUS-REPORT.md` figures above were taken on an otherwise-idle machine. Numbers
gathered later in the same session were taken while up to seven agents were
compiling concurrently at load average 25, and a real-crate lowering slowed from
21.9 s to 44.6 s under that load — a 2× swing with no code change. Any
before/after comparison must therefore come from the *same* machine state, and
ideally back-to-back. `.config/scripts/perf-report.nu --baseline` exists to make
that comparison explicit rather than remembered; treat a delta smaller than the
observed noise floor (~2×) as no signal at all.

---

## L2 — RESOLVED: six of seven languages are now reachable

> ### ✅ RESOLVED, 2026-08-05. `nudox-store`: 48/48. Workspace check: clean.
>
> `ProducerRegistry::with_all_available()` now resolves a runner for **Rust, Go,
> Java, C#, TypeScript, C, and C++** — the last two through
> `register_c_and_cpp`, so the `Cpp`-yields-nothing trap (L33) cannot recur.
>
> **Python is the only remaining gap**, and it is deliberate: without the
> `pyrefly` feature its `invoke()` returns an empty oracle, so it yields
> `SourceError::ToolchainMissing` rather than pretending to succeed. That is the
> honest answer, not a regression.
>
> Each producer was proven against a **real third-party package** before being
> registered — not a fixture:
>
> | | package | entries |
> |---|---|---:|
> | Rust | memchr 2.8.3 | 1,347 |
> | Go | go.uber.org/zap 1.28.0 | 2,919 |
> | Java | Gson 2.11.0 | 2,103 |
> | C# | Polly.Core 8.5.2 | 2,124 |
> | C# | CommunityToolkit.Mvvm 8.4.0 | 1,241 |
> | C/C++ | multi-file C project + `compile_commands.json` | verified |
>
> **Every one of the four agents found a defect that fixtures had hidden** — see
> doctrine §4. Go crashed on all real code; Java silently dropped 52 entries; C#
> would have rejected entire packages; C/C++'s silent skip concealed a
> process-wide singleton race. Four for four.
>
> **New build requirement:** `nudox-producer-java`'s `build.rs` runs `javac`
> unconditionally, so building `nudox-store` now needs JDK 17+ on PATH. Recorded
> in doctrine §7. C# does *not* have this property — its oracle resolves at
> runtime.
>
> **Independent confirmation of L24 from a second angle:** the linked
> `nudox-store` test binary shows **zero** libclang references and zero
> `LC_RPATH` entries, with 48/48 and `nudox-engine`'s 135/135 running clean.
>
> Registration alone does not mean the GUI *displays* these well — L39 (external
> type identity) and L40 (C# namespaces dropped) both still apply, and no
> screenshot yet shows a non-Rust package.

## Superseded — kept for the reasoning trail

**Blast radius:** six of seven languages. The product claims to be multi-language.

**Evidence:** `nudox_engine::ProducerLanguage` has exactly one variant (`Rust`).
`Engine::start_with_versions` matches on it and calls `PackageDescriptor::cargo`
unconditionally. `nudox-store`'s only producer dependency is
`nudox-producer-rust`, and `ProducerRegistry::with_rust_pilot()` is the only
constructor used anywhere. `workspace/gui/src/main.rs` hardcodes
`language: ProducerLanguage::Rust`.

Meanwhile seven producer crates exist and are substantial: go (1 757 lines),
java (3 980), csharp (3 328), python (2 048), typescript (1 758), clang (1 789).

**Status:** PARTIALLY RESOLVED — and the remainder is much larger than "wiring".

`ProducerLanguage` now has seven `#[non_exhaustive]` variants, each documenting
its oracle and manifest file, and `ProducerRegistry::with_all_available()` exists.
The choke point is gone.

**But only three of seven producers can actually be registered**, because
**`impl Producer` does not exist for three of them at all**:

| Language | `impl Producer` | Registered | Notes |
|---|:--:|:--:|---|
| Rust | yes | yes | proven: 19 real crates lowered |
| TypeScript | yes (`impl<O: TsOracle> Producer for TypescriptProducer<O>`) | yes | OXC, in-process |
| Python | yes | yes | pyrefly is behind a crate feature; **without it the oracle is empty**, so "registered" ≠ "produces anything" |
| C / C++ | yes (`type Id = Usr`) | **no** | implements the contract but `with_all_available` does not register it |
| Go | **no** | no | 1 757 lines. `producer.rs` is gated on `#[cfg(feature = "producer-trait")]` — **a feature that does not exist in its `Cargo.toml`**, so the impl is compiled out unconditionally and nobody noticed |
| Java | **no** | no | 3 980 lines, no `producer.rs` at all |
| C# | **no** | no | 3 328 lines, no `producer.rs`. Its own module docs claim a `Producer` impl exists "coded against the published contract signature" — **no such file is present** |

So roughly **9 000 lines of producer code across Go, Java and C# cannot be
invoked by anything**, because none of them implements the `Producer` contract the
pipeline drives. They compile (`cargo check -p nudox-producer-{go,java,csharp}`
is clean) — they are simply not connected to the trait that makes them runnable.

**This is the real blocker for "20 packages per language".** Four of seven
languages cannot lower a single package today, and a fifth (Python) needs a
feature flag verified before it can lower a non-empty one.

**Next steps, in order:** register the C/C++ producer (it already implements the
contract — cheapest win); verify Python's `pyrefly` feature actually produces
symbols; then write `impl Producer` for Go, Java and C# over their existing
lowering code.

---

## L3 — `Symbol::cfg`, `attrs`, `aliases` are declared but never populated

**Blast radius:** feature-gated API is indistinguishable from core API in the UI.
docs.rs shows "Available on crate feature `std` only"; we show nothing.

**Evidence:** `workspace/compiler/languages/rust/src/ra/item.rs` hardcodes
`cfg: None`, `attrs: Box::new([])`, `aliases: Box::new([])` at every `Symbol`
construction site. The only mentions in `crates/nudox-engine/src/chunk/` are test
fixtures setting them empty. The fields therefore never reach `wire::SymbolHead`
and never reach the GUI.

**Status:** PARTIALLY RESOLVED.

- `cfg` now flows end to end. It turned out the *producer* was already computing
  it correctly (`ra::docs::cfg_expr` + `ra::ctx::symbol_parts`); the loss was
  entirely at the chunk/wire/GUI boundary. `chunk::head::head()` destructured only
  the fields someone had remembered to wire, and nothing made the compiler ask for
  the rest. Now: `wire::SymbolHead.cfg` carries a rendered predicate, and the
  symbol-page header renders it as a chip reusing the existing badge component.
  Verified against a real crate — `memchr::arch::aarch64` (declared
  `#[cfg(target_arch = "aarch64")]` in `.real-crates/memchr-2.8.3/src/arch/mod.rs`)
  reaches `SymbolHead.cfg == Some("cfg(target_arch = \"aarch64\")")`.
- **Still open on the producer side:** five sites build `Symbol` literals directly
  instead of going through `ctx.symbol_parts()`, and each hardcodes `cfg: None` —
  `lower_module`'s re-export symbol, `lower_enum`'s variant symbol,
  `declare_hir_fields`'s field symbol, the builtin-type symbol, and `plain_sym`.
  Real crates do put `#[cfg(…)]` on enum variants, struct fields and `pub use`
  re-exports, so these are genuine (smaller) losses. The structural fix is to make
  the direct-literal construction impossible rather than to patch five call sites.
- `attrs` and `aliases` remain OPEN, untouched.

**See also L14**, which is the same theme at much larger scale.

---

## L19 — Rendered signatures drop generic wrappers: `Option<usize>` shows as `usize`

**Blast radius:** correctness of the single most-read line on every page. This is
not missing information, it is *wrong* information — a reader who trusts the
displayed signature writes code that does not compile.

**Evidence:** `.shots/memchr/06-selection-second-hit.png` renders

    fn memchr(needle: u8, haystack: &[u8]) -> usize

The actual declaration, `.real-crates/memchr-2.8.3/src/memchr.rs:27`, is

    pub fn memchr(needle: u8, haystack: &[u8]) -> Option<usize>

The parameters survive intact — `needle: u8` and `haystack: &[u8]` are both
correct, and `&[u8]` proves references and slices render fine. Only the return
type's generic wrapper is lost, leaving the innermost argument.

**My original hypothesis was WRONG and has been disproved.** I guessed the bug was
in `ra/ty.rs` or `chunk/signature.rs` — that a nominal type's generic arguments
were rendered *instead of* the type. Direct instrumentation of `lower_path_type`
during a real memchr lowering shows **150/150 `Option<T>` uses (memchr's own
included) resolve via `PathResolution::Def` and build `Type::Apply { base, args }`
correctly**. Neither file has a defect; `Type::Apply` structurally separates the
constructor from its arguments and nothing collapses them. That is now an explicit
checked invariant (`generic_signature_shapes.rs::apply_nesting_is_correct_not_collapsed_to_inner_arg`,
passing) rather than an assumption.

**The two real causes are both one level down, and the second is much worse than
the symptom that led us here.**

### (a) Cross-package `Ref::Local` is never resolved to `Ref::Foreign`

`Lowering::refer_import` (`workspace/ir/model/src/lower.rs:271-273`) returns a
`Ref::Local` pointing at an import-arena slot "to be resolved later". **Nothing
ever resolves it.** `IrPackage::seal` (`workspace/ir/model/src/package/seal.rs`)
rewrites `Ref::Local → Ref::Intro` only for the package's *own* declared entries,
and a repo-wide grep confirms `Ref::Foreign(` is **constructed nowhere** in
`workspace/ir/model/src` — it is only ever matched.

So every foreign generic's ref survives as `Ref::Local`, and
`signature.rs::resolve_nominal` — whose own comment says this "should not appear
in a sealed table" — renders it `"?"`.

Measured on real memchr: **147 of 420 functions (35%) render a bare `?`** for a
foreign generic base, and **0 of 420** correctly render `Option<`, `Result<`,
`Vec<`, `Box<` or `HashMap<`. Nothing in the mechanism is language-specific, so
this affects every producer.

> **Confirmed independently by a second producer, 2026-08-05.** The Go producer
> hit this from a standing start, without knowledge of the Rust finding: every
> foreign named type (`io.Closer`, `sync.Mutex`, universe-scope `error`) routes
> through `refer_import` and lands in the same unresolved import arena. Go's
> `zap` lowering preserves the reference structurally but it will render the
> same `?` downstream.
>
> That upgrades this from "predicted to affect every producer" to **measured
> across two independent language frontends**, and it means the cost scales with
> every producer we add rather than being a Rust-specific defect. Java, C#, and
> C/C++ will each hit it the moment they lower anything that imports.
>
> This is now the highest-value single fix in `nudox-ir`: one resolution step
> repairs signature rendering for every language at once.

### (b) Parameter identities collide — see L23

Cause (a) alone would render `?<usize>`. We see the literal `usize`, which takes
a second bug: **`memchr`'s own return-type `Param` is overwritten by a sibling
function's**. That is L23, and it is a symbol-identity defect, not a rendering one.

**Status:** OPEN, root-caused, and both fixes are in files no agent was authorised
to touch this session (`workspace/ir/model/src/{lower,package/seal}.rs` and
`ra/item.rs`). Two `#[ignore]`d tests are committed deliberately RED to document
the gap — `generic_signature_shapes::text_rendering_of_foreign_generics_is_currently_broken`
and `real_memchr_generic_return::real_memchr_return_type_keeps_option_wrapper` —
following the repo's existing precedent (`real_crate.rs::router_doc_links_are_populated`).

---

## L23 — Symbol identities collide: 416 functions reduce to 188 identities

**Blast radius:** the foundation. `IntroId` is the key everything else is built
on — search results, hyperlink targets, lineage across versions, the graph. If two
different symbols share one, every one of those is silently wrong.

**Evidence, measured on real memchr:** **58 collision groups**; the 416 functions
that have a return type reduce to **188 distinct identities**. `memchr`'s own
`return` Param is overwritten by one of 18 siblings (`memchr2`, `memrchr`,
`*_raw`, `*_iter`) — most plausibly `count_raw`, which legitimately returns bare
`usize`. That is why the rendered signature shows the literal `usize` rather than
the `?<usize>` that cause (a) alone would produce.

**Mechanism** — three things compounding:
1. `declare_params` and the `output_refs` closure in
   `lower_free_function_with_id` (`ra/item.rs`) declare every parameter and return
   `Param` under the function's **enclosing** `parent` (module or impl) instead of
   under the function's own id.
2. `seal.rs`'s disambiguator falls back to `Disambiguator::Span` for kinds that
   are not Function or Impl.
3. `item.rs::plain_sym` always sets `span: 0..0` for synthesized params.

So every same-named parameter in the same module lands on one `IntroId`.

**The weak abstraction** (doctrine §3): `item.rs` reuses a single
`parent: Option<RaId>` variable to mean two different things — "where this
function is declared" and "where this function's own children are declared" —
with nothing in the type system preventing a call site from passing the wrong one.
The fix is for `declare_params` and the output-param closure to take the
function's own `RaId` as a required, non-optional argument rather than reusing
`parent`.

**Related weak abstraction for L19(a):** `Lowering::refer_import` returns the same
`Ref::Local` type used for ordinary forward references, so nothing distinguishes
"a local forward-ref that `seal` will resolve" from "a foreign import placeholder
that nothing currently resolves". Both type-check identically, so the missing
resolution pass cannot be caught by the compiler.

**Status:** OPEN. Almost certainly the highest-severity defect in this ledger.

---

## L20 — Search results are indistinguishable when leaf names collide

**Blast radius:** search usability on any real crate. Real crates repeat names
across modules constantly.

**Evidence:** `.shots/memchr/06-selection-second-hit.png`, query `memchr`,
22 results. After `struct Memchr<'h>` and `fn memchr(...)`, there are **seven
consecutive rows that are pixel-identical apart from position**:

    mod  memchr     mod memchr     ● local
    mod  memchr     mod memchr     ● local
    …seven times…

They are presumably `memchr::arch::x86_64::memchr`, `memchr::arch::aarch64::memchr`,
`memchr::arch::generic::memchr` and friends — genuinely different modules that
share a leaf name. The row renders the leaf name and the kind, and nothing that
distinguishes one from another, so the list is unusable for choosing between them.

docs.rs shows the full path in search results for exactly this reason.

The data is present — `SymbolHead.breadcrumb` carries the ancestor chain — it is
simply not shown in the hit row. Note this interacts with L18: the breadcrumb on
the symbol page renders `memchr › memchr › memchr`, so whatever produces the
ancestor chain may itself need attention before it can disambiguate anything.

**Status:** OPEN.

---

## L14 — Every `#[cfg(feature = "…")]`-gated item is missing from the IR entirely

**Blast radius:** enormous, and worse than it first sounds. This is not a missing
badge — it is missing *API*. Any public item gated on a cargo feature is invisible
to the producer, the engine, search, the graph, and the GUI. Our entry counts are
undercounts of the real public surface of essentially every crate.

**Evidence:** found by instrumenting `Crate::cfg(db)` during a real `memchr`
lowering. rust-analyzer's `CfgOptions` for the loaded workspace contains real
`target_*`, `unix`, and `panic=unwind` atoms, but **zero `feature = …` atoms** —
even though `memchr` declares `default = ["std"]` (which implies `alloc`).
Consequently items such as `memmem::FindIter::into_owned` never appear in the
lowered IR at all.

**Diagnosis:** the cargo config the producer builds does not enable any features
when loading the workspace, so rust-analyzer evaluates every `cfg(feature = …)`
as false and prunes those items before we ever walk them. The fix lives in
`build_cargo_config` / `LoadCargoConfig` in
`workspace/compiler/languages/rust/src/ra/loaded.rs` — the same file as L1.

**Note this interacts with L10:** if feature-gated items are uniformly pruned, the
identical 11 329 entry counts across three `memchr` generations become easier to
explain and harder to trust as evidence of lineage.

**Status:** OPEN. Probably the highest-value correctness fix outstanding.

---

## L22 — RESOLVED: a second render root deleted the ancestor context stack

> ### ✅ RESOLVED, 2026-08-05. GUI suite: **380 passed / 0 failed**, stable over 5 consecutive runs.
>
> **Root cause — and it was not where anyone looked.** `SymbolPage::render`
> produced **more than one root element**. Its `ColdError` early-return built a
> second bare root with no `key_context` and no `track_focus`.
>
> GPUI attaches a view's `FocusHandle` to the dispatch tree only where
> `track_focus` is called, and `Window::focus_node_id_in_rendered_frame`
> **silently falls back to the window root** when the focused handle is absent
> from the last rendered frame — a root carrying no key context at all. So a
> *leaf* view taking one render branch deleted the **entire ancestor context
> stack** for the focused element, `Pane` included. `cmd-W` was bound in the
> `Pane` context and handled on `Pane`'s own div two levels up.
>
> This explains every confusing observation recorded below: `close_active` was
> never called, yet `cmd-1` reached `Pane` fine, and the focus and keymap
> contexts both audited clean. It also explains why the L16 fix "caused" it —
> before L16 nothing focused the page, so nothing depended on the page attaching
> its handle. The fix did not introduce the bug; it revealed a latent one.
>
> **Fixed by removing the possibility, not the instance:** `render` is split
> into `page_root` (sole producer of the root, applies chrome once) and
> `page_body` (owns every `PageState` branch, present and future, and is handed
> no root). No future branch can drop the attachment because no branch is in a
> position to.
>
> Recorded in `AGENTS-DOCTRINE.md` §8 under GPUI — it is a general hazard, not a
> fact about this view.
>
> **A real user bug was found alongside it.** `scrim_click_dismisses_omni_search`
> was flaking 20–25%. The search panel's `max_h(relative(0.60))` had no definite
> containing-block height, so the clamp was silently dropped and an animation
> spring's 9999 px sentinel became the panel's actual layout height. The panel
> is `.occlude()`d, so its hitbox covered the whole window and there was no
> scrim left to click. 25/25 after the fix, with a new test that fails
> deterministically when the bug is reintroduced.

## Superseded diagnosis — kept for the reasoning trail

**Blast radius:** two previously-green adversarial tests. Must not be allowed to
land.

**Evidence:** the GUI adversarial suite was **23 passed / 0 failed** earlier in
this session. It is now **21 / 2**:

```
scrim_click_dismisses_omni_search ... FAILED
  clicking the scrim must close the OmniSearch overlay
  left: Some(OmniSearch)   right: None

pane_tab_activation_keeps_store_and_pane_in_sync ... FAILED
  cmd-w must close the active pane tab
  left: 2   right: 1
```

**Cause — this is the L16 fix's cost, and it is instructive.** `Pane::add_item`
was changed to route through `activate_ix`, which now calls `window.focus` on the
newly activated item so the item's own `.on_action` handlers become reachable.
That is the correct shape of the L16 fix. But `dispatch_action` walks the
ancestor chain of the focused node, so moving focus *into* the item removes
`Pane` from that chain unless the item's `FocusHandle` is genuinely a
**descendant** of the pane's in the focus tree. `cmd-w` (`CloseTab`) and the
scrim's click handler are both pane/shell-level, and both stopped arriving.

**The fix is not to revert.** Reverting restores these two tests and restores
L16 — six actions unreachable by any keystroke. The correct outcome is a focus
tree where the item is focused *within* the pane, so item handlers fire first and
unhandled actions still bubble to `Pane` and `Shell`. GPUI supports this; the
handles have to be parented rather than swapped.

**Note the two failing tests are doing exactly their job** — they are the reason
this was caught within minutes rather than shipping. Do not weaken them.

**Status:** RESOLVED — see the banner at the top of this entry.

---

## L17 — Unresolved intra-doc links leak raw markdown to the reader

**Blast radius:** every rendered doc page. It is the difference between "some
links work" and "links work".

**Evidence:** `.shots/memchr/08-symbol-opened.png`, the doc prose for
`struct Memchr<'h>`. In one sentence:

> This iterator is created by the **memchr_iter** or **[memrchr_iter]** functions.
> It can also be created with the **Memchr::new** method.

`memchr_iter` and `Memchr::new` render as real underlined hyperlinks. `memrchr_iter`
renders as the literal text `[memrchr_iter]` — brackets and all — styled as inline
code. So when a doc link fails to resolve, the fallback emits the *markdown source*
rather than plain text.

**Diagnosis:** the resolver in `crates/nudox-engine/src/chunk/walk/doc_link_table.rs`
builds its table from `Symbol::doc_links` (producer-resolved). Links it cannot
match fall through to a path that preserves the `[...]` delimiters. Two separate
questions, and both matter:
1. Why did `memrchr_iter` not resolve when `memchr_iter` did? Both are free
   functions in the same crate. That asymmetry is the more interesting bug.
2. Whatever the answer, the *fallback* must never show markup. An unresolved link
   should render as plain text (or as a visibly-inert styled span), never as
   `[name]`.

**Status:** CLOSED 2026-08-06. Both questions are answered.

**Answer to (1) — it was never a resolution failure.** The producer resolved
`memrchr_iter` perfectly well; the *renderer* never asked. The source reads
`` `[memrchr_iter`] `` with the backtick and bracket transposed, and CommonMark
binds code spans tighter than link brackets, so pulldown-cmark emits
`Code("[memrchr_iter")` + `Text("]")` — two tokens that the old event-stream
lookahead did not recognise as a link attempt at all. Nothing was unresolved.
The asymmetry was between two *spellings*, not two symbols.

**Answer to (2) — met, and pinned.** `consume_bracket_run` is now the single
place that decides what a bracket run shows the reader, and every branch of it
emits ordinary `InlineRun`s. `assert_no_leaked_brackets` in
`chunk/walk/tests.rs` asserts no run of *any* kind contains a `[` or `]`, across
the whole L17 adversarial suite.

**What replaced it.** Rendering that transposed spelling as a link is a repair,
not a resolution — and for a while it was a *silent* one, indistinguishable in
the wire protocol from a link the author spelled correctly. That is now L46,
which owns the repair rather than merely performing it.

---

## L18 — Breadcrumb repeats the package name three times

**Blast radius:** cosmetic, but on the most-viewed surface in the product.

**Evidence:** `.shots/memchr/08-symbol-opened.png` shows the breadcrumb
`memchr › memchr › memchr` for `struct Memchr<'h>`. The crate is `memchr`, the
root module is `memchr`, and the type is `Memchr` — so the chain is arguably
"correct" and still useless. Compare docs.rs, which renders `memchr::Memchr`.

Related: the "ON THIS PAGE" outline for this struct lists only "Documentation",
despite the page reporting `Implementations 6`. Either the outline should include
the structural sections or its heading overpromises.

**Status:** OPEN.

---

## L15 — Keybindings exist, are documented, and do nothing

> ### Partially RESOLVED, 2026-08-05. The six named here are closed; seven more were found.
>
> **Implemented** (both were real handlers that could never receive the key,
> because the view was never focused — the same class as L22, fixed by a shared
> `present_overlay` path):
> - `OpenCommandPalette` — test asserts `overlay_kind_on_top() ==
>   Some(CommandPalette)`, depth 1, and that escape closes it.
> - `ToggleShortcutsOverlay` — tests cover open, second-press-closes, and
>   palette-over-cheat-sheet replacing rather than stacking.
>
> **Deleted, because implementing them would have meant inventing behaviour:**
> - `NavigateBack` / `NavigateForward` — `stores::nav::NavHistory` is complete
>   and tested, but **nothing constructs it, pushes to it, or subscribes to
>   `Navigated`**. There is no sequence to walk. Any implementation would have
>   been a stub wearing back/forward's name.
> - `ToggleHud` — `crate::perf` is a one-line skeleton, and the binding was
>   scoped to `"DebugMode"`, a context **no element has ever declared**, so it
>   could not have dispatched even with a handler.
> - `DeepLinkLine` — its binding was already gone (context
>   `"SymbolPage > SourceTab"` never existed); only the action declaration
>   remained, still enumerable by the palette.
>
> **Seven more of the same defect, newly disclosed rather than quietly left:**
> `f`/`e`/`p` on `GraphView` and `d`/`left`/`right`/`tab` on `PackageBrowser` are
> bound and inert because those views do not exist. They sit behind a
> `PENDING_VIEWS` allowlist in the new `every_context_is_declared_somewhere_in_the_view_tree`
> test. **That allowlist is the thing to watch** — it is a legitimate guard today
> and becomes a hiding place the moment anyone adds to it without a view.
>
> **The structural fix that matters more than any individual binding:** a new
> test asserts every declared key context is actually declared somewhere in the
> view tree. That is what makes this class of defect self-reporting instead of
> requiring a human to press every key.

**Blast radius:** the keymap's central invariant. `main.rs` states it explicitly:
"`app::keymaps` is the single source of truth: the `?` cheat sheet and the command
palette both render from the same registry … so a binding cannot exist without
being discoverable and cannot be documented without working." That is false today.

**Evidence:** these are bound in `workspace/gui/src/app/keymaps.rs` and have **zero**
handlers anywhere in `workspace/gui/src/` (grep excluding `actions.rs`/`keymaps.rs`):

| Binding | Action | Handlers |
|---|---|---:|
| `cmd-shift-p` / `ctrl-shift-p` (keymaps.rs:113,120) | `OpenCommandPalette` | 0 |
| `?` (keymaps.rs:212) | `ToggleShortcutsOverlay` | 0 |
| `shift-l` (keymaps.rs:640) | `DeepLinkLine` | 0 |
| — | `NavigateBack` | 0 |
| — | `NavigateForward` | 0 |
| `f12` (DebugMode context) | `ToggleHud` | 0 |

Confirmed empirically: frames `18-command-palette`, `19-shortcuts-overlay` and
`15-deep-link-line` in `.shots/memchr/` are byte-identical to the frame before
them (`changed=0`).

`OpenCommandPalette` and `ToggleShortcutsOverlay` are dead in a particularly
complete way: both name **real `OverlayKind` variants** (`CommandPalette`,
`Shortcuts`), so the overlay type exists — but nothing anywhere pushes either
variant onto `Shell`'s overlay stack. `shell.rs` only ever pushes
`OverlayKind::OmniSearch`; the other two variants appear solely in the enum
definition and in comments.

`NavigateBack`/`NavigateForward` are the same class and are the more damaging
pair in daily use — a documentation browser without back/forward is a browser
you cannot browse. They were found by grep rather than by a frame, so they are
listed here without screenshot evidence.

The `?` case compounds the others — the cheat sheet is the mechanism by which a
user would *discover* that the command palette exists, and it does not open either.

**Status:** OPEN. Either implement them or remove the bindings; a documented
binding that does nothing is worse than an absent one.

---

## L16 — View-scoped actions do not reach handlers in the screenshot harness

**Blast radius:** evidence quality. It makes seven frames worthless as proof and,
until fixed, blocks demonstrating source-jumping, references and lineage — three
things the product goal names explicitly.

**Evidence — MD5 of every frame in `.shots/memchr/`, after a re-run:**

```
08-symbol-opened.png     16b69ff7  ┐
09-docs-tab.png          16b69ff7  │
10-source-tab.png        16b69ff7  │  EIGHT byte-identical frames
11-refs-tab.png          16b69ff7  │
12-timeline-tab.png      16b69ff7  │
13-version-picker.png    16b69ff7  │
14-copy-symbol-uri.png   16b69ff7  │
15-deep-link-line.png    16b69ff7  ┘

17-bottom-dock-open.png  1928adff  ┐
18-command-palette.png   1928adff  │  three more
19-shortcuts-overlay.png 1928adff  ┘
```

Ten of the twenty-six frames are redundant. (`05` and `07` also match, at
`13995633`, but that pair is *correct*: selection moved down and back up, so the
identical frame is the assertion succeeding.)

Unlike L15 these actions DO have handlers (2–4 references each), so the dispatch
simply never arrives. An attempt to fix this from the test harness alone was made
and did not move any of the eight frames — which is the expected outcome given the
diagnosis below, and is itself the confirmation that this is a product bug.

**Diagnosis — this is a PRODUCT bug, not a harness bug.** Confirmed both
empirically and structurally:

`SymbolPage` **never calls `window.focus` and implements no `Focusable`** — there
is no `.track_focus` or `Focusable` anywhere in
`workspace/gui/src/views/symbol_page/*.rs`. GPUI's `dispatch_action` only walks
the ancestor chain of whatever is *currently focused*, so handlers registered on
`SymbolPage`'s own root `div` can never fire — not from this harness, and **not
from a real keystroke either**. The handlers exist and three of them
(`GoToSourceTab`, `GoToRefsTab`, `OpenVersionPicker`) mutate real visible state;
they are simply unreachable.

The control case proves it is specific to `SymbolPage` rather than a shell-wide
dispatch problem: frame `25-activate-tab2` works, because `ActivateTab2` is
handled on `Pane`'s root and `Shell::close_overlay` explicitly refocuses the pane.

**Two of the six are dead for additional, independent reasons:**
- `GoToTimelineTab`'s handler body is literally `let _ = page; cx.notify();`,
  sitting under a TODO stating the four `GoToXTab` bindings *and*
  `NextInnerTab`/`PrevInnerTab` should be removed from the keymap **because the
  symbol page has no separate tabs**. The page renders Implementations /
  References / Source as collapsible sections, which is what the screenshots
  actually show. GUI-PLAN §16 describes inner tabs; the implementation chose
  sections and the keymap was never updated. So frames 09–13 were photographing a
  feature that does not exist.
- `CopySymbolUri` writes the clipboard but never calls `cx.notify()`, so even with
  focus fixed it would repaint nothing — a real action with no visual feedback.

**Also:** these frames passed because they were captured with
`expect_change: false`. That escape hatch was intended for steps that genuinely
may not repaint; used here it converted a silent failure into a green run. See
`SHOT-REVIEW.md` R1.

**Status:** OPEN. The fix is `Focusable` on `SymbolPage` plus a decision on
whether the tab actions should exist at all.

---

## L4 — `bytes` cannot be lowered: duplicate declaration

**Blast radius:** unknown, but this class (a name declared twice) will recur.

**Evidence:** `CORPUS-REPORT.md` — `bytes-1.11.0` fails with
`declared more than once: ["bytes", "bytes::Bytes"]`.

**Diagnosis:** same *family* as the `#[macro_export]` bug already fixed (L11): the
walk reaches one item by two routes and declares it twice. The macro case was
fixed by routing declarations through the existing `id_of()` authority; this is
likely a second route that still bypasses it, or a re-export (`pub use`) being
declared as if it were a definition.

**Status:** OPEN.

---

## L5 — `serde_json` times out (>300 s)

**Blast radius:** large or heavily-generic crates may be unlowerable in practice.

**Evidence:** `CORPUS-REPORT.md` — timeout at the 300 s harness budget.

**Diagnosis:** unclear whether this is L1 overhead scaled up, a pathological type
in the HIR walk, or a genuine hang. **We do not currently know**, because the
harness records "timeout" without a profile. Needs a bounded investigation before
it is assumed to be "just slow".

**Status:** OPEN, undiagnosed.

---

## L6 — RESOLVED: the `index` crate compiles, and its 731 tests run

> ### ✅ RESOLVED, 2026-08-05. `cargo check -p index --lib`: **0 errors**. `cargo test -p index`: **731 passed, 0 failed**, plus 12 further binaries all green.
>
> Those tests had **never executed once** in this project's history.
>
> **16 of the 17 errors were mechanical**, each verified individually rather than
> assumed:
> - a module file misnamed `lib.rs` instead of `mod.rs` (`transport/`)
> - the `zip` crate used but absent from `Cargo.toml` — 13 errors, for an API
>   that was already being called correctly
> - 2 call sites stringifying an error the target variant already declared as a
>   typed `#[source] postcard::Error`
> - 4 sites using `format_args!` where `format!` was needed for a
>   `Box<dyn Error>` bound
>
> **Exactly 1 of 17 was genuine unfinished work**: a missing hand-curated
> `cpp_alias_seed.ron` (≥60 real alias→stem mappings) which **cannot be
> mechanically fabricated** without violating doctrine §6's ban on fake fixtures
> standing in for real data.
>
> **`ir-vcs` was the same story.** 115 errors, of which **110 were a single
> missing import** — `pijul_err` exists at `workspace/ir/vcs/error.rs:19` and
> was simply absent from two `use` lines that already imported `VcsError` from
> the same module. Adding it to both took 115 → 6.
>
> **The lesson is about the gate, not the code.** This crate was excluded from
> every build command precisely *because* it did not compile, so nothing ever
> re-checked whether that was still true. A crate that is excluded from the gate
> stops being measured, and a one-character module-name typo can then survive
> indefinitely behind a reputation for being "unfinished". Doctrine §7's exclude
> list is a tourniquet, not a diagnosis — re-test the excluded crates
> periodically.
>
> **Still genuinely blocked, unchanged:** `dolt-engine` is **code-complete** —
> compiling it adds zero new errors beyond the 17 — but needs the ~20 MB vendored
> `doltlite.c` amalgamation that is not in this checkout. That is a build-asset
> gap, not a code gap, and `test-engine` remains explicitly documented as
> "NEVER a product mode: its versioning calls are honest fakes."

## Superseded — kept for the reasoning trail

**Blast radius:** the entire server-side catalog plane. Off the GUI path, so it
did not block this session's goal, but `cargo check --workspace` cannot be green
until it is fixed.

**Evidence:** its errors were hidden behind a vendor build-script failure. With
the vendor packages excluded, `index` surfaces: `mod transport` unresolved
(`workspace/index/transport/` has 9 tracked files but no `mod.rs`); `zip` used but
absent from `Cargo.toml`; `Arguments<'_>: Into<Box<dyn Error>>` errors in
`pack/reader.rs`; a missing `assets/cpp_alias_seed.ron`. Confirmed pre-existing —
`git ls-files` shows the transport files tracked with no module root, and the root
`Cargo.toml` comment claims a fold into `index::transport` that was never done.

**Status:** OPEN. Pre-existing, not a regression from this session.

---

## L7 — Vendored C sources are absent from the checkout

**Blast radius:** `qdrant-edge` (vector store, `registry`'s `local` feature) and
`doltlite`/`rusqdoltlite` (versioned SQLite catalog) cannot be built.

**Evidence:** `workspace/vendor/qdrant-edge/cpp/` does not exist;
`workspace/vendor/doltlite/doltlite.c` does not exist (deliberately — see its
`VENDORING.md`). Both packages are now excluded from the workspace so they no
longer abort `cargo check --workspace`; `qdrant-edge`'s build script warns loudly
rather than skipping silently.

**Note:** `build_quantization.rs` is now the only *code* divergence from upstream
in `workspace/vendor/` with no recorded patch. `libpijul` sets the convention
(`libpijul-fork.patch`); qdrant-edge should follow it if it accumulates a second
change.

**Status:** MITIGATED (build unblocked), sources still absent.

---

## L8 — No remote / upstream package store; everything is a local checkout

**Blast radius:** the product is currently "point it at a directory". docs.rs has
every crate ever published; we have whatever is on disk.

**Evidence:** the only corpus sources are `FixtureSource` and `ProducerSource`
over local `PackageSource { root }` paths. `.real-crates/` is populated by copying
out of the local cargo registry cache.

**Vector:** a content-addressed upstream store over IPLD — packages and their IR
addressed by hash, fetched on demand, verifiable, and shareable between
instances. The IR is already content-addressed in spirit (`ContentHash`, BLAKE3,
`PristineIntroTable::seal`), and `heart::sync` already exists as an iroh-free
seam, so the shape is there. Nothing is wired.

**Status:** OPEN, not started. Large.

---

## L9 — Corpus fixtures are not Nix-pinned

> ### Largely RESOLVED, 2026-08-05 — with one disclosed gap.
>
> `corpus/manifest.toml` now carries **140 packages, 20 in each of 7
> ecosystems**, 154 version entries, every one hash-pinned:
>
> | ecosystem | packages | versions | multi-version |
> |---|---:|---:|---|
> | crates.io | 20 | 23 | log, memchr |
> | go | 20 | 22 | pkg/errors, stretchr/testify |
> | npm | 20 | 22 | lodash, zod |
> | pypi | 20 | 22 | click, pydantic (1.x→2.x) |
> | maven | 20 | 22 | guava, jackson-databind |
> | nuget | 20 | 22 | Newtonsoft.Json, Serilog |
> | cpp | 20 | 21 | nlohmann-json |
>
> `corpus/fetch.nu` was rewritten from crates.io-only and **hash-blind** into a
> multi-ecosystem fetcher that verifies every hash *before* extraction and exits
> non-zero on mismatch — proven with a deliberate wrong-hash negative test.
> Source, not binaries, wherever a convention exists: Maven **sources** jars,
> PyPI **sdists** (resolved via the JSON API rather than guessed filenames), Go
> module zips with Cargo-style path case-escaping.
>
> All 133 new packages fetched on the first attempt; a second run `SKIP`ped all
> 154, confirming idempotence. Disk grew 1.5 G → 2.1 G (+505 MB). Nothing was
> dropped for disk.
>
> **Every hash was computed from real downloaded bytes** and cross-checked
> against `nix-prefetch-url` across all seven ecosystems, so the from-scratch
> Nushell base32 implementation is verified correct rather than merely
> self-consistent.
>
> **Two honest gaps, disclosed not hidden:**
>
> 1. **`flake.nix`'s `buildCorpus` still handles only crates.io**, so
>    `nix build .#corpus` builds one slice of the manifest. `fetch.nu` is the
>    complete path. PyPI needs a live JSON lookup and cpp needs raw-file
>    handling, neither of which maps cleanly onto a static-URL derivation.
> 2. **NuGet fetches compiled binaries plus `.nuspec`, not C# source** — no
>    NuGet-wide sources convention exists. This breaks the source-not-binaries
>    pattern the other six ecosystems follow, and matters directly for whether
>    the C# producer can use these fixtures.
>
> **Bug found and fixed along the way:** `fetch.nu`'s `[workspace]`-append used
> Nushell's *list* `append` rather than `save --append`, so it had **never
> written anything**. Every fixture that has the table got it from `flake.nix`'s
> separate shell append. Recorded in doctrine §8 — a step that looks correct,
> errors nothing, and does nothing.

**Blast radius:** reproducibility. The corpus is whatever happens to be in the
host's cargo registry cache.

**Evidence:** `.real-crates/` is materialised by `cp -R` from
`~/.local/share/cargo/registry/src/index.crates.io-*/`. No hashes are recorded and
no fetcher is declared in `flake.nix`.

**Status:** OPEN.

---

## L10 — Lineage is proven only for `memchr`, and only three generations

> ### ⚠ REOPENED. The "innocent" verdict below was wrong.
>
> The identical 11,329 across all three generations was **not** the expected
> outcome — it was an artifact of the `--no-deps` degradation (**L28**). All
> three were being measured as the same feature-less shell, which is why they
> agreed exactly. With dependencies resolving they split:
>
> | memchr | degraded | resolved |
> |---|---:|---:|
> | 2.7.6 | 11,329 | **902** |
> | 2.8.0 | 11,329 | 11,361 |
> | 2.8.3 | 11,329 | 11,361 |
>
> A 12× gap between 2.7.6 and 2.8.0 cannot be explained by a three-item API
> delta, so **one of those numbers is a real defect** and lineage is unproven
> until it is settled.
>
> ### ✅ SETTLED, and lineage is now PROVEN end to end.
>
> Both defects were L38's. With them fixed the three generations read **1,322 /
> 1,325 / 1,325** — a delta of exactly 3, matching the `*_owned` builders found
> by source diff. The arithmetic finally agrees with the source.
>
> **Proven in the GUI, not just in a test.** `.shots/memchr/13-version-picker.png`
> shows the version popover **open with 2.8.3 / 2.8.0 / 2.7.6 all selectable**,
> and the strip reading `2.8.3 current · 2.8.0 unchanged · 2.7.6 present`.
> Selection is real: `SymbolEngine::select_version` → `EngineHandle::select_version`
> → re-stream.
>
> Two reasons it had never been captured before, both now fixed:
> 1. The suite's readiness probe stopped at the **first** generation, so a
>    one-version corpus was being photographed regardless of how many roots were
>    passed.
> 2. The popover needed `gpui::deferred` — **GPUI has no z-index**, so later
>    siblings were painting straight over it. It had been rendering correctly and
>    invisibly the whole time, which is why the frame read as byte-identical.
>
> **Why this was missed, because the reasoning is worth keeping.** Every fact
> in the analysis below is *true* and was independently verified — the trees do
> differ, the API delta really is three `alloc`-gated methods, and L14 really
> does drop feature-gated items. The error was accepting an explanation because
> it fit the observation, without asking what *else* could produce agreement
> that exact. A count identical to the digit across three inputs is stronger
> evidence of a common upstream cause than of a coincidence, however well
> motivated. Doctrine §8 now carries this as a standing rule.
>
> Note also that the analysis explained the agreement via L14 (feature-gated
> items missing). That was directionally right and causally wrong: features
> were not merely *gated out*, they were never evaluated at all, for every
> crate in the corpus.

**Blast radius:** the version/timeline feature.

**Superseded analysis follows — facts sound, conclusion wrong.** Investigated by
`diff -rq` over the three real checkouts before writing any test:

- The trees are genuinely different, not one tree under three labels. 2.7.6→2.8.0
  differs in `cow.rs` and `memmem/mod.rs`; 2.8.0→2.8.3 in `arch/all/{rabinkarp,twoway}.rs`,
  `arch/generic/{memchr,packedpair}.rs`, `arch/x86_64/*`, `vector.rs`,
  `memmem/{mod,searcher}.rs`. Each `Cargo.toml` version matches its label.
- Public-API diff: 2.7.6 has 126 uniquely-named `pub` items; 2.8.0 and 2.8.3 have
  129 — three added `*_owned` builder methods.
- **All three additions are `#[cfg(feature = "alloc")]`-gated.**
- Every remaining diff is a statement edit *inside* an existing function body —
  `.get(0)`→`.first()`, an added `unreachable_unchecked` hint, a rewritten bounds
  check, a dropped `.clone()`, an endian-cfg split. No header, signature or doc
  changed.

~~So a byte-identical entry count across these three releases is the *expected*
outcome.~~ **This inference is retracted — see the banner.** The trees and API
deltas above are real, but they are not what produced the identical counts.

Registry-level lineage was separately confirmed live against the running
engine: `start_with_versions` over the three real checkouts, three versions
resident, `VersionEvent::Switched` observed, and the same `SymbolKey` still
resolving after a switch.

**But this exposes a causal chain worth naming: L14 is what prevents lineage from
being *demonstrable* here.** The only producer-visible API difference between
these generations is the three `alloc`-gated methods — and because the producer
loads rust-analyzer with zero cargo features active, those items do not exist in
any generation's IR. Fixing L14 should make this fixture show a real content-level
version difference for the first time.

**Better fixture available meanwhile:** `log` 0.4.17 lowers to 390 entries and
0.4.33 to 419 — a 29-entry difference that is already visible today. That is the
pair to use for a content-level lineage demonstration until L14 lands.

**Status:** PARTIALLY RESOLVED. Registry-level lineage verified against real
generations. Content-level lineage not yet demonstrated on `memchr` (blocked by
L14) and not yet attempted on `log`.

---

## L11 — `#[macro_export]` crates could not be lowered — RESOLVED

**Was:** any crate using `#[macro_export]` failed with a paired
"duplicate RaId emitted" / "referred but never declared" error, because the
declaration and reference sites computed macro ids by different routes
(crate-root path vs defining-module path).

**Fix:** the `ModuleDef::Function/Const/Static/Macro` arms of `lower()` now route
through the existing `id_of()` authority — which was already documented as "called
at both declaration sites and reference sites so the two can never diverge" and
simply was not being called.

**Evidence:** `log-0.4.33` now lowers to 419 entries; `log-0.4.17` to 390;
`memchr` unchanged at 11 329 (no regression).

---

## L12 — Panel titles were illegible in the dark theme — RESOLVED

**Was:** "Project" and "Editor" rendered at near-background contrast in every
frame. Root cause: gpui-component's `Panel::title_style()` defaults to `None`, and
`TabPanel::render_title_bar` only applies `.text_color(...)` `when_some(...)` — so
with no style the title inherited a colour never chosen for that surface.

**Fix:** `NudoxThemeExt::panel_title_style()` — one themed definition of the
panel-header pairing.

---

## L13 — Screenshot evidence was self-contradicting — RESOLVED (harness)

**Was:** frames 01/02 byte-identical and 03/04 near-identical while carrying
captions claiming distinct states; the `expect_change > 0` guard passed on a
~12 000-pixel caret blink out of 5 184 000.

See `SHOT-REVIEW.md` R1–R3 for the full finding and the acceptance criteria.

---

## L21 — Vendored `trustfall/` is untracked and carries a nested `.git`

**Blast radius:** the next commit. Not a runtime problem; a repository-integrity one.

**Evidence:** `git status` reports `?? trustfall/` (21 MB), and `trustfall/.git`
exists. Committed as-is, git records a **gitlink** — a bare commit pointer with no
`.gitmodules` entry — so a fresh clone gets an empty directory and a build that
fails for a reason nobody will connect to this.

**The vendored copy is correct otherwise**, and was checked: it sits at commit
`7e71e35715dfac561e3453c834b469a200330997`, which is **exactly** the rev the root
`Cargo.toml` already pins for `trustfall = { git = "…philocalyst/trustfall", rev = … }`.
It carries both features we fork upstream for — `Adapter::Error`
(`trustfall_core/src/interpreter/mod.rs:533`) and the `AsyncAdapter` stream engine.
So switching the git dep to `{ path = "trustfall/trustfall", features = ["async"] }`
is semantically a no-op and buys local editability.

**Three ways to resolve, pick one deliberately:**
1. Follow the existing convention — move it to `workspace/vendor/trustfall`,
   delete the nested `.git`, and record a regenerable fork patch the way
   `workspace/vendor/libpijul-fork.patch` does. Most consistent with the repo.
2. Keep it at the top level, delete the nested `.git`, commit the 21 MB.
3. Make it a real submodule with a `.gitmodules` entry (the repo already has one,
   for `linkml`).

**Do not** simply `git add trustfall/` — that silently takes option (0), the
broken one.

**Status:** OPEN. The dependency switch is deferred until the current agent wave
lands, because changing a dependency's source kind rebuilds every downstream crate
and would invalidate in-flight baselines.

---

## L24 — RESOLVED: `nudox-producer-clang` no longer link-depends on libclang at all

> ### ✅ RESOLVED, 2026-08-05. 20/20 tests passing, and they now actually run.
>
> **Root cause, reproduced exactly as described below:** without `LIBCLANG_PATH`,
> `clang-sys`'s build script falls back to Xcode CLT's
> `libclang.dylib`, whose Mach-O install name is `@rpath/libclang.dylib`. Cargo's
> default dynamic link never emits an `-rpath`, so the binary carried that load
> command with **zero `LC_RPATH` entries**.
>
> **The fix is stronger than the rpath patch this entry proposed.** Switching
> `clang-sys` to its `runtime` feature makes `clang::Clang::new()` do a real
> `dlopen`, eliminating the link-time dependency **entirely**. After:
>
> ```
> otool -L : only libiconv + libSystem — no libclang reference at all
> otool -l | grep LC_RPATH : (empty — none needed)
> ```
>
> An `-rpath` would have fixed the binaries someone remembered to patch. This
> fixes every binary that ever links the crate, present and future — doctrine §2
> applied to a linker problem. Verified on a genuine downstream consumer in a
> separate crate, not just the crate's own test harness.
>
> It also makes the module's doc comment **true for the first time**: doctrine §8
> already recorded that it "documents a lazy-`dlopen` failure (it is a hard
> link-time one)". The code moved to match the comment rather than the reverse.
>
> `flake.nix` still pins `LIBCLANG_PATH` from `pkgs.libclang.lib` so *which*
> libclang is loaded stays reproducible — but it is no longer required for
> correctness on macOS.
>
> **`cargo nextest` no longer aborts with signal 6** when enumerating this crate,
> regardless of environment. TESTING.md's instruction to export `LIBCLANG_PATH`
> and `DYLD_LIBRARY_PATH` before running nextest is now obsolete for this crate.
>
> ### Two real bugs the silent-skip had been hiding
>
> `try_clang()` returned `None` and printed `SKIP` when libclang was
> unavailable — a suite that could not fail (doctrine §8). Replacing it with
> `require_clang()`, which panics by default with a loud
> `NUDOX_ALLOW_CLANG_TEST_SKIP=1` opt-out, immediately exposed:
>
> 1. **`clang::Clang` is a process-wide singleton, not per-thread.** All 11
>    libclang tests raced for one slot under cargo's parallel harness, and every
>    loss was silently absorbed as "skip". Serialised with a `Mutex`.
> 2. **`entity.get_arguments()` returns `None` for an uninstantiated
>    `FunctionTemplate`** — `clang_Cursor_getNumArguments` only answers for
>    concrete declarations — so `template_function_type_var` was silently broken.
>    Now falls back to filtering the cursor's `ParmDecl` children.
>
> ### `compile_commands.json` ingestion landed, and it found a third bug
>
> Relative `-I`/`-isystem`/`-iquote`/`-idirafter` paths were **present but
> inert**: `clang::Parser` has no notion of the database's working directory, so
> libclang resolved them against the *test process's* cwd. They are now rewritten
> to absolute, anchored at each entry's `directory`.
>
> Measured on a multi-file C project with a shared header: **without** the
> database, 0 functions and 0 records extract (both signatures name types
> declared only in the unreachable header, so libclang marks the declarations
> invalid). **With** it, both extract correctly. `CMakeLists.txt` is deliberately
> not parsed — `compile_commands.json` is the tool-agnostic artifact CMake, Bear,
> Meson, and Bazel all already emit.
>
> **`WEAKENED:`** the C/C++ "real package" is a hand-authored multi-file project,
> not a vendored upstream OSS package — this repo has no C/C++ fixture
> convention yet, and a hermetic fixture was chosen over inventing one. The
> `NUDOX_ALLOW_CLANG_TEST_SKIP=1` opt-out remains, as a loud non-default.

## Superseded diagnosis — kept for the reasoning trail

**Blast radius:** C/C++ is unreachable (see L2), and worse, the crate's mere
presence as a workspace member breaks test enumeration for unrelated crates.

**Evidence:** `ProducerRegistry::with_all_available` deliberately does not register
a clang producer. The precise mechanism, established with `otool`:

> the linked binary references `@rpath/libclang.dylib` and has **zero `LC_RPATH`
> load commands**, so dyld aborts the process at load time, before any code runs.

Confirmed via `otool -L` and `otool -l | grep LC_RPATH` on the linked
`nudox-engine` test binary. **This is a hard link-time defect in the
`clang`/`clang-sys` build script on this host — NOT the lazy-`dlopen` failure the
crate's own doc comment claims.** `ClangProducer` implements `Producer` correctly
and `cargo check -p nudox-producer-clang` is green; it simply cannot be linked
into anything.

It also takes down unrelated crates: `nudox-producer-clang` is a workspace member
with its own tests, so its test binary aborts the moment `cargo nextest list`
enumerates it, and `nudox-engine`'s `impls_refs_flows` binary goes with it.

**The transferable lesson (now in AGENTS-DOCTRINE §8): `cargo check` is a
necessary but insufficient gate for any crate with C FFI bindings, because it
never links.** This was caught only by running the full test suite after wiring
clang in, seeing the SIGABRT, and reverting.

**Fix options:** an rpath-embedding `build.rs`, or switching `clang-sys` to its
genuine runtime-`dlopen` feature.

A build-time `LIBCLANG_PATH` is not sufficient; `DYLD_LIBRARY_PATH` must also point
at a directory containing `libclang.dylib` at **run** time:

```bash
export LIBCLANG_PATH=$(dirname "$(find /nix/store -maxdepth 2 -name 'libclang.dylib' -path '*clang-*-lib*' | head -1)")
export DYLD_LIBRARY_PATH="$LIBCLANG_PATH"
```

Without it, `cargo nextest list` aborts with signal 6.

**Status:** OPEN, worked around by environment. The workaround is now documented in
`TESTING.md`.

---

## L25 — `cargo nextest` cannot run the workspace without `--exclude`

**Blast radius:** every test invocation, until L6 is fixed.

**Evidence:** because `workspace/index` has never compiled (L6) and `driver` and
`ir-vcs` both depend on it, a bare `cargo nextest list` at the repo root fails at
the `cargo build` step **before any nextest filter runs**. No config can route
around a compile error. With
`--exclude index --exclude driver --exclude ir-vcs` the same command succeeds and
lists **1 166 tests (50 `#[ignore]`d)** across the remaining 16 crates.

**Also:** a single nextest config file cannot govern both Cargo workspaces.
nextest validates every name-matcher against whichever workspace's metadata is
loaded and hard-errors if a matcher matches nothing — in both directions. So
`.config/nextest.toml`'s `gui` group is defined and correct but nothing in that
file can *assign* a test to it; `workspace/gui` needs its own config.

**Status:** OPEN (downstream of L6).

---

## L26 — `sandbox`'s real-VM tests cannot run here

**Evidence:** `tests/escape.rs` (4 cases) and
`smolvm_backend::tests::smoke_real_launch_succeeds_or_typed_unavailable` need
`NUDOX_GUEST_ROOTFS` and a `smolvm`/`libkrun` binary on `PATH`, neither present.
They are `#[ignore]`d so a plain `-P ci` never touches them, but
`--run-ignored all` **attempts and fails them rather than skipping** — unlike the
`real-crate` group, which skips gracefully when a checkout is missing. Their real
wall-clock cost is therefore unmeasured, and their `slow` group placement
(`max-threads = 2`) is a conservative guess, not a measurement.

**Status:** OPEN. The asymmetry with `real-crate` is the actionable part: missing
infrastructure should skip loudly, not fail.

---

## L27 — Bracket interpretation is decided per symbol, not per link attempt

**Blast radius:** doc rendering correctness on any symbol whose doc comment
mixes a real intra-doc link with unrelated bracketed prose. Not every symbol
hits it, but the ones that do render wrong silently — there is no error, just
a `[NOTE]` that quietly loses its brackets.

**Evidence:** `crates/nudox-engine/src/chunk/walk/prose.rs`'s
`is_bracket_open` predicate (and the paired bare-`]` strip) gate the entire
shortcut-link scan on `doc_link_table.has_declared_links()` — a single
`bool` computed once per symbol from `!entry.sym().doc_links.is_empty()`
(`doc_link_table.rs::DocLinkTable::build`). That is coarser than the thing it
is standing in for: whether *this particular* bracket run is a link attempt.
A symbol that declares one doc link anywhere in its documentation has *every*
bracket run in that documentation scanned as a potential shortcut, including
ones with nothing to do with the declared link. Concretely: a doc comment
containing both `` [`SomeType`] `` (a real, resolving intra-doc link) and a
literal `[NOTE]` callout marker will have the `[NOTE]` brackets stripped too,
because the symbol's `has_declared_links()` is `true` for the whole comment,
not just for the `SomeType` span.

**Why this is the right heuristic anyway:** the alternative — always treating
brackets as prose unless a specific bracket run resolves — is what shipped as
L17 (raw `[memrchr_iter]` markdown leaking to the reader) and reintroducing
it is worse than this. Per-symbol is the best granularity available *given
what the producer currently emits*; see the diagnosis below for the actual
fix.

**Diagnosis — the real fix is not in the engine.** `DocLink`
(`workspace/ir/model/src/entry/symbol.rs`) carries only `target: String` and
`label: Option<String>` — no byte span into the doc comment. Nothing
downstream of the producer can therefore say "the doc link for `SomeType`
covers bytes 40..50 of this doc comment, and `[NOTE]` at bytes 5..11 is a
different span that this `DocLink` says nothing about." Fixing this requires
the producer to record the byte span of each link *attempt* in the source
doc comment (not just the resolved target) and threading that span through
to `prose.rs`, so the shortcut scan can ask "is *this* bracket run inside a
recorded link-attempt span?" instead of "did this symbol declare *any* link
anywhere?" **This span-recording does not exist today** — it would be new
work in the Rust producer (and every other producer, for parity) plus a
schema change to `DocLink`, not a fix confined to the engine.

**Also note:** `doc_link_table.rs::DocLinkTable::has_declared_links()`
carries a doc comment claiming it is "No longer used to gate shortcut-link
scanning in `prose.rs`" — that claim is stale. `prose.rs` currently gates on
it at both call sites (`is_bracket_open` and the bare-`]` strip); the comment
describes an earlier, reverted state of the code. Left as-is here because
`doc_link_table.rs` is outside this entry's edit scope; flagging it so the
next reader of that file does not trust it (doctrine §8: a comment
describing behaviour is a claim, and this one is currently wrong).

**Status:** OPEN. Cheap to work around per-comment (nobody currently mixes an
unrelated bracket marker with a real link in the same doc comment in the
corpora exercised so far), expensive to fix properly (producer schema change,
all producers).

---

## L28 — Nearly the entire real-crate corpus was measured under a silent `--no-deps` degradation

**Blast radius:** every number in `CORPUS-REPORT.md` and L1's table except `lazy_static`
and `serde` (the only two fixtures that resolved cleanly beforehand). 19 of 21
corpus entries — every `memchr` generation, `log`, `itoa`, `once_cell`,
`thiserror`, `hashbrown`, `parking_lot`, `bytes`, `indexmap`, `nom`, `regex`,
`serde_json`, `syn`, `tracing`, `libc`, `unicode-width` — were lowered against a
dependency-free crate graph and reported as ordinary successes.

**Mechanism, confirmed empirically.** `cargo metadata --offline` fails in each
fixture with a distinct `error: no matching package named "…" found, location
searched: crates.io index` (or, for a few, `error: failed to download "…"`).
Examples captured directly:

```
$ cd .real-crates/memchr-2.8.3 && cargo metadata --offline
error: no matching package named `rustc-std-workspace-core` found

$ cd .real-crates/log-0.4.33 && cargo metadata --offline
error: no matching package named `sval_derive` found

$ cd .real-crates/thiserror-1.0.40 && cargo metadata --offline
error: no matching package named `trybuild` found
```

In every case the missing package turned out to be a dependency that is real
and legitimate but **irrelevant to the crate's own public API** — an optional
`rustc-dep-of-std` facade (`rustc-std-workspace-core`, `rustc-std-workspace-alloc`,
`compiler_builtins`), or a dev/test-only dependency (`sval_derive`, `trybuild`,
`automod`, `no-panic`, `regex-test`, `owning_ref`, `atty`, `lexical-core`,
`critical-section`, `cc`). `cargo metadata` (unlike `cargo build`) always
resolves the **full** graph — normal, build, *and* dev dependencies together,
because they share one `Cargo.lock` — so a dev-only dependency the host's
cargo cache never happened to fetch is enough to fail the whole resolve.
Confirmed the local registry cache genuinely lacked these: neither an index
cache entry (`~/.local/share/cargo/registry/index/*/​.cache/**`) nor an
extracted source directory existed for `rustc-std-workspace-core` before this
session touched it.

**The silent retry, confirmed against the actual upstream source** (not
in-repo — `ra_ap_project_model` v0.0.341, pulled in transitively via
`ra_ap_load_cargo`; source at
`~/.local/share/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ra_ap_project_model-0.0.341/src/cargo_workspace.rs:806-823`):

```rust
if !output.status.success() {
    let error = cargo_metadata::Error::CargoMetadata { stderr: ... }.into();
    if !no_deps {
        // If we failed to fetch metadata with deps, return pre-fetched result without them.
        // This makes r-a still work partially when offline.
        if let Ok(metadata) = no_deps_result {
            tracing::warn!(?error, "`cargo metadata` failed and returning succeeded
                result with `--no-deps`");
            return Ok((metadata, Some(error)));
        }
    }
    return Err(error);
}
```

`FetchMetadata::new` *always* runs a `--no-deps` pre-fetch up front (line 714,
"useful as a fast fallback to extract info like `target-dir`"), independent of
whether the real fetch is attempted. When the real fetch then fails, this code
substitutes the pre-fetched `--no-deps` result and returns `Ok`, threading the
original error through as a second, easily-ignored tuple field. `--no-deps`
metadata carries no `resolve` section at all, so — as the in-repo comment at
`workspace/compiler/languages/rust/src/ra/loaded.rs:106-120` already
documents — every dependency is absent from the graph and every Cargo
feature, **default included**, evaluates false before the walk ever begins.
`tracing::warn!` is the only signal, and this codebase installs no subscriber
in the producer or its tests, so a degraded load has been silently
indistinguishable from a complete one.

**This was already half-diagnosed and half-fixed in-tree before this session
(uncommitted, `workspace/compiler/languages/rust/src/ra/loaded.rs:106-134`):**
someone added the `ProjectWorkspaceKind::Cargo { error: Some(_), .. }` check,
a `warn!`, and an unconditional `eprintln!("ra_load
metadata_degraded=true …")`. That eprintln fired and was captured directly
during this session's baseline re-run of `memchr-2.8.3` — see below. **It is
still print-only.** `LoadedWorkspace` carries no `metadata_degraded: bool`
field, `RustProducerError` has no variant for it, and nothing stops a caller
from treating the returned table as complete. Per doctrine §2, the correct
fix is a type-level one — `LoadedWorkspace`/`ProduceResult` should carry a
`DependencyResolution { Full, NoDeps { cause: Arc<anyhow::Error> } }` (or
equivalent) that a consumer must inspect, not a stderr line a caller can fail
to read. **Not implemented here — it is a change to producer code, out of
this investigation's scope** (see below).

**Root cause is NOT doctrine §8's `[workspace]` gotcha.** Checked all 21
fixture `Cargo.toml`s directly (`tail -5` on each): every one already carries
the trailing empty `[workspace]` table. That mitigation is intact everywhere;
it is not what is failing here.

**Fix applied — contained, no producer code touched.** For each of the 17
affected fixtures, ran `cargo vendor vendor` (one-time network fetch of only
the previously-uncached packages — confirmed reachable: `crates.io` and
`static.crates.io` both answer; the local sandbox is not actually
network-isolated, only the *producer* is deliberately configured `--offline`)
and added `.real-crates/<crate>/.cargo/config.toml`:

```toml
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
```

Fixed: `bytes-1.11.0`, `hashbrown-0.17.1`, `indexmap-2.13.0`, `itoa-1.0.18`,
`libc-0.2.161`, `log-0.4.17`, `log-0.4.33`, `memchr-2.7.6`, `memchr-2.8.3`,
`nom-5.1.3`, `once_cell-1.20.2`, `parking_lot-0.12.1`, `regex-1.10.3`,
`serde_json-1.0.113`, `syn-1.0.109`, `thiserror-1.0.40`, `tracing-0.1.40`,
`unicode-width-0.1.11`. `memchr-2.8.0` started resolving for free once
`rustc-std-workspace-core` landed in the shared `CARGO_HOME` cache as a side
effect of vendoring `memchr-2.8.3` — confirming the packages were genuinely
just missing from the cache, not blocked by anything structural.
**Verified**: `cargo metadata --offline --format-version=1` now exits 0 with a
populated `resolve` section in all 22 fixture directories (21 crates + the
`log` → `log-0.4.33` symlink). This does not solve L9 (fixtures are still not
Nix-pinned/reproducible across machines) — it makes *this* checkout resolve,
using vendor snapshots that are themselves gitignored artifacts of this host
and this session.

**Headline measurement — `memchr`, all three corpus generations, before vs.
after, same harness** (`NUDOX_PKG_ROOT=… NUDOX_PKG_NAME=memchr
NUDOX_PKG_VERSION=… RUSTC_BOOTSTRAP=1 cargo test -p nudox-store --test
real_package -- --ignored --nocapture`, reading the `lowered N entries`
line):

| Crate | Degraded (`--no-deps`, current baseline) | Resolved (this fix) | Δ |
|---|---:|---:|---:|
| memchr 2.7.6 | 11 329 | **902** | **−10 427 (−92%)** |
| memchr 2.8.0 | 11 329 | 11 361 | +32 |
| memchr 2.8.3 | 11 329 (reconfirmed live: `ra_load metadata_degraded=true package=memchr error=… rustc-std-workspace-core …` fired, then `lowered 11329 entries`) | 11 361 (no degradation line; reproduced twice) | +32 |

2.8.0's and 2.8.3's +32 is the expected L14 story — resolving the graph
activates `default`/`std`/`alloc` properly, unlocking a small, plausible
number of feature-gated items (the `*_owned` builders L10 already found via
source diff). **2.7.6's result is not that story.** A 92% drop, reproduced
twice, is not "a few more feature-gated items appeared" — it is nearly the
entire IR disappearing. 2.7.6's `Cargo.toml` feature table is byte-identical
to 2.8.3's, it has no build script (`build = false` in both), and L10 already
established the source trees are nearly identical (a handful of implementation-only
diffs, no header/signature changes). The most likely locus is something
specific to 2.7.6's *locked dependency versions* (it pins `libc 0.2.153`,
`cfg-if 1.0.0`, `rustc-std-workspace-core 1.0.0`, all older than 2.8.0/2.8.3's
locked versions) interacting badly with the walk once those dependencies are
genuinely present in the crate graph for the first time — but this is
speculation, not a diagnosis; finding the actual mechanism requires
instrumenting the producer walk itself, which is out of this investigation's
authorized scope (read-mostly; no producer code touched). **Flagging this as
the single most important open follow-up**: this session's fix did not just
reveal a small L14-shaped undercount, it reveals that the corpus's apparent
memchr-generation-lineage stability (L10) was itself an artifact of the same
degradation, and at least one generation has a real, currently
unexplained defect that only manifests once dependencies resolve.

**WEAKENED:** nothing disabled or narrowed. This entry documents an
investigation that fixed offline resolvability for 18 fixtures and took a
real measurement, but explicitly did **not** root-cause the 2.7.6 anomaly and
did **not** implement the type-level degradation signal doctrine §2 calls
for — both are named above as open rather than silently left out.

**Status:** MITIGATED for offline resolvability (18/21 fixtures, all but
`bytes`/`serde_json`'s pre-existing L4/L5 producer bugs and any not yet
re-measured). OPEN and now the top-priority item: (a) the producer-side
type-level fix for the degradation signal itself, (b) the memchr-2.7.6
anomaly, (c) re-measuring the rest of the corpus (`log`, `itoa`, `thiserror`,
etc.) now that they resolve, since L1's whole cost table was taken under this
same degradation and may shift the same way memchr did.

---

## L29 — A `TODO` is rendered as user-facing UI text

**Blast radius:** product credibility. This ships.

**Evidence:** `.shots/memchr/17-bottom-dock-open.png`. The Jobs panel body reads,
centred, in the product's own typeface:

```
Jobs
TODO(views): crate::views::jobs_panel
```

**Diagnosis:** the panel is a stub whose placeholder names a Rust module path at
the user. Doctrine §8 holds that a comment is a claim; a placeholder is one too,
and this one claims the product is unfinished in the one place a user is most
likely to look when something seems stuck.

**Fix:** implement the panel or render an honest empty state describing what
*the user* would see here. Add a test asserting no rendered text node contains
`TODO(` or `FIXME` — a one-line guard that closes the class permanently. Check
the `Logs` tab for the same pattern.

**Status:** OPEN. Filed as F9 in `GUI-WORKORDER-2.md`.

---

## L30 — The reader wastes ~60% of its viewport; docs.rs expands what we collapse

**Blast radius:** the central claim that we display more information than
docs.rs. On this evidence we display less of it at once.

**Evidence:** `.shots/memchr/08-symbol-opened.png` — doc prose ends around y=400
of 1800. Below it sits ~600 px of empty background, with `Implementations 6`,
`References`, and `Source` collapsed and pinned to the very bottom, separated
from the content they belong to by a void. `ON THIS PAGE` occupies a full right
rail to display exactly one entry, `Documentation`.

docs.rs, fetched 2026-08-05, **expands every section by default**, so all six
implementations are visible on arrival.

**Diagnosis:** this is not missing data. `Implementations 6` is correct and
complete (verified against source — see `DOCSRS-COMPARISON.md` §3). We hold the
information and choose not to show it.

**Status:** OPEN. Filed as F2 and F3 in `GUI-WORKORDER-2.md`.

---

## L31 — No per-item source jumping

**Blast radius:** an explicit deliverable — the brief asked for "full hyperlink
support and source jumping".

**Evidence:** docs.rs carries a `[Source]` link on every item, targeting an exact
range (`src/memchr/memchr.rs.html#288-291`). We have a single collapsed `Source`
disclosure for the whole symbol, and **what it does is unverified**:
`10-source-tab.png` is byte-identical to `08`, so it never opened in any
captured frame.

**Next step:** establish what it already does by mouse before designing
anything. It may work and merely be unreachable from the keyboard, in which case
this collapses into L16.

**Status:** OPEN. Filed as F6.

---

## L32 — Package provenance is thin

**Blast radius:** the "richness" half of the docs.rs comparison.

**Evidence:** docs.rs shows owner, dependency list, repository and homepage
links, license, release date (`08 July 2026`), and a doc-coverage badge. Our
sidebar shows name, version, and symbol count.

**Caution:** establish which of these the IR can actually supply before
promising any. Adding a rail the engine cannot fill reproduces L30's empty
table of contents in a new place.

**Status:** OPEN. Filed as F8.

---

## L33 — `Language::Cpp` resolves to no runner, and Python registers a producer that cannot produce

**Blast radius:** any caller asking "can you document this package?" — the
answer is currently indistinguishable from "yes, and it has no public API".

**Evidence:**

- `workspace/compiler/languages/python/src/producer.rs:47` — without the
  `pyrefly` feature, `invoke()` discards its input and returns an empty oracle.
  Nothing in the workspace enables that feature. Yet
  `ProducerRegistry::with_all_available()` registers Python.
- `nudox_ir::body::Language` has both `C` and `Cpp`. `ClangProducer` declares
  `const LANGUAGE: Language = Language::C`, so a package tagged `Cpp` finds no
  runner and yields nothing. A comment at `producer.rs:366` already acknowledges
  the trap — a comment is not a fix.

**Diagnosis:** the root defect is that *absence of a runner* and *emptiness of a
result* are the same observable at the call site. Three distinct situations —
unsupported language, supported but toolchain unavailable, documented and
genuinely empty — collapse into one.

**Status:** RESOLVED, 2026-08-05.

- **Python** is now registered only behind a `pyrefly` Cargo feature, forwarded
  from `nudox-store`. In a build without it, a Python lookup falls through to
  `SourceError::ToolchainMissing { language, .. }` — which already names the
  language — instead of returning an empty success. "Available" now means
  available.
- **`Cpp`** is closed by a new `ProducerRegistry::register_c_and_cpp<P>()`,
  which inserts one runner under *both* `Language::C` and `Language::Cpp`.
  Because `Producer::LANGUAGE` is a single const that cannot name two variants,
  the C/C++ unification is now represented once, in the registry, so a future
  caller wiring in `ClangProducer` cannot recreate the trap by registering only
  `C`. `ProducerEntry.runner` moved `Box` → `Arc` to share one runner across two
  keys.

The three situations are now distinct at the call site: unsupported is
`ToolchainMissing`, a registered runner failing at runtime is
`OracleFailed`/`LoweringFailed`, and genuinely-empty is `Ok` with an empty
`PristineIntroTable`.

New tests: `run_names_the_unsupported_language_in_its_error`,
`cpp_lookup_yields_the_same_typed_error_as_any_other_unsupported_language`,
`register_c_and_cpp_resolves_a_runner_for_both_c_and_cpp`,
`register_c_and_cpp_runner_is_reachable_via_cpp_not_just_via_c`.

**Known softness, disclosed rather than hidden:** `register_c_and_cpp`'s
precondition that `P::LANGUAGE == Language::C` is a `debug_assert_eq!`, not a
compile-time bound — Rust cannot easily pin an associated const to a value here.

**Doc/code mismatches from L34 corrected in the same pass:** the phantom
`CSharpProducer` claim in `csharp/src/lib.rs`, the phantom
`#[cfg(feature = "producer-trait")]` guards in `go/src/producer.rs`, and the
nonexistent **Nix** frontend in `workspace/compiler/producer/src/lib.rs`.
`nudox-engine`'s two stale "registers Rust, Python, and TypeScript" comments
were corrected separately.

---

## L34 — Three of seven language frontends have no oracle in the checkout

**Blast radius:** the 20-packages-per-language goal. It is achievable today for
Rust and TypeScript only.

**Evidence** (full audit, 2026-08-05):

| Language | `impl Producer` | Registered | Lowers a real package |
|---|---|---|---|
| Rust | yes | yes | **yes** — 1,347 entries on memchr 2.8.3 (see L38) |
| TypeScript | yes | yes | **yes** — real OXC parse, in-process |
| **Go** | **yes** | pending | **YES, 2026-08-05** — 2,919 entries from `go.uber.org/zap` 1.28.0 |
| **Java** | **yes** | pending | **YES, 2026-08-05** — 2,103 entries from Gson 2.11.0 |
| **C#** | **yes** | pending | **YES, 2026-08-05** — 2,124 from Polly.Core 8.5.2; 1,241 from CommunityToolkit.Mvvm 8.4.0 |
| **C/C++** | **yes** | pending | **YES, 2026-08-05** — L24 resolved; `compile_commands.json` ingested |
| Python | yes | feature-gated | **no** — oracle is a no-op stub (L33) |

**Go, resolved 2026-08-05.** The oracle was recovered from commit `23530d7^`
(~1,070 lines of Go across `main.go`/`docs.go`/`serialize.go`), built with the
nix Go 1.26.4 toolchain with **no source changes required**, and
`impl Producer for GoProducer` restored. Verified individually on real `zap`:
iota enums with exact discriminants (`DebugLevel = -1` … `FatalLevel = 5`),
embedded-interface expansion (`Sink` gains `Close`/`Sync`/`Write` from `io.Closer`
+ `zapcore.WriteSyncer`), Go 1.18+ generics (`Pool[T any]`, disambiguated from a
genuine cross-package name collision by generic arity), embedded struct fields,
struct tags, and value-vs-pointer receivers.

**It also found a defect that made the producer unusable on all real code** —
see doctrine §4. Type lowering called `refer()` unconditionally for named types
while `Lowering::finish` requires every `refer`'d id to be declared, so any
package importing `sync`/`io`/`time`, or merely using bare `error`, failed to
lower. 47 fixture tests passed throughout.

**Java, resolved 2026-08-05.** `Extractor.java` (902 lines) and `Json.java`
restored **byte-identical** from `23530d7^`, plus a new `build.rs` that compiles
the doclet with `javac` at crate build time so `cargo test` needs no manual
step. Verified on real Gson: 8 `toJson` and 11 `fromJson` overloads, real nested
classes, `FieldNamingPolicy`'s 7 enum constants, javadoc split into structured
sections with `doc_links`.

Three real bugs found by real data, none reachable from the hand-authored
fixture:
1. Param ids were built from the *bare* method name, so overloads sharing a
   parameter name collided in `Lowering::finish`.
2. The `$return` pseudo-param hardcoded span `0..0` while sibling params reused
   the method's line, so every overload's return entry collided in seal's
   content-hash disambiguation. **Recovered 52 entries** (2,051 → 2,103).
3. JPMS module directives were dropped entirely — `lower_module` never read
   `m.directives`.

**Multi-version, verified empirically rather than claimed:** `--release` is
unset by default (native level 21) and pinnable via `NUDOX_JAVA_RELEASE`;
`--release 8` genuinely rejects `record`. **JEP 467 Markdown javadoc is
unreachable on JDK 21** — `Elements.getDocCommentKind` does not exist before
JDK 23 and javac does not recognise `///` as a doc comment at all, so the text
is *lost*, not mis-parsed. The test branches on the oracle's reported
`java_version` and will begin asserting the positive case automatically on
JDK 23+ with no code change.

**C#, resolved 2026-08-05 — and unlike Go and Java, this oracle is new code.**
`23530d7^` contained only `oracle/BUCK`; nothing was recovered. Five new C#
files against Roslyn 5.6.0 / net10.0. Two libraries lowered from **source**
(GitHub tarballs, never NuGet `.nupkg`, which ships assemblies): Polly.Core
(2,124 entries, 1,146 documented, 46 async, 180 types) and CommunityToolkit.Mvvm
(1,241 entries, 11 events, 8 indexers, 7 explicit interface impls, 11 variant
type params). The second library exists because Polly declares **zero** events,
indexers, operators, or explicit impls — so it could not prove those paths.

Two defects invisible to fixtures:
1. **Nested types whose container was filtered out.** `lower.rs:343`
   (`enclosing`) is unguarded, and the draft oracle emitted such types under a
   comment asserting `Lowering` treats it as a forward reference — **that claim
   was false**. Polly has 1 and CommunityToolkit 4 public types nested inside
   internal containers; under `--public-only` each would have produced a dangling
   parent and `Lowering::finish` would have rejected the **entire package**.
   Fixed at the abstraction: `IncludeType` recurses the container chain, so the
   document is self-consistent by construction rather than the lowering learning
   to tolerate dangling parents.
2. **1,276 extraction errors → 0.** MSBuild's `ImplicitUsings` file lives in
   `obj/`. The loader now reconstructs the SDK implicit set, reads `<Using>`
   items from the csproj and auto-imported `Directory.Build.*`, honours
   `<Using Remove>`, and drops synthetic usings that fail to bind.

**Contract revised deliberately:** stdout rather than `--out`, because
`nudox_producer::oracle::run_json` is the shared helper Go and Java already use;
and the four `DOTNET_*` env vars documented as required are **not** — verified
empirically with a pristine `HOME`.

---

## L40 — C# namespaces are extracted and then thrown away

**Blast radius:** every C# package's structure. Directly contradicts "all
information is preserved and displayed".

**Evidence:** the oracle emits **19 namespaces** for Polly.Core.
`lower_extraction` never reads `extraction.namespaces`, so every type hangs off
the package root with no namespace hierarchy at all.

**Also dropped** on the same path, each emitted by the oracle and read by
nothing: `diagnostics` (so a degraded extraction is invisible downstream —
the L28/L38 failure shape again), `assembly.version`/`tfm`/`forwardedTypes`/
`ivt`, `mode`, `roslyn`, `type_kind`, and `returnsByRef` for properties.

**Neither side captures:** array rank (`T[,]` and `T[]` both become
`Type::Slice`) and `System.Decimal` (no `Primitive` variant).

**A resolution bug hiding in the same area:** only *types* get
`aliases_from_doc_id`, so a `<see cref="P:…"/>` pointing at a property, field,
or event **cannot resolve** — the docId is emitted but never keyed.

**Pre-existing schema mismatch found while proving this:** `tests/lowering.rs`'s
fixture spells `"typeKind"` where `schema.rs` declares `type_kind`. Only
snake_case deserializes, so that fixture field is silently `""`. Harmless only
because nothing reads it — which is exactly why it survived.

**Status:** OPEN.

---

**New, smaller finding:** the iota-enum heuristic over-includes block-mates.
`detect_iota_enums` checks only `group_has_iota` (a per-*block* signal from Go's
AST) plus a type match, so `_minLevel = DebugLevel` and
`InvalidLevel = _maxLevel + 1` are lowered as enum variants despite not
mentioning `iota` themselves. Locked into a named assertion documenting current
behaviour rather than left to pass silently.

Go, Java, and C# each depend on an external oracle program
(`workspace/compiler/compile/{go,java,csharp}/`) that **does not exist in this
repository**. Their Rust-side lowering layers are substantial and real —
Java's is ~2,600 lines with a dual HTML/Markdown javadoc parser — but every one
of them consumes a JSON contract that nothing currently produces. Their tests
pass against hand-authored JSON fixtures.

**Cheapest real win:** C/C++. The code is complete and correct; the gap is a
missing dependency edge plus the libclang runtime link, which `TESTING.md`
documents as resolvable on this host.

**Doc/code mismatches found and being corrected** (doctrine §8):
`csharp/src/lib.rs` documents a `producer` module and `CSharpProducer` type that
exist nowhere; `go/src/producer.rs` claims `#[cfg(feature = "producer-trait")]`
guards that appear nowhere in the file; `workspace/compiler/producer/src/lib.rs`
names the seven frontends as including **Nix**, which has no crate — the seventh
on disk is `clang`.

**Status:** OPEN.

---

## L35 — The MCP server is never started by the application

**Blast radius:** the "complementary MCP server" deliverable. It exists, it is
tested, and it does not run.

**Evidence:**

- `crates/nudox-mcp` has `[lib]` only — **no `[[bin]]`, no `main.rs`**. It runs
  only if a host constructs `NudoxMcpServer::new(engine)` and calls
  `McpEndpoint::start`.
- Nothing in the repo does that outside the crate's own tests. A grep of
  `workspace/gui/src` for `nudox_mcp` / `McpEndpoint` / `NudoxMcpServer` returns
  **zero hits**, and `nudox-mcp` is absent from `lindsey`'s `Cargo.toml`.
- `workspace/gui/src/workspace/status_bar.rs` carries the UI plumbing —
  `mcp_endpoint: Option<SharedString>` and `pub fn set_mcp_endpoint(…)` — and
  `set_mcp_endpoint` has **zero call sites in the whole repo**.
- `GUI-LOCAL-PLAN.md §L6` states "Lifecycle: started by lindsey after the engine,
  stopped on window close." `nudox-mcp`'s own module docs are written entirely on
  the premise that lindsey hosts it.

**Diagnosis:** a new variant of the doctrine §8 pattern. Previous instances were
docs describing a *type* that did not exist (`CSharpProducer`). This is docs
describing an *integration* that does not exist — harder to catch, because every
individual piece is real. A dead setter is the tell.

**Note the dependency law is not violated.** `lindsey` names exactly one backend
crate, `nudox-engine`, annotated "The ONLY backend crate lindsey may name."
Wiring MCP in must not breach that — the endpoint has to be started somewhere
that is allowed to name both.

**Status: RESOLVED, 2026-08-07.** The application starts it, and the doctrine
tension this entry predicted was real and had to be settled rather than dodged.

The blocker was structural, not a missing call: `McpEndpoint::start`/`stop` are
`async` and `lindsey` links no runtime (LR-9, LD-2). So the seam was built
rather than a runtime bolted into the GUI — `EngineHandle::runtime_handle()`
lets a host *borrow* the engine's one runtime, and `McpHost` (synchronous
`start`/`stop`) holds its own `EngineHandle` clone so the runtime provably
outlives the server by **ownership**, not by documented startup ordering.

**The status type is where the real defect was.** The status bar field was
`Option<SharedString>`, in which one `None` meant four different things: not
started, failed to bind, stopped, and "this process hosts no server". That
conflation *is* the bug — a failed bind rendered identically to a process that
never hosted one. `McpStatus` is now
`Absent | Listening { url, client_config } | Failed { reason } | Stopped`, and a
bind failure paints a warning-coloured "mcp unavailable" segment carrying the
full `#[source]` chain. There is deliberately **no `Starting` variant**:
`McpHost::start` returns only once the listener is bound, so "queried before
startup completed" is unrepresentable rather than merely handled.

**On §1:** hosting MCP requires `lindsey` to depend on a second backend crate,
which §1's old text forbade. Settled by amending §1 rather than by exception —
see AGENTS-DOCTRINE.md §1 and `workspace/gui/tests/dependency_law.rs`. The short
version: `lindsey` always had a transitive edge to `nudox-ir`/`-store` at depth 2
under `nudox-engine`, so the rule could never have meant "no edge"; it meant "no
direct dependency, therefore no import". `nudox-mcp` sits *beside* lindsey as a
second view of the same `EngineHandle` — the surface crossing the seam is
`McpHost`/`McpStatus`, whose vocabulary is `EngineHandle`/`SocketAddr`/`String`.
No IR type crosses it, and that is now machine-checked instead of reviewed.

**Kept honest by** `workspace/gui/tests/mcp_endpoint.rs` (4 tests) and
`crates/nudox-mcp/tests/host_lifecycle.rs` (7). The load-bearing one is
`the_endpoint_the_status_bar_displays_answers_a_real_tools_call`: it starts the
service the way `main.rs` does, builds the real `StatusBar`, then reads the URL
*and the credential back out of the status bar* and dials **that string** over
blocking `std::net` — real `initialize` → `notifications/initialized` →
`tools/call` — asserting on the decoded JSON-RPC body. Reading the address back
out of the widget is what makes it a reachability test rather than a test that
some socket somewhere is open.

Mutation-verified: pointing the displayed URL at a dead port turns 2 of 4 red
with `Connection refused`; making `cancel()` a no-op turns 2 of 7
`host_lifecycle` tests red.

---

## L36 — No end-to-end MCP test: transport and real corpus are never combined

**Blast radius:** every claim that the MCP server works on real data.

**Evidence:** four test files, and the gap falls exactly between them.

| File | Real transport | Real lowered package |
|---|---|---|
| `tests/endpoint.rs` | **yes** — raw TCP, bearer token, 401/200 | no — synthetic `FixtureSource::rich()` |
| `tests/tool_integration.rs` | no — direct `NudoxTools` calls | no — synthetic fixture |
| `tests/real_crate_tokio.rs` | no — bypasses rmcp/HTTP entirely | **yes** — but see below |
| `tests/schemas.rs` | no | no — pure serde/schemars |

`endpoint.rs` sends one raw `initialize` and reads only the **HTTP status line** —
it never issues a `tools/call` or inspects a JSON-RPC result body.

`real_crate_tokio.rs` is entirely `#[ignore]`d and gates on
`.real-crates/tokio/Cargo.toml`, **which does not exist in this checkout**. So it
would print `SKIP:` and return even when run with `--ignored`.

**Doc/code mismatch:** those tests point at `scripts/fetch-real-crate.sh` to
obtain fixtures. **That script does not exist anywhere in the repo.** The real
tooling is `corpus/fetch.nu` + `corpus/manifest.toml` — which has no `tokio`
entry either.

**Cheapest fix, no product code needed:** point `endpoint.rs`'s raw-TCP harness
at an `Engine::start_with_producer` engine over a fixture that *is* present
(memchr — do not fetch tokio just for this), start `McpEndpoint`, issue one real
`tools/call` for `search_symbols`, and assert on the **result body**.

**Status:** OPEN.

---

## L37 — The local engine has no daemon, no file watching, and four no-op stubs

**Blast radius:** "live index daemon". There is no such thing in the local path.

**Evidence:** `crates/nudox-engine/src/lib.rs:433-462` — four methods documented
as stubs in their own doc comments:

```rust
/// **Stub (M3).** Returns an immediately-`Done` stream.
pub fn resolve_project(&self, _root: PathBuf) -> (StreamHandle, Receiver<ProjectEvent>)
/// **Stub (M4).** Returns a receiver that immediately closes.
pub fn sync(&self) -> flume::Receiver<SyncEvent>
/// **Stub (M4).** Returns a receiver that immediately closes.
pub fn jobs(&self) -> flume::Receiver<JobEvent>
/// **Stub (M4).** Currently a no-op.
pub fn command(&self, _cmd: ClientCommand)
```

**This is the root cause of L29.** The Jobs panel renders
`TODO(views): crate::views::jobs_panel` because `jobs()` hands it a receiver that
closes immediately — there is nothing to render. Fixing the panel's text without
fixing the stream would just move the lie.

`Engine::start` builds an in-process object whose lifetime is its host process —
a library object, not a service. No file-watching exists: `notify` appears in
`Cargo.lock` only transitively, and `use notify::` appears nowhere in `crates/`
or `workspace/`. Packages load once at `start*` time and never re-index.

**Two long-running processes do exist, and neither is this.** `workspace/driver`
is the remote federation registry server — it does not depend on `nudox-mcp` or
`nudox-engine` at all. `workspace/index`'s `ingest` binary is a documented no-op
whose `main` prints one line and exits, though the ~2,840-line library beneath it
is real and tested.

**"Embedded registry" names nothing in this codebase** — `grep -rn "embedded
registry"` returns zero hits. The three plausible referents are: `Registry<R>` in
`workspace/ir/model` (real, tested, but an IR entry cache — not packages), the
`registry` crate (real, linked into `driver`), and `DoltEngine` backed by
DoltLite (**source absent** — `workspace/vendor/doltlite/` holds only
`LICENSE.md` and `VENDORING.md`; its `dolt-engine` feature is off by default and
would fail to link). Decide which one the brief meant before building to it.

**Status: PARTIALLY RESOLVED, 2026-08-07 — the lying is fixed, the features are
not built.** Read that distinction literally; it is the whole change.

All four stubs returned an immediately-`Done` stream or a silent receiver, which
is indistinguishable from "asked and the answer was nothing". Each now returns
`Result<_, Unimplemented>` carrying an `EngineCapability` that *derives* its
milestone and its distinct blocker (derived, never stored alongside — the two
cannot drift). All four had **zero callers**, so this cost no ripple, which is
also the strongest evidence they were never load-bearing.

Per-method reasoning, because "not implemented" is not one decision made four
times:

- **`resolve_project`** — the decisive argument is not effort, it is that a
  correct discovery list could not be acted on: `EngineHandle` has no "add a
  package to a running corpus" method, and packages reach the engine only via
  `Engine::start_with_producer`. Implementing the walk would have shipped half a
  feature *and* a second dead API.
- **`sync`** — needs a file-watching subsystem that does not exist. A silent
  receiver claims "everything is up to date" when the truth is "nothing is
  watched", which is the worse of the two failures.
- **`jobs`** — the engine does spawn producer work, but it is observable only as
  `PackageLoadEvent`, which carries no job identity. Projecting one onto the
  other would invent semantics. This is the root cause of L29.
- **`command`** — `ClientCommand::Noop` is rejected *too*, deliberately:
  accepting "the command that does nothing" is exactly what would make the plane
  look alive to the caller most likely to probe it.

**Still open, and deliberately not fixed here:** `open_package` carries the
identical immediately-`Done`-stream defect and was outside the assigned four. It
has no callers, and its doc comment now says so. The daemon and file watching
remain unbuilt; no `notify` dependency was added.

---

## L38 — 88% of memchr's IR was `core`, not memchr. Every entry count is ~8× inflated.

**Blast radius:** the largest on this ledger. Every entry count, the corpus
total, every cost-per-entry figure, search result counts, and the symbol count
the GUI has been displaying to users.

### Defect 1 — a private glob import re-exports the world

`memchr/src/vector.rs:293-294`:

```rust
mod aarch64neon {
    use core::arch::aarch64::*;
```

A **private** glob, inside a **private** module, inside the private `mod
vector`. `item.rs::lower_module`'s re-export scan sweeps it, and **10,014
`Reference` entries** land under `memchr::vector::aarch64neon`: `vdupq_n_u8`,
`svmlalt_n_s32`, `vld3q_lane_f64`, the `ISH`/`SY` barrier constants, the
`_PREFETCH_*` family. All 10,014 were classified — **not one is a memchr name.**

The mechanism is the visibility test at `item.rs:301-305`: it asks whether an
item is `pub` **in its defining crate**, not whether it is reachable from the
package being documented. `core::arch::aarch64::vdupq_n_u8` is `pub` in `core`,
so it passes.

`vector.rs` is byte-identical across all three generations, which is why the
inflation was uniform and therefore invisible.

### Defect 2 — `--all-features` swaps `core` for a stub

`loaded.rs:309` sets `CargoFeatures::All`. That activates memchr's
`rustc-dep-of-std` → `dep:core` → `[dependencies.core] package =
"rustc-std-workspace-core"`, which cargo inserts into the graph **under the name
`core`**, shadowing the real one.

- 2.7.6 pins `rustc-std-workspace-core` **1.0.0** — whose `src/lib.rs` is **0
  bytes**.
- 2.8.0/2.8.3 pin **1.0.1** — `#![no_std] extern crate core as the_core; pub use
  the_core::*;`

That single 3-line difference is the *entire* 2.7.6 divergence. Decisive
experiment: copy 2.7.6 to scratch, replace only the shim body with 1.0.1's,
change nothing else → **902 → 11,358**.

The 442 entries lost in unpatched 2.7.6: 132 derive-generated impls (`Debug` 52,
`Clone` 51, `Copy` 29) that cannot expand because the derive macros arrive via
`core`'s prelude; ~29 hand-written trait impls that silently degrade from
`impl Iterator for OneIter<'a,'h>` to an inherent `impl OneIter<'a,'h>` because
the trait path does not resolve; plus their methods, params, and associated
types. `impl Vector for uint8x16_t` renders as `impl Vector for {unknown}`.

`rustc-dep-of-std` exists solely for building inside rust-lang/rust's std
workspace. docs.rs does not enable it. A documentation tool never should.

### The true counts

| generation | published | true |
|---|---:|---:|
| memchr 2.7.6 | 11,329 → 902 | **1,344** |
| memchr 2.8.0 | 11,329 | **1,347** |
| memchr 2.8.3 | 11,329 | **1,347** |

Inter-generation delta is 3 — the `*_owned` builders L10 found by source diff.
Degraded→resolved is +29 for 2.7.6 and +32 for 2.8.x, all `alloc`-gated. **That
is why 11,329 was identical for three generations**: the only real difference
between them is feature-gated, and degradation hid exactly that.

### A hypothesis this killed

The arch-gating theory — that unresolved features left every SIMD backend live —
is **wrong**. `x86_64`, `avx2`, and `wasm32` appear **zero** times in every
census, degraded and resolved alike. Target-arch `cfg` comes from the
sysroot/rustc, not from `cargo metadata`'s `resolve` section, so it evaluates
identically either way. Only the one aarch64 module explodes, and only because
of the glob.

### Both fixes landed, 2026-08-05. Corpus re-censused.

**Fix 1 asked the wrong question.** The old filter asked the *imported item* for
its visibility — and an item imported from another crate is essentially always
`pub` there, so every private `use` of a public foreign item became a re-export.
The right question is about the **binding**: is this name visible outside the
module that declares it? `Module::scope`'s `visible_from` filters on the scope
entry's visibility, which for an import is the visibility of the `use`. Anchor
at the parent module for nested modules; at a **foreign** module for the crate
root, since "outside this module" there means "outside the crate". That last
part is what stops a bare `use std::…;` in `lib.rs` becoming a re-export.
`document_private` keeps its meaning by *choosing between the two anchors*
rather than disabling the check.

**Fix 2 is structural, not a name blocklist**, as required: a feature is
excluded when activating it would enable a dependency whose resolved package
name starts with `rustc-std-workspace-`, computed transitively over the feature
graph (`dep:x`, `x/feat`, `x?/feat`). It immediately caught names no blocklist
would have:

| fixture | excluded |
|---|---|
| memchr | `core`, `rustc-dep-of-std` |
| hashbrown | **`alloc`**, `core`, `rustc-dep-of-std` |
| libc | `rustc-dep-of-std`, `rustc-std-workspace-core` |
| unicode-width | **`std`**, `core`, `rustc-dep-of-std` |

`hashbrown/alloc` is `alloc = ["dep:alloc"]` where `alloc` *renames*
`rustc-std-workspace-alloc`; `unicode-width/std` likewise renames
`rustc-std-workspace-std`. Neither means what its name suggests. Both cost zero
non-`Reference` entries when excluded, verified — hashbrown's real allocator API
is `allocator-api2`, which is untouched.

**Two further bugs found *by* the re-census** — which is why it was run rather
than trusted:
1. `PackageDependency::name` is the *lib* name: the rename for renamed deps
   (`core`), but the package name **with hyphens turned to underscores**
   (`rustc_std_workspace_core`) otherwise, while the feature table says
   `rustc-std-workspace-core`. Matching one spelling silently missed **libc**.
2. `--features pkg/feature` is resolved against the package's *dependencies*, so
   it fails on single-package manifests: ``package `libc` does not have a
   dependency named `libc` ``.

**Verification that the drops are only phantoms:** for syn, serde, and tracing
the diff is `Reference` entries **only** — zero entries added, zero
non-`Reference` lost. Genuine public re-exports were checked by name and survive
intact (`bytes::Buf`/`Bytes`, `hashbrown::HashMap`, `tracing::Span`/`Level`/
`Subscriber`/`Dispatch`, memchr's 16 root `pub use`s). What went was `Box`,
`IoSlice`, `Allocator`, `Layout`, `NonNull` — private `use` in private modules.

**Status:** RESOLVED. `66,983` and `87,347` are both void; the corpus total is
**55,449**.

**Untested:** the `direct_repo = true` parent-anchor branch of Fix 1 — no
fixture exercises it.

---

## L14 — RESOLVED as a side effect of L28

The canary `nudox-store::feature_gated_api::memchr_alloc_gated_method_reaches_the_lowered_ir`
began **failing** on 2026-08-05 — which is the outcome it was written to detect.
Its own comment says: *"If a future change... starts letting this resolve...
this test's assert should be flipped."*

Feature-gated items now reach the IR, because fixture vendoring (L28) let
`cargo metadata` resolve, which let Cargo features evaluate at all. The +29/+32
in L38's table **is** this limitation lifting.

**Action:** flip the assert and correct its comment, so the next reader is not
told a resolved thing is still broken. A failing canary is not a red test to
silence; it is the test doing its job.

---

## L39 — ROOT CAUSE: external type identity is discarded, and three tracked defects are all symptoms of it

**Blast radius:** every producer, every language, permanently. This entry exists
because three findings that were filed separately, by three agents, on three
languages, turn out to be one problem.

### The three symptoms

| Filed as | Language | Observed |
|---|---|---|
| **L19(a)** | Rust, then Go | `Ref::Foreign` is **constructed nowhere** in `nudox-ir` — only matched. `refer_import` returns a `Ref::Local` into an import arena nothing resolves. **147 of 420 memchr functions (35%) render a bare `?`**; 0 of 420 render `Option<`/`Result<`/`Vec<`. Go hit it independently on `io.Closer` and `sync.Mutex`. |
| **L23** | Rust | 416 functions collapse to 188 distinct identities. |
| **new** | Java | **28 real `Function` entries (~4.3% of 655) vanish during `seal`.** `Gson.fromJson(String,Class)`, `(String,Type)`, `(Reader,Class)`, `(Reader,Type)` all erase to `(Any, Any) -> T` and collide. |

### The single root cause

`lower_type` degrades **any type outside the current extraction** to
`Type::Any`. That is deliberate and documented. The consequence is not: once two
declarations erase to the same skeleton, everything downstream that keys on
identity conflates them.

Then `PristineIntroTable::insert_live` **silently keeps only the last insert** —
its own module doc says *"no tombstones here"*. So the collision does not error,
does not warn, and does not appear in any count. The entries are simply gone.

That is the same failure shape doctrine §8 keeps recording: a real loss
converted into an apparent success, with nothing in the type system forcing
anyone to notice. A `map_err(|_|)`, a `--no-deps` fallback returning `Ok`, a
Nushell `append` that writes nothing — and now a table that answers "how many
symbols?" with a number that has silently absorbed its own collisions.

### Why this is now the top architectural priority

- It is **measured on three independent frontends**, so it is not a
  language-specific quirk and will not be fixed incidentally.
- It **scales with every producer added.** C# and C/C++ will hit it on contact.
- It corrupts the two things the product is *for*: rendered signatures (`?`
  instead of `Option<usize>`) and symbol counts.
- One fix in `workspace/ir/model` — real identity for external types, plus a
  seal-level disambiguator that does not collapse under `Any` — repairs
  signature rendering, overload preservation, and identity collision for every
  language simultaneously.

### Required, not optional

Whatever the fix, **`insert_live` must stop discarding silently.** A collision
must be observable — an error, a diagnostic, or a counter — because the current
behaviour means no measurement taken through this table can be trusted, and we
have already been burned once this session by exactly that (L38).

### Landed 2026-08-05 — but the "every language at once" claim was WRONG

`Ref::Foreign` is now constructed. `insert_live` no longer discards silently —
it delegates to `try_insert_live` and panics naming both the live entry and the
rejected declaration, with `try_insert_live` available where collision is a real
case. Verified on real packages:

```
--- Implementations for memchr::Memchr ---
impl Clone for memchr.memchr.memchr.Memchr
impl Debug for memchr.memchr.memchr.Memchr
impl<'h> DoubleEndedIterator for memchr.memchr.memchr.Memchr
impl<'h> FusedIterator for memchr.memchr.memchr.Memchr
impl<'h> Iterator for memchr.memchr.memchr.Memchr
impl<'h> memchr.memchr.memchr.Memchr
```

Gson's overloads survive seal as **11 distinct declarations** with 11 distinct
IntroIds and zero `any` — including all four named in the defect report.
Notably `0 forced groups`: Java needed no escalation, because naming the foreign
types fixed it at the root and the skeletons simply stopped being identical.

**Correction — I claimed this repairs every language simultaneously. It does
not, and all three diagnoses said so.** Coverage is **4 of 7**:

| producer | emits for a cross-package type |
|---|---|
| Rust, Go, Java, C# | `Ref::Foreign` ✅ |
| TypeScript | `Primitive::Builtin(name)` — **looks resolved to consumers** |
| C/C++ | `Type::TypeVar(name)` — **looks resolved to consumers** |
| Python | `Type::Any` |

The TypeScript and clang cases are **worse than `Type::Any`**, because a
consumer cannot tell them from a genuinely resolved type. Tracked as task #12.

**This explains the unexplained +503 symbol jump.** The run reports
`recovering 510 declarations that previously vanished`. The count did not
inflate — 510 real declarations had been silently lost to collisions and are
now retained. That is the same class of loss L38 recorded, found and fixed.

**A separate user-visible defect found and fixed on the way:** memchr rendered
`impl<''h>` with **two** apostrophes. The Rust producer stores the source
spelling `'h` via `lp.lifetime().text()`, and every renderer prepended a second
sigil. Fixed one level up with `nudox_ir::kinds::lifetime_label`, so the doubled
sigil is unrepresentable regardless of which convention a producer picks.

**Still open:** memchr seals with `0 foreign keys linked, 24 unlinked, 186
forced groups`. Zero linked is suspicious and needs explanation — the labels
render correctly, so this may be a naming/latching distinction rather than a
failure, but it has not been established either way.

**NOT adversarially verified.** Three of four verifier agents died on a session
limit. The implementation is evidenced by its own pasted real-package output
and a green tree, but the independent refutation pass did not run.

### Self-type rendering also fixed, 2026-08-05

The `memchr.memchr.memchr.Memchr` half is closed. Final real output:

```
impl Clone for Memchr
impl Debug for Memchr
impl<'h> DoubleEndedIterator for Memchr
impl<'h> FusedIterator for Memchr
impl<'h> Iterator for Memchr
impl<'h> Memchr
```

That is character-for-character what docs.rs renders.

Fixed in **one** place — `signature.rs::resolve_nominal`, which is the single
renderer every surface goes through (LR-4), so `ImplRow::label`, fields, return
types, quick-peek and MCP all get it at once. `display_path` collapses adjacent
duplicate segments, then returns the bare leaf **if it is unique in the
package**; on collision it elides only the ancestor prefix every namesake shares
and keeps the differing tail — the same rule the search rows use.

**The detail that made it work on real code:** re-export entries had to be
excluded from the namesake set. memchr's own `pub use crate::memchr::{Memchr,…}`
at the crate root counted as a second `Memchr`, so the leaf never looked unique
and never collapsed. Nearly every real crate re-exports its public types at the
root, so without that exclusion the fix would have passed any synthetic test and
done nothing at all in production.

Ambiguity is structurally guaranteed: both colliding entries render through the
same function against the same symmetric namesake set, so they elide the
identical prefix and diverge exactly where their real paths diverge
(`io.Error` vs `fmt.Error`). Pinned by an adversarial test.

**Known sibling defect, disclosed not hidden:**
`crates/nudox-engine/src/search.rs::qualified_display_name` builds its label
from the same raw `path_of` output and has the identical repetition bug when a
search collision forces the qualified form. Different call site, different
problem — left alone under scope discipline. Tracked as task #14.

**Status:** LANDED, verification incomplete. See tasks #12 and #14.

---

## L43 — Enabling `pyrefly` is blocked by a `blake3` version conflict

**Blast radius:** the seventh language. Python remains the only producer that
cannot lower a real package.

**Evidence:** wiring `pyrefly = { git = "…", optional = true }` into
`nudox-producer-python` fails workspace resolution outright:

```
error: failed to select a version for `blake3`.
    ... required by package `pyrefly v1.1.1`
versions that meet the requirements `=1.8.2` are: 1.8.2
all possible versions conflict with previously selected packages
  previously selected package `blake3 v1.8.5`
    ... of package `driver v0.1.0`
```

pyrefly pins `blake3 =1.8.2`; `driver` resolves `^1.8` to 1.8.5. Cargo resolves
optional dependencies at lock time, so **the conflict bites even with the
feature off** — it breaks the whole workspace build, not just Python.

**This blocker was not previously known.** That crate's `Cargo.toml` documents
three others (a network typeshed download at build time, ~30 missing transitive
crates, API-stability risk). This is a fourth, and it is the one that fires
first.

**Options:** pin `blake3` workspace-wide to 1.8.2 and verify `driver` tolerates
it; vendor pyrefly with a relaxed pin; or keep Python unregistered and honest.

**Status:** OPEN. The exploratory change was reverted to keep the tree building.

---

## L41 — Semantic search is a UI affordance with no engine behind it

**Blast radius:** one of the four search modes the product advertises.

**Evidence:** the `Semantic` pill has been drawn since the first screenshot run.
`04b-semantic-mode.png` now shows it **selected** for the first time — and
honestly, because:

- `nudox_engine::SearchQuery` has **no mode field at all**. The GUI bridge's
  `_mode` has nowhere to put it.
- `run_search` hardcodes `SECTION_SEMANTIC` to an empty batch.

So the chips can only scope *presentation*, which is real and visible, and the
honest semantic result is the zero-hit state. **The alternative was fabricating
matches**, which would have been a doctrine §6 weakening of the worst kind: a UI
that looks like it works.

**What it needs:** an embedding store in the engine and a mode on
`SearchQuery`. `workspace/registry`'s vector plane exists and is heavily tested
(`tests/vector/`, 15 files), but is wired to `driver`, not to `nudox-engine`.

**Status:** OPEN. Advertised in the UI, unimplemented in the engine — the gap
should be closed in one direction or the other, and closing it by removing the
pill is a legitimate option.

---

## L42 — Three GUI defects whose fix is in the backend

Recorded here rather than as GUI work, because no amount of `workspace/gui`
effort can resolve them.

1. **The project panel shows a stale generation.** The row is built from
   `PackageLoadEvent::Loaded` plus a `versions()` read at that instant, and the
   engine emits **no further `Loaded`** when later generations finish. Visible
   in every current frame: the rail says `v2.7.6` while the reader pane
   correctly serves `2.8.3`. Needs a new engine event.
2. **`SymbolHead::source_path` is an opaque `<file-id-806>`**, not a path — and
   `source_span` is a **byte** range, not lines. Visible in
   `13-version-picker.png` as `<file-id-806>` / `bytes 8880–9435`. docs.rs links
   `src/memchr/memchr.rs.html#288-291`. Per-item source jumping (L31) cannot be
   built until the wire carries a real path, and the engine deliberately does
   not read files on the doc path.
3. **`wire::ImplRow` carries no source location at all** —
   `{key, label, is_blanket, trait_label, self_generic_count}`. A per-impl source
   link has nothing to point at.

**Status:** OPEN, all three.

---

## L44 — Declaration identity is a lossy string projection, in 5 of 7 producers

**Blast radius:** every producer. Found by the first full corpus sweep
(`CORPUS-SWEEP.md`), which lowered 109 of 153 real packages across 7 ecosystems.

**Evidence — the same defect, independently, in five languages:**

| producer | how identity was lost |
|---|---|
| **Go** | `_` is a legal *and repeatable* parameter name; `format!("param:{}")` fell back to a positional index only when `is_empty()` → viper, client_golang, go-redis/v9, go-cmp |
| **Java** | `type_erase` used a type variable's bare letter rather than the erasure of its bound (JLS 4.6) → commons-lang3's four unrelated `<T> Validate.notEmpty` overloads collapsed to **one** `JavaId` |
| **TypeScript** | `TsId` omitted the enclosing `namespace` chain → zod ×2, @types/node; star re-export fan-out had no dedup across convergent barrels → date-fns |
| **C#** | the Roslyn docId is unique per *compilation*, but the MSBuild-free oracle feeds `JsonReaderHelper.netstandard.cs` **and** `.net8.cs` into one → System.Text.Json |
| **Rust** | `impl_display_name` leaked `<T as IntoParallelIterator>::Item` into `Symbol.name`, violating the name index's identifier invariant → rayon |

**Diagnosis:** `Producer::Id` carries **no obligation that the projection be
injective**, and nothing type-checks it. The instructive part: Go and TypeScript
both used *structured* id types (`GoId` enum, `TsId` struct) and collided
anyway, because a `String` field **inside** the structure dropped the
distinction. Structure at the top level buys nothing if identity is stringly
encoded underneath.

**The one thing standing between a lossy id and a silently wrong table** is
`Lowering::finish`'s `LoweringError::Duplicate` (`workspace/ir/model/src/lower.rs:404`).
It caught all five. It is the reason this sweep produced findings instead of
corrupt data, and it deserves to be named as the load-bearing check it is.

**Relationship to L39/L23:** L39 was *foreign* type identity discarded; this is
*local* declaration identity discarded. Same shape, opposite side of the
package boundary.

**Status:** 7 producer defects found and fixed this sweep across 5 producers —
every one on real packages that a combined 47 (Go) + 27 (TypeScript) + 20 (C#)
hand-authored fixture tests had all passed green. Doctrine §4 again.

---

## L45 — Java lowers 5 of 22 packages: `javadoc` is invoked with no classpath

**Blast radius:** the worst per-ecosystem result in the sweep.

**Evidence:** `JavaProducer::invoke` (`producer.rs:99-119`) builds its `javadoc`
argument list with **no `-classpath` and no `--module-path` at all**. Any library
whose sources reference a type it does not itself declare fails to resolve.
17 of 22 Maven packages failed this way.

**The failures surface as a generic `OracleExit`** — but
`ProducerError::DependenciesUnresolved` already exists as a *shared* variant
(`workspace/compiler/producer/src/lib.rs:206`) whose doc describes Java's exact
situation, and **only Rust constructs it**. So the correct typed error was
designed, shipped, and routed around.

Worse, in the same family: `ProducerError::UnsupportedConstruct` is constructed
**exactly once** workspace-wide, as the catch-all `other =>` arm at
`rust/src/lib.rs:168`, relabelling every non-dependency Rust error including
genuine lowering bugs. That is doctrine §8's `map_err(|_|)` shape wearing a
different hat.

**Status:** OPEN. Highest-value single fix in the sweep — resolving the Maven
dependency classpath should move Java from 5/22 toward parity with Go's 22/22.

---

## L31 — RE-DIAGNOSED: source jumping is a producer gap, not GUI wiring

This entry previously read *"it may work and merely be unreachable from the
keyboard."* **That is wrong.** Span-producing call sites, counted across the
sweep:

| producer | real spans emitted |
|---|---:|
| TypeScript | 17 |
| Rust | 11 — **free functions only** |
| Go, Java, C#, C/C++, Python | **0** |

The GUI has nothing to jump to for roughly 90% of Rust item kinds, and for
*every* item in five languages. No amount of `workspace/gui` work can fix it.
Source jumping requires producers to emit spans first.

---

## L47 — The TypeScript producer reads nothing out of CommonJS packages without bundled `.d.ts`

**Blast radius:** 4 of 22 npm corpus entries, and by extension a large fraction
of the real npm registry — every package that publishes its types as a separate
`@types/*` package rather than shipping them.

**Evidence,** measured 2026-08-07 by
`languages/typescript/tests/real_npm_packages.rs` against the `.real-crates/`
checkouts:

| package | declarations contributed | first name |
|---|---:|---|
| `lodash` 4.17.21 / 4.17.20 | **1** (~300 public functions) | `"lodash"` |
| `debug` 4.3.4 | **1** | `"index"` |
| `ws` 8.16.0 | **2** | — |

All three ship **no `.d.ts` and declare no `types`/`typings`** — verified by
inspection of the checkouts. Their API escapes only through `module.exports` at
runtime, so there is no top-level declaration for the extractor to read. The
name that does come back is the entry *file*, not an exported symbol.

**Why it stayed invisible:** the sweep's only guard was `entry_count == 0`, read
off `produced.table.len()`. That branch is unreachable — `produce` synthesizes
the root `Symbol` before `lower` is ever called, so every successful run seals a
table of at least one entry. A producer that read no bytes at all scored 1 and
passed. Doctrine §4's "a test that would pass against a stub is not a test", in
its purest form: the number was real, the denominator was not.

**Status:** OPEN, and now *pinned rather than hidden*. The four entries are
`Expect::Stub { contributed, why }` in that sweep, asserted as **exact
equality** — so the day the extractor learns to read CommonJS, the test fails
and the claim has to be retracted in the same change that invalidates it. This
is `ProducerError::YieldContractOutgrown`'s reasoning applied one level up, at
the corpus. Contrast `p-limit` 5.0.0, which ships one `.d.ts` and is a genuine
`Expect::Declarations(3)`: the obstruction is the missing type declarations, not
CommonJS as such.

---

## L48 — Cargo's test autodiscovery cannot see `tests/<dir>/<name>.rs`, and silently says nothing

**Blast radius:** 18 test files across two crates, invisible for their entire
existence. Not "failing" — never *compiled*, not once.

Cargo's integration-test autodiscovery globs exactly two shapes: `tests/*.rs`
and `tests/*/main.rs`. A file at `tests/<dir>/<name>.rs` matches neither. It
gets no target, so it is never built, never run, and — the part that makes this
a trap rather than an inconvenience — **produces no error**. The crate reports a
clean suite. `registry` had exactly one target (`lib`) and looked healthy.

Found in two places independently, which is what makes it structural rather
than an accident:

| crate | files | what they covered |
|---|---:|---|
| `registry` | 16 + 2 support modules | the whole vector plane: sharding, packing, hotset admission, scheduler gating, adversarial fanout |
| `index` | 2 | `object_pack` adversarial + transport |

**A second, independent reason `registry`'s 16 could not have compiled even
with a target:** every one of them imported a crate named `vector`, which no
longer exists — it was folded into `registry::vector`. 89 path references had
gone stale with nothing to notice, because nothing ever type-checked them. Two
unrelated causes of the same silence, stacked.

**What it cost.** First-ever execution of `registry`'s 16: **102 tests, 97 pass,
5 fail**, plus 2 pre-existing failures in `--lib`. All 7 are now fixed; the
interesting part is that they were **not** 7 product bugs. The split matters,
because "the test was wrong" is the answer that needs the most evidence:

| # | verdict | what it actually was |
|---|---|---|
| 2 | **product** | `merge_hits` sorted on a bare `b.score.total_cmp(&a.score)`. Under IEEE-754 totalOrder a positive-signed NaN is the *maximum* — above `+Infinity` — so a NaN score won top rank. Fixed with a `compare_hits` total order that sinks NaN to worst before the existing ascending-`id` tiebreak. |
| 1 | **product** | `PayloadValue` was declared `Str, Int, Bool`, so `#[derive(Ord)]` gave `Str < Int < Str`-order — contradicting the documented `Bool < Int < Str`. Reordered. Safe because `registry` uses only self-describing codecs, which tag enums by variant *name*: no positional wire format depends on the discriminant. |
| 4 | **test/fixture** | one unsatisfiable assertion, two wrong fixtures, one wrong expectation. Each derived, not asserted — see below. |

The unsatisfiable one is worth keeping. `merge_hits_non_finite_scores_do_not_panic`
demanded both "NaN ranks worst" *and* a generic loop asserting every adjacent
pair satisfies raw `total_cmp() != Less`. Since NaN is `total_cmp`'s unique
maximum, nothing but another NaN can precede it without violating that loop —
the two clauses cannot both hold. Replaced with an exact pinned order
(`[Infinity, 0.5, -Infinity, NaN]`), which is strictly stronger than the loop it
replaced.

**The security-relevant one, in full**, because it is the only finding here that
was a *missing* defence rather than a broken one: `absolute_path_artifact_rejected`
and `parent_dir_traversal_artifact_rejected` were failing inside their own
fixture. The `tar` crate now rejects `..` and absolute paths at *write* time, so
the fixture could not build the hostile archive, and the traversal guard had
therefore **never been exercised against a hostile input**. Fixed by writing the
raw path bytes straight into the GNU header's `name` field and calling the
validation-free `Builder::append` with a hand-fixed checksum — and then
re-parsing the built archive inside the fixture to assert the raw header bytes
equal the hostile string verbatim, so a future regression to a benign fixture
fails loudly instead of passing for the wrong reason.

**Where it landed:** 279 tests pass, 0 fail, under
`cargo test -p registry --features local,embed --tests`.

**Feature-gated coverage, stated so it is not mistaken for completeness.** Under
`--features local` alone, six of the sixteen targets report **zero** tests: five
are `#![cfg(feature = "embed")]` and one is `#![cfg(feature = "onnx")]`. `embed`
costs only `dep:sha2`, so it is nearly free and adds 29 tests — run it. `onnx`
pulls `fastembed` + `tokenizers` and `onnx_live` remains unrun here.

**The fix, and why it is the right shape.** Explicit `[[test]]` entries naming
each file, rather than flattening the directory. Flattening would break the two
shared support modules (`mod common;` / `mod support;` resolve against the
directory), and — more importantly — an explicit target means deleting or
renaming a file fails the *manifest*, loudly, instead of silently dropping its
coverage again. Restating the problem in a form that can recur is not a fix.

**Kept honest by:** the manifest itself. There is no test for this, and there
cannot usefully be one — the failure mode is a target that does not exist, so
there is nothing to run. The audit is:

```text
find workspace crates -path '*/tests/*/*.rs' -not -path '*/vendor/*' \
  -not -name 'main.rs' -not -name 'mod.rs'
```

Every result must appear as a `[[test]] path = …` in its crate's manifest.
Re-run it when adding a test directory.

**Adjacent, unrelated, and now contradicted:** `AGENTS-DOCTRINE.md` §7 states
the `driver` duplicate-zstd-symbol claim was false and that "nothing in the real
diagnostic mentions zstd". As of 2026-08-07 `driver` emits
`ld: duplicate symbol '_ZSTD_*'` between `libzstd_seekable` and `libzstd_sys`
— as a `warning: linker_messages`, so the link still succeeds. The doctrine's
text was accurate when written and is not now.

---

## L49 — REPORTED, NOT REPRODUCED: qdrant-edge segment flush panics on `Drop` under load

**Status: unconfirmed.** Recorded at the confidence the evidence supports, not
promoted to a finding. Treat it as a lead.

**What was seen.** During the L48 work,
`vector_depshard_install::install_registers_searchable_shard_and_evict_removes_it`
failed **2 times in ~11 runs** with a panic inside vendored qdrant-edge —
`workspace/vendor/qdrant-edge/src/edge/mod.rs:181`, segment flush on `Drop`,
`IO Error: No such file or directory`. Every one of those runs was under heavy
concurrent-cargo contention (three agents building against the shared `target/`).

**What was not seen.** Re-run 8 times on a quieter machine: **8 passes, 0
failures.** The reproduction attempt is recorded because a failed reproduction is
evidence too, and because promoting an unreproduced observation to a "known
flake" is how a real bug gets an excuse attached to it.

**Why it is still worth writing down.** The failure is on a `Drop` path in
vendored C++-backed code, which is the one place where "it only happens under
load" is a plausible *description of a real race* rather than a synonym for
"noise" — a flush racing teardown of the directory it writes into would look
exactly like this and would be invisible at low concurrency. It is also
adjacent to L7, the other vendored-qdrant-edge defect.

**To settle it:** run that target in a loop under deliberate load (a concurrent
`cargo build -p driver` is the cheapest way to reproduce the original
conditions) and capture the full panic with `RUST_BACKTRACE=1`. If it
reproduces, it is a teardown-ordering bug in vendored code and belongs in the
`vendor/` tracking, not here.

---

## L50 — RESOLVED: a failed build-script step silently deleted every `#[cfg]`-gated public API

**Status: FIXED 2026-08-07.** Both confirmed instances now load correctly, and
the failure mode is a typed refusal rather than a `warn!`. What follows keeps
the original diagnosis because it is the evidence, and marks the two places
where the original text was **wrong** — both in ways that mattered.

**Blast radius (historical):** any crates.io package whose `build.rs` emits
`cargo:rustc-cfg` and whose published tarball omits a file its own `Cargo.toml`
declares as a target. Two confirmed in a 23-entry corpus; the class is much
larger, and the lowering was **wrong without being marked wrong**, which is the
property that made it worse than a failure.

### The chain, every link reproduced 2026-08-07 (re-reproduced before the fix)

1. `.real-crates/log-0.4.17` is a `cargo package` tarball extraction. Its
   `Cargo.toml` declares `[[test]] name = "filters"` and `[[test]] name =
   "macros"` — and `tests/` was **excluded from the published tarball**.
2. `ra::loaded::load` calls `ws.run_build_scripts(...)`, which shells out to
   `cargo check`. Reproduced directly in that checkout:

   ```text
   $ cargo check --all-targets
   error: can't find integration-test `filters` at path `…/log-0.4.17/tests/filters.rs`
   error: can't find integration-test `macros`  at path `…/log-0.4.17/tests/macros.rs`
   error: could not compile due to 2 previous target resolution errors
   ```

   Cargo fails at **target resolution**, before compiling anything.

   **CORRECTION 0 — the timing was NOT the tell, and citing it was a mistake.**
   The original text argued from `ra_load phase=build_scripts elapsed_ms=74.8`,
   calling it "three orders of magnitude too fast to be a real check". That
   reasoning does not hold: measured on the **fixed** build, the same phase
   takes **70.8 ms** and the cfgs arrive correctly. A warm cargo cache and a
   trivial `build.rs` make a successful run just as fast as a failed one, so
   this number never distinguished the two states. It is a plausible-looking
   signal that happens to be worthless, which is worse than no signal — anyone
   re-deriving this bug from the timing would have concluded the fix had not
   worked.

   The invariant is the **symbols**, never the duration. On the fixed build,
   from `module_census` over the real checkout:

   ```text
   census total=1251
   set_logger        3 occurrences   (was 0)
   set_boxed_logger  3 occurrences   (was 0)
   AtomicUsize       0 occurrences   (was present — the counterfeit shim)
   set_logger_racy   3 occurrences   (ungated control, unchanged)
   ```

   **CORRECTION 1 — where `--all-targets` came from.** The original text implied
   this crate asked for it. It did not: `build_cargo_config` has always set
   `CargoConfig::all_targets = false`. `ra_ap_project_model` 0.0.341 adds the
   flag **unconditionally**, ignoring that field, whenever the toolchain is new
   enough for `--compile-time-deps` (`build_dependencies.rs:530-535`):

   ```rust
   if cargo_comp_time_deps_available {
       cmd.arg("--compile-time-deps");
       // we can pass this unconditionally, because we won't actually build the
       // binaries, and as such, this will succeed even on targets without libtest
       cmd.arg("--all-targets");
   }
   ```

   That comment is true of *compilation* and false of *target resolution*,
   which happens first and is fatal. Chasing our own config field would have
   found nothing.
3. The failure was swallowed by a `warn!`, and **nothing in this crate or its
   tests installs a `tracing` subscriber**, so the warning went nowhere.

   **CORRECTION 2 — which `warn!`.** The original text named
   `warn!(error = %e, "build scripts failed; continuing without OUT_DIR")` at
   `loaded.rs:406-408` — the `Err` arm. That arm never fired. Upstream reports a
   `cargo check` that exited non-zero as **`Ok(scripts)`**, parking the
   diagnostics in `WorkspaceBuildScripts::error() -> Option<&str>`
   (`build_dependencies.rs:423-429`). The arm that actually fired was
   `warn!(error = %err, "build scripts reported errors; OUT_DIR items may be
   missing")` — and `ws.set_build_scripts(scripts)` ran immediately after it, so
   the load carried on with an empty build-script table. The distinction is not
   pedantry: it is the same `Option`-field-nobody-must-read shape as
   `ProjectWorkspaceKind::Cargo::error`, which is what makes this the *same*
   defect as L39 rather than merely a similar one.
4. `cargo:rustc-cfg=atomic_cas` and `has_atomics` never reached the crate graph.
5. Every item behind those cfgs was absent from the lowering — and every item
   behind their `#[cfg(not(...))]` counterparts was lowered in their place.

### Instance A — `log 0.4.17` lost two public functions and was analysed as a different target

| symbol | gate (verified in `src/lib.rs`) | before | after |
|---|---|---:|---:|
| `set_logger` (`:1465`) | `#[cfg(atomic_cas)]` | **0** | present |
| `set_boxed_logger` (`:1407`) | `#[cfg(all(feature = "std", atomic_cas))]` | **0** | present |
| `set_logger_racy`, `logger` | ungated | 1, 2 | unchanged |

It was worse than two missing functions. The table contained
`Record log::log::AtomicUsize` with `impl Sync` and `impl AtomicUsize::{new,
load, store}` — log's private `#[cfg(not(has_atomics))]` **fallback shim**
(`src/lib.rs:350-392`), lowered as though it were part of log's public
structure. On aarch64-apple-darwin both cfgs are true. **The lowering described
a crate that does not exist on this target.**

`log 0.4.33` passed all seven expectations throughout, because it deleted its
build script and uses the built-in `#[cfg(target_has_atomic = "ptr")]`. That is
the A/B: two adjacent versions of one crate, same public API, one correct and
one silently gutted — **invisible to any single-version fixture**, and the
entire reason the corpus carries two `log` versions.

### Instance B — `nom 5.1.3` lost eight public parsers

`build.rs` emits `cargo:rustc-cfg=stable_i128` on any compiler ≥ 1.28. Same
tarball defect (`can't find bench 'arithmetic' at …/benches/arithmetic.rs`, ×5),
`build_scripts elapsed_ms=114.8`. `be_u128`, `be_i128`, `le_u128`, `le_i128` →
**0 hits each**, in both `complete` and `streaming` (8 functions). All eight are
back.

**`libc 0.2.161` was the control:** same code path, but its declared test
targets *are* present in the tarball, so its `cargo check --all-targets`
succeeded — 18,864 entries, every expectation satisfied, before and after. The
trigger was the tarball's missing files, not build scripts as such.

### The fix, in two halves

Both were needed. The first makes the two packages load *correctly*; the second
guarantees that any future package this does not save fails **loudly** instead
of quietly.

**Half 1 — narrow the request (`ra::loaded::narrowed_build_script_config`).**
`run_build_script_command` is rust-analyzer's own supported override for the
build-script `cargo check` (`rust-analyzer.cargo.buildScripts.overrideCommand`).
`load` now sets it to the command upstream would have assembled, minus
`--all-targets`. Nothing this engine does needs those targets: a build script's
output is a property of the *package*, not of which of its targets cargo was
asked to check, and the only thing `--all-targets` adds is dev-dependency build
scripts, whose `OUT_DIR` and cfgs are irrelevant to documenting a public API.
`--compile-time-deps` is kept and is load-bearing beyond speed — it stops cargo
compiling `value-bag 1.0.0-alpha.9`, a `log 0.4.17` dependency that no longer
builds on a modern rustc and would otherwise fail the check for an unrelated
reason.

Measured directly, the identical command minus the flag exits 0 and delivers
`atomic_cas`+`has_atomics` (log 0.4.17), `stable_i128` (nom 5.1.3), and libc's
fifteen.

**Half 2 — make the degraded state a value (`ra::loaded`).** This is the same
shape `DependencyResolution` already had, one layer down:

- `BuildScriptExecution::{NotDeclared, Ran, Failed(BuildScriptFailure)}` — the
  axis, on the loaded workspace, private and reachable only through a
  projection, so no caller can pattern-match past it.
- `BuildScriptFailure::{NotRun, CargoRefusedTheWorkspace,
  DisabledByConfiguration}` — carries what cargo actually said, in a `#[source]`
  slot, reachable by walking `std::error::Error::source` (doctrine §8).
- `LoadCompleteness { dependencies, build_scripts }` — **one** type describing
  how complete a load is, with **one** choke point, `require_complete`, which is
  now the only function in the crate that turns "degraded but unaccepted" into a
  typed error on either axis. `require_resolved_dependencies` is gone; its check
  is the first arm of that function.
- `LoadedWorkspace::accept_missing_build_script_cfgs` — a consuming `#[must_use]`
  opt-in, deliberately *separate* from `accept_degraded_dependencies` because
  the two license different lies about the output.
- `RustProducerError::BuildScriptsFailed` → `ProducerError::BuildScriptsFailed`,
  which — `ProducerError` being exhaustive by design (§3) — broke
  `nudox-store`'s mapping until it decided what the new failure means.

The two axes stay separately representable rather than being folded into one
enum because they are not mutually exclusive: a `--no-deps` load makes upstream's
`run_build_scripts` a silent no-op, so a package can be degraded on both at once
and a sum type would have forced the load to report whichever was checked first.
The argument is written out on `LoadCompleteness` itself.

### What the entry counts did — and why the old prediction here was wrong

The note that used to sit in `corpus/entry-baseline.toml` said both numbers
"MUST GO UP" when L50 was fixed. One did. The other went **down**:

| package | before | after | direction |
|---|---:|---:|---|
| `log 0.4.17` | 1255 | **1251** | **down 4** |
| `nom 5.1.3` | 3260 | **3288** | up 28 |

`nom`'s `stable_i128` gate has no `#[cfg(not(...))]` counterpart, so restoring
it only adds. `log`'s does: fixing it restores `set_logger` and
`set_boxed_logger` and **deletes the whole `#[cfg(not(has_atomics))]` shim** —
struct, four methods, and an `unsafe impl Sync` — which is more items than it
gains. The broken table was never a subset of the correct one; it contained
things the correct one does not.

That is the strongest single argument in this file for doctrine §4's rule
against count-based assertions. No entry count, moving in any direction, could
have told anyone which of these two tables was the right one. The 21 unaffected
packages' counts are byte-identical before and after, which is the other half of
the evidence.

### Kept honest by

- `workspace/compiler/languages/rust/tests/corpus_sweep.rs` — **23 lowered, 0
  unresolved, 0 failed, of 23 entries** (2026-08-07, 783.84 s). It asserts on
  `(name, KindDiscriminant)` pairs read out of each checkout's own source —
  never on counts, and never on `is_ok()`.
- `workspace/compiler/languages/rust/tests/build_script_cfgs.rs` — four
  hermetic cases (62.10 s for the whole file, against the sweep's 783.84 s)
  that isolate the mechanism without a real crate.
  `a_phantom_test_target_does_not_stop_the_build_script` synthesizes the exact
  published-tarball shape (a `[[test]]` entry pointing at a file that is not
  there) and is the regression test proper.
  `accepting_a_failed_build_script_yields_a_table_describing_a_different_crate`
  asserts the counterfeit item *is* present once the caller opts in, which is
  what makes the refusal legible rather than merely cautious.

Verified by mutation, not inspection. Restoring `--all-targets` turns
`a_phantom_test_target_does_not_stop_the_build_script` red with
`Failed(CargoRefusedTheWorkspace { … "can't find integration-test `phantom`" })`
— note that it *refuses* rather than mis-lowering, which is half 2 catching what
half 1 missed. Restoring the old `warn!` in place of the typestate turns
`a_failed_build_script_is_refused_and_carries_the_cargo_diagnostic` red with
"must be recorded as a failure, not as `Ran`; got Ran". Deleting the
`nudox-store` match arm fails to compile with
`E0004: non-exhaustive patterns: ProducerError::BuildScriptsFailed not covered`.

### Still true, and not fixed by this

`--all-targets` remains hard-coded upstream; this repo routes around it for its
own build-script step only. Any other `ra_ap_project_model` consumer on a
tarball with phantom targets has the same defect, and an `ra_ap_*` bump could
change the command shape under us. That is bounded rather than dangerous: any
override that stops delivering cfgs makes cargo exit non-zero, and
`BuildScriptExecution::Failed` then refuses the whole load. There is no
arrangement of those flags that produces a quietly-wrong table — which is
exactly the property the old code lacked.

---

## Things we have NOT tested at all

Recorded so nobody mistakes silence for success:

- Any language other than Rust, end to end (L2).
- Any package fetched from a remote source (L8).
- Concurrent multi-package loading under memory pressure.
- The MCP server against a real (non-fixture) corpus.
- Anything on Linux. All evidence in this repo is macOS/aarch64.
- Mutation testing. `cargo-mutants` is wired into the nightly tier but has not
  been run this session.

---

## L46 — We repair one class of malformed intra-doc link, deliberately

**This entry is not a defect report.** It is the record of a behaviour we chose,
bounded, and made visible — filed here so that "what does nudox do to a doc
comment that rustdoc would not?" has a written answer rather than a folk one.

**Status:** OWNED. Decided and implemented 2026-08-06.

### What we do

Exactly one malformed spelling is turned into a working link:

| `LinkRepairKind` | Source | rustdoc / docs.rs | nudox |
|---|---|---|---|
| `TransposedOpenDelimiter` | `` `[foo`] `` — backtick and bracket transposed | literal text | resolved link, **marked** |

Nothing else. `[foo]` and `` [`foo`] `` are rustdoc's own documented intra-doc
syntax; rendering those as links is fidelity, not repair, and they are recorded
as `LinkOrigin::Authored`.

Real instances: `.real-crates/memchr-2.8.3/src/memchr.rs` lines 282
(`` `[memrchr_iter`] `` on `Memchr`), 358 and 426 (`` `[memrchr2_iter`] `` on
`Memchr2` / `Memchr3`).

### Why this is filed as a limitation at all

Because the first version of it was a **silent** repair, and a silent repair is
the same defect class as a silent failure: the degraded case was representable
as the good one. A repaired link and an authored link produced a byte-identical
`InlineRun::Link`. Nothing in the wire, the IR, or the GUI could answer "what
else do we silently repair?" — see `DOCSRS-COMPARISON.md` §2, which refused to
credit the behaviour in either direction until it was owned.

### The four properties that make it ownable

**Typed.** `InlineRun::Link` carries a mandatory `origin: LinkOrigin` with no
`Default`. A link cannot enter the protocol without answering whose spelling it
was. `shortcut_link` in `chunk/walk/prose.rs` is the sole constructor on the
shortcut path; it takes a `DelimiterShape` by value and derives the origin from
`DelimiterShape::repair_kind()`, so a caller never gets to *name* an origin.

**Bounded.** `LinkRepairKind` is deliberately **not** `#[non_exhaustive]` — it
takes the same exception `ProducerError` does (doctrine §3), and more sharply:
the entire value of the enum is that adding a variant breaks every match in the
workspace.

**Visible.** A repaired link paints in the link accent (it *is* a real link)
with a wavy `warn` underline, and hovering shows the engine's sentence verbatim
— lindsey does no string work (LR-3). The mark never touches the text, so
selection, copy and every text assertion are unaffected.

**Counted.** `RepairTally` derives the count *from the rendered runs*. There is
no parallel counter, so a count and the thing it counts cannot drift.

### The audit

```bash
RUSTC_BOOTSTRAP=1 cargo test -p nudox-engine --test link_repair_audit \
  -- --ignored --nocapture
```

emits one TSV row per (package, kind), zeroes included:

```text
link-repair	memchr	2.8.3	transposed_open_delimiter	3
```

**Pinned number: 3**, measured 2026-08-06 (1835 entries lowered, 1835 chunked).
It agrees with the three source sites above; the multiplier is one because none
of them lives under memchr's cfg-gated `arch/` tree. Doctrine §8 applies in both
directions if it ever moves — a suspicious agreement needs explaining as much as
a disagreement.

### What a sixth repair costs

Five compile errors in five files, plus one red test, before it can render once:

| # | Where | Why it breaks |
|---|---|---|
| 1 | `chunk/walk/shortcut.rs` — `open_shape` | the only entry to the bracket path |
| 2 | `chunk/walk/shortcut.rs` — `DelimiterShape::repair_kind` | no wildcard arm → must give a repair/not-a-repair verdict |
| 3 | `wire/repair.rs` — `LinkRepairKind::label` / `::token` | no wildcard arm → must state a reader-facing name and an audit token |
| 4 | `wire/repair.rs` — `all_lists_every_link_repair_kind` | red until `ALL` is extended *and* the pinned length bumped |
| 5 | `workspace/gui/.../docs.rs` — `repair_legend_label` | the separate GUI package refuses to build until the reader affordance exists |

Plus the audit test, which asserts every non-baseline kind counts zero — so a
new repair that fires on real crates is red on its first run.

### Kept honest by

`transposed_backtick_shortcut_is_recorded_as_a_repair`
(`crates/nudox-engine/tests/hyperlink_flows.rs`) — end-to-end through the public
`chunk` API, asserting the link reaches the GUI seam carrying
`LinkOrigin::Repaired(TransposedOpenDelimiter)` with the author's bytes in
`raw`, and failing with "SILENT REPAIR" if the origin comes back `Authored`.

Verified by mutation, not by inspection: making `DelimiterShape::repair_kind`
return `None` for the transposed shape turns **five** tests red across three
files, including the runtime backstop
(`authored_link_text_appears_verbatim_in_the_doc_comment`) which catches it
without knowing how it happened.

`workspace/gui/tests/repair_paint.rs` closes the last gap: it rasterises two
paragraphs with *identical text*, differing only in `LinkOrigin`, and asserts the
frames differ. 1560 pixels do. That is what stops the mark from being a Rust
enum nobody can see.

