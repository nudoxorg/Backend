# Resolution × IR Unification — The IR Is the Universe

**Date:** 2026-07-19 · **Status:** Rev 4 — normative, implementation-grade target (supersedes Rev 1–3 of this document; supersedes [03-exhaustive-plan](./03-exhaustive-plan.md) §2/§6). Rev 4 fully qualifies the implementation plane: cross-language places, a universal capability lattice, the dual-tier merge algorithm, the region taxonomy, the summary ruleset, and the taint ontology — each grounded in named prior art, none deferred.
**Reads with:** `.research/ir-vcs/design/SEMANTIC-IR-VCS-PLAN.md` (Rev 2.1 — "SIV"), `INDEX-PLAN.md` (rev 2 — "IP").
SIV amendments are proposed explicitly in §20. Ladybug / GD-34 / client-side graph engines are **cut** (KuzuDB, on which GD-34 rested, was archived 2025-10; "Ladybug" is an orphaned fork).

**Design law for this document:** every construct is specified to the token or rule, beginner-implementable, with the cross-language mapping written out. No "Rust-shaped and generalize later," no "partial tier now." Where a fact cannot be known (a syntactic-only parse), that is represented **honestly** as a first-class `Absent`/confidence-tagged value — degradation is a quality signal we always emit, while always aiming for the highest fidelity.

---

## 0. Thesis

The IR is **two planes of one type system**, both stored as F1 frames in the pijul channel, both speaking one `StableRef` vocabulary, both re-keyed by the one substitution cascade σ, both versioned as semantic history; and **one query plane** over them.

- **Declaration plane** (`{intro}.nir`) — *what a symbol is*: kind, signature, exports, docs, attrs. (Part I.)
- **Implementation plane** (`{intro}.nb`) — *what a symbol does*: a structured (region-tree) body with cross-language places, a universal capability annotation on every value-flow, per-local types, control-flow structure, and per-function dataflow summaries. As rich as a compiler mid-level IR, as diff-stable as F1, **dual-fidelity** (tree-sitter skeleton + oracle enrichment merged by a deterministic algorithm), every fact **confidence-tagged**. (Part II.)
- **Query plane** — one Trustfall adapter; semver, security, navigation, examples are the same thing at different planes. (Part III.)

The storage law, from every durable system surveyed (CodeQL, Joern, Glean, Infer, Semgrep):

> **Store what a producer sees; derive what analysis computes.** The extensional base (EDB) is structure, bindings, places, modes, resolved targets, and small per-function summaries. The intensional layer (IDB) — SSA, reaching-defs, dominators, taint reachability — is a pure function of the EDB, computed on demand, cached as a disposable projection, **never stored**. Joern's one regret is storing computed dataflow edges that go stale.

---

# Part I — The Declaration Plane

*(Rev 2/3, retained, tightened. Occurrences are subsumed by the implementation plane — §5.)*

**U-1 One vocabulary.** Every reference-shaped fact speaks the frozen F1 encodings `S:<hex>` (same-package IntroId) and `F:<eco>/<pkg>#<hex>` (foreign). Grains: type positions (`in`/`out`/`fieldty`/`type`/`super`/`iof`/`ifor`), export chains (`retgt`), doc links (`dlink`), symbol links (`link`), and — new — every body reference (§8/§9). Deleted: graph IRIs, UUIDv5 `SymbolId` identity, `~extern` stubs, `NudoxPath::External` as terminal.

**U-2 Tree-sitter is a producer** on the same ir-stream wire as the oracles, lower fidelity. `Extraction`/`RawDefinition`/`RawReference`/`occurrence.rs` dissolve. `bootstrap_intro_id_v2` is pure over what treesitter supplies, so treesitter entries get real IntroIds.

**U-4 "Surface" is a reflection, not a structure.** `ApiSurface` as a reified artifact is cut; `exported`/`monikers`/`boundary` are pure functions over IR frames, cached as disposable projections keyed by (tip, policy, config). Dependencies are served as **IR** via `DepIrProvider::ir(pkg, pin) -> Arc<IrView>`; consumers reflect what they need.

**U-5 The resolve ladder** (7 rungs) survives; substrate changes to reflections over the staged generation + dep IR. Rung 3 (import) binds `F:` at `Import` via the `monikers` reflection; globs become prefix scans; multi-hop re-exports and dep-of-dep close via `boundary`. `RESOLVER_VERSION → "occ-v2"`. `symtab.rs` deleted.

**U-6 Producers own resolution; treesitter is the honest dumb tier.** Merge per fact = max confidence; Oracle never downgraded.

**U-7 Consumer codebases are packages** on `local/<device-id>` (offline is a branch). `CodebaseInput` does not exist.

---

# Part II — The Implementation Plane

## 5. Occurrences dissolve into bodies

A reference is not a tuple — it is a fact with structural context: which region encloses it, which place it reads, in what capability, at what type. That context is the body. So Rev 2's flat `occ` frame is **retracted**: the occurrence set of an entry is the reference-bearing frames of its body (`cal`, `use`, `cap`), and the usage index (§Part IV) is a reverse projection over their `StableRef` targets. At the dumb tier, treesitter still forms a *partial* body, so occurrences are the body at whatever fidelity was reached.

## 6. Storage: a companion body file per entry, sharing its IntroId

SIV stores each symbol as `{intro_hex}.nir`. The implementation plane adds one sibling per entry with a body: **`{intro_hex}.nb`** (NdBody; domain `nudox.body.v1`), sharing the entry's IntroId — the implementation half of the same entry, not a new identity. This answers three weak axes at once:

1. **Semantic history.** `log_for_path({intro}.nb)` = implementation history, disjoint from the `.nir` API history. `changes_between_refs` on the body file answers "what implementation shipped between v1 and v2."
2. **O(delta) + fan-out.** A body edit touches only the `.nb` file; the `.nir` is byte-identical, so `api_surface_hash`/`embed_hash` do not move — semver, graph rewrite, and re-embedding are skipped by construction.
3. **Size.** The `.nir` (what semver/embed/graph read) stays O(API surface); the `.nb` carries O(all code).

Both files re-key together under σ. New hash class:

| Hash | Domain | Covers | Gates |
|---|---|---|---|
| `body_hash` | `nudox.bodyhash.v1` | the `.nb` frames | implementation-history, def-use/taint/usage projection rebuild, example windowing — **not** semver, **not** embed |

Bodies are produced whenever the producer runs (every indexed package). Their *projection* into query indexes is demand-gated (IP projection discipline); the sovereign frames always exist.

## 7. The region taxonomy (frozen; structured control flow, not CFG edges)

The 2025 analysis-IR consensus (MLIR scf, RVSDG, WASM structured control flow; V8's retreat from sea-of-nodes) is that structured regions beat CFG edges for analysis. The body is a tree of nested regions; control-flow containment is **ancestry**. Frozen kind set (each maps losslessly across all 8 languages — Rust, Go, Java, C#, TS, JS, Python, C++):

| Region kind | Sub-structure | Exceptional edge |
|---|---|---|
| `funcbody` | 1 body; entry args = parameters; isolated-from-above | via unwind |
| `if` | `then`, `else`; **else-if = nested `if` in the else** | no |
| `switch` | scrutinee + N case arms + default; **no fallthrough** (fallthrough lowers to explicit CFG) | no |
| `match` | scrutinee + N **arms** (sub-records, see below) | no |
| `loop` | 1 body; `break`-with-value ⇒ region result | no |
| `condloop` | before(condition) + body — while / for-cond | no |
| `tailloop` | 1 body — do-while | no |
| `foreach` | iterable operand + body(block args = iter vars); iteration protocol is an attribute | no |
| `comprehension` | body producing accumulated elements | no |
| `trycatch` | body + N handler arms + `else`? + `finally`? | yes (handlers) |
| `finally` | 1 body + **discriminant arg** {normal, handled, unhandled} — sub-structure of trycatch/resource | re-raises |
| `resource` | acquire? + body(res binding) + release — RAII/using/with | yes (release on throw) |
| `defer` | 1 body per defer site; LIFO at function exit | re-raises after |
| `coroutine` | 1 body with **suspension ops** — async/generator | effects in ops |
| `spawn` | 1 body — goroutine / async.execute / thread | detached |
| `sync` | monitor operand + body — lock/synchronized | via implicit finally |
| `select` | N channel-op arms — Go select | no |
| `lambda` | 1 body; captures = block args; not isolated-from-above when capturing | no |

Three resolutions of prior confusion, each load-bearing:

- **An arm is a sub-record of a `match`/`switch`/`select` op, never a region kind.** A match arm = `(pattern: PatternAttr, guard: region?, body: region)`; `PatternAttr` is a declarative pattern tree (literal/type/struct/tuple/seq/or/binding/wildcard/range) that the exhaustiveness checker reads directly. Or-patterns are a disjunction inside `PatternAttr` sharing bindings/guard/body. This is confirmed by MLIR `scf.index_switch`, MIR, and RVSDG gamma.
- **Suspension points (`await`/`yield`) are ops inside a `coroutine` region, not region kinds.** A `susp` op terminates its block and names a resume anchor; dominance answers "is this call before/after the await" (a call is before await N iff dominated by entry but not by resume N).
- **Cleanup (`finally`/`defer`/RAII drop) uses a discriminant-argument region** carrying both normal and exceptional entry — avoiding javac's code-duplication; the discriminant re-raises on the unhandled path.
- `unsafe`/`fixed`/`unchecked`/`checked`/`consteval`/`constexpr` are **attributes on a region, not kinds** (§11).

Full 8-language lowering (abridged; the full table is the region-taxonomy research appendix): Rust `?` → inline `match` on `Result`/`Option`; Go `select` → `select`; Python `try/except/else/finally` → `trycatch` with `else` + `finally`; C#/Java `using`/try-with-resources → `resource`; every language's `async` body → `coroutine`; every closure → `lambda`.

## 8. The `NdBody` frame grammar (frozen)

One `\n`-terminated line per frame (F1: line diffs = semantic diffs). **Region nesting is bracketed by line order** — `rgn`/`end` open and close; a frame's region is the innermost open `rgn` above it. **No positional structural ids in the stored form** (F5 lesson; §13). Every typed field is a `Typed<T>` rendered `key:value@tier` where `tier ∈ {exact, inferred, syntactic}`, or **omitted entirely when `Absent`** (honest degradation, §12).

| Frame | Value grammar | Filled by |
|---|---|---|
| `rgn` | `<kind>` [`scrut:<place>`] [`args:<binding>,…`] [`proto:<token>`] [`cfg:<pred>`] [`attr:<tok>,…`] — opens a region (§7 kinds); `attr` carries `unsafe`/`fixed`/`unchecked`/`consteval` | kind from syntax; args/proto from oracle |
| `arm` | `<pattern>` [`guard`] — sub-record of match/switch/select | pattern from syntax; binding types from oracle |
| `end` | (bare) — closes innermost region | — |
| `bnd` | `<name\|_>` `<shadow-ord>` [`lty:<typeref>@t`] [`mut`] [`cap:<capability>@t`] [`cfg:<pred>`] — a local | name/shadow from syntax; lty/cap from oracle |
| `asn` | `<lhs-place>` `<rvalue>` where rvalue operands carry `(place, mode)` | places from syntax; modes from oracle |
| `cal` | `<callee-stableref>@<conf>` `<dest-place>` `<arg:(place,mode)>…` [`recv:<typeref>@t`] | callee at `suf`/`syn` from syntax; `orc` + modes/recv from oracle |
| `use` | `<place>` `<mode>` — a read/borrow/move | place from syntax; mode from oracle |
| `ret` | `<place>` [`err`] — return; `err` = error-variant return (from `?` desugaring) | err from oracle |
| `susp` | `<kind:await\|yield>` [`<place>`] `resume:<anchor>` — suspension op in a coroutine | oracle (or `?`/await syntax heuristic) |
| `yld` | `<place>…` — region result / next-iteration block-arg supply (phi-free) | syntax where structural |
| `cap` | `<captured-place>` `<capability>@t` — closure capture on a lambda region | capture set from syntax; capability from oracle |
| `sum` | summary frame — §14 (`sum.flow`/`sum.src`/`sum.sink`/`sum.san`/`sum.err`/`sum.own`) | oracle-computed or model-pack-authored |

Determinism: the merge (§12) is a pure function of the two producer emissions; canonical serialization (frames in bracket order, sets sorted) makes `body_hash` replica-reproducible.

## 9. Places and projections (cross-language, StableRef-identified)

A **place** is a root plus a projection chain — MIR-shaped, but with field identity that is cross-language and dual-tier stable. Field identity is **not** a numeric index (fragile: source-order-dependent, treesitter/oracle disagree) but a **`StableRef` to the declaring member symbol** — the convergent answer of SCIP Term descriptors, CodeQL `FieldContent`, and Jimple `FieldSignature`.

**Root** = `Local(ref: Option<StableRef>, name)` | `Global(ref: Option<StableRef>, name)` | `SelfParam` | `Synthetic(id)`.

**Projection elements (frozen, 7 kinds):**

| Element | Rendered | Covers |
|---|---|---|
| `Field { ref: Option<StableRef>, name }` | `.f<ref\|name>` | struct/class field, record component, named tuple field, Go promoted field (expanded to a chain), C# property (backing field; getter/setter is a separate `cal`), typed Python attr |
| `TupleIndex(pos)` | `.t<pos>` | Rust tuple / tuple struct, Python `tuple[i]`, C# ValueTuple |
| `Index(key)` where key ∈ `Const(lit)`\|`AnyElement`\|`AnyKey`\|`AnyValue`\|`Slice(from,to)` | `.i<…>` | array/list element (`AnyElement`), map key/value (`AnyKey`/`AnyValue`), literal index (`Const`); precision matters for dataflow (`arr[0]` tainted, `arr[1]` read ⇒ no FP) |
| `Deref` | `.d` | raw ptr, `&T→T`, Box/Arc/Rc, C++ `*`/`->` (Go pointer auto-deref is transparent, no element emitted) |
| `Downcast { variant: Option<StableRef>, name }` | `.dc<variant>` | enum variant narrow (Rust), discriminated-union narrow (TS), `is`/`instanceof` pattern (Java/C#); always followed by `Field` |
| `Await` | `.aw` | `.await`, `await`, asyncio await |
| `Captured { ref: Option<StableRef>, name }` | `.cap<ref>` | closure upvar edge (Rust/Java/C#/Python cell) |

**Dual-tier field identity — the resolve ladder at sub-symbol grain.** Treesitter emits `Field { ref: None, name: "foo" }` (name always present). The oracle fills `ref: Some(StableRef)`. Equality uses `ref` when both sides have it (stable across versions and tools, version-agnostic by stripping the SCIP package version segment), falling back to `name` when one side is unresolved. When a semantic pass resolves `None → Some`, it patches matching `(name, context-type)` projections. This is *identical* to symbol resolution (U-5) one level down — one confidence model for the whole system, StableRef all the way through (U-One-Algebra).

**Per-language idioms, resolved (no special kinds):** Go embedded `s.B.F` → `Field(B), Field(F)` chain (type-checker expands during extraction). C# `obj.Prop` → `Field(backing)` for heap dataflow + a `cal` to the getter in the call graph. TS `a?.b?.c` → `Field(b), Field(c)` — optional-chaining short-circuit lives in control flow, not the projection. JS `a[expr]` → `Index(AnyElement)` (dynamic) or `Index(Const("k"))` (literal, upgradable to `Field(None,"k")`). Python `getattr(o, s)` / `__getattr__` → `Field(None, s)` (literal `s`) or `Index(AnyKey)` (dynamic) — the same widening Pysa uses.

"What fields of `req` are read?" = `use` frames whose root binding is `req`, read off the `Field` projections. Structural, stored, no analysis.

## 10. The universal capability annotation (modes are an axis tuple, not a Rust enum)

Rust's `move/copy/borrow/clone` is a *point* in a larger space; a GC language has no `move` and `clone` is noise. The ideal is the **Pony deny-capabilities lattice extended with escape analysis** — a `mode` is an **orthogonal axis tuple**, each language filling the subset that applies:

| Axis | Lattice |
|---|---|
| **write** | `none < read < readwrite` |
| **alias** | `consumed(0) < unique(1) < shared-read < shared-rw` |
| **transfer** | `∅ < borrow < copy < clone < move` (∅ where the language has no transfer notion at this site) |
| **escape** | `local < caller < heap < thread < process` |
| **sync** | `none < ordering < exclusive < immutable` |

Eleven canonical tokens name the load-bearing tuples (each expands to an axis assignment); a producer emits the token from its language's subset:

`consume` · `own` · `mut-ref` · `ref` · `copy-val` · `clone-val` · `shared-ro` · `shared-rw` · `send-move` · `send-val` · `tag`

Per-language mapping (abridged; full table in the capabilities research appendix): Rust `let y=x`(non-Copy)→`consume`, `&x`→`ref`, `&mut x`→`mut-ref`, `.clone()`→`clone-val`, `spawn(move…)`→`send-move`, `Arc<T:Sync>`→`shared-ro`, `Arc<Mutex>`→`shared-rw`. Go value→`copy-val`, `&x`→`shared-rw`, `go f(v)`→`copy-val`+escape=thread. Java/JS/Python object ref→`shared-rw`, `final`/frozen→`shared-ro`, `postMessage(v,[buf])`→`send-move`, `structuredClone`/pickle-spawn→`send-val`/`clone-val`. C# `struct`→`copy-val`, `ref`→`mut-ref`, `in`→`ref`, `class`→`shared-rw`. C++ value→`copy-val`, `std::move`→`consume`, `unique_ptr`→`own`, `thread(f,move v)`→`send-move`.

The payoff: **"moved or cloned into the spawn" is the language-agnostic compound predicate `escape ≥ thread ∧ transfer ∈ {move, clone}`**, validated across all 8 languages — the identical query in Rust, Go, JS, and Python. Captures on a `lambda`/`spawn` region carry the same tuple via `cap` frames.

## 11. `cfg` and block attributes are per-node, not per-region-only

`cfg` is an attribute frame attachable to **any body node** — region, binding, statement, or arm — using the frozen SIV §8.6 normalized predicate grammar (`all`/`any`/`not`/`feature=`/`target_os=`/`target_arch=`/`other:`). The producer normalizes `cfg_attr` and complex `all(any(not(...)))` into that grammar at lower time, on exactly the affected node. "Is this inside `#[cfg]`?" = does this node **or any ancestor** carry a `cfg` predicate (evaluated symbolically). Every cfg site — item-level, statement-level, or region-level — gets a normalized predicate; nothing is partial. `unsafe`/`fixed`/`unchecked`/`consteval` ride the same per-node attribute mechanism (`rgn … attr:unsafe`, or `attr:` on a statement frame).

## 12. The dual-tier merge algorithm (record-time; "same line" made precise)

Two producers emit a body at different fidelities; the host merges them into one canonical `.nb` **at record time** (inside the session, after both emit, before `finish`). The research is decisive: **no tree-diff algorithm bridges a syntactic tree and a desugared typed tree** (GumTree/truediff/SAT-DIFF are edit-distance between two versions of the *same* tree). The merge is therefore a **deterministic join on a shared anchor key**, with span-containment for the residue.

**Anchor key** (each producer computes independently):
```
AnchorKey { struct_path: Vec<(NodeTypeCanonical, sibling_ordinal)>, content_hash: Option<u64> }
```
`struct_path` is the path from the function root using a per-language **CANON_MAP** — a static table mapping *both* tree-sitter node types *and* oracle/HIR node types onto the same canonical strings (e.g. tree-sitter `call_expression` and HIR `Expr::Call` both → `"call"`). Building CANON_MAP per language is the honest calibration cost; it is a table, not an algorithm. `content_hash` is a HyperAST-style structural hash `hash(canon_type, leaf_label, [child hashes])`, set for leaf/shallow nodes as a join corroborator.

**Four dispositions per node:**

1. **Both emit the same anchor** → `merge_matched`: the oracle fills `resolved_type`, `resolved_symbol`, `recv_type`, modes, dataflow; the syntactic node keeps `source_span` and `children`. Span reconciliation: prefer the span contained within the other (finer-grained); if neither contains the other, oracle wins.
2. **Syntactic-only** (oracle skipped it) → all typed fields `Absent`.
3. **Oracle-only + desugared/macro** (`for`/`?`/`async`/`format!` expansions; `provenance.desugaring_kind.is_some()` or non-empty `expansion_chain`) → a **synthetic node**: no anchor, attributed to its originating source span via `SyntheticOrigin { desugaring_kind, originating_span, parent_anchor }` (the nearest syntactic node whose span encloses it). All synthetic nodes from one macro share the `macro_call_site` span.
4. **Oracle-only + not desugared** (grammar mismatch — autoref, coercion) → **span-containment fallback**: attach under the deepest syntactic node whose span contains it, with a synthetic anchor suffix (`content_hash = None`, marking non-independently-reproducible).

**Call-chain granularity** (`uri().path()`: one syntactic `call_expression`, two oracle `MethodCall`s) is reconciled by a `CallChainGroup` record: the syntactic node keeps the full-chain span; the oracle sub-calls attach as ordered `chain_steps` with per-step types. Both views preserved, lossless.

**Confidence is first-class.** `Typed<T> = Present(T, tier) | Absent`, `tier ∈ {exact, inferred, syntactic}`; `Absent` is a sentinel, never null. Body-level `BodyConfidence = FullOracle | SyntaxOnly | Partial{oracle_coverage}`. Provenance per node = `{source_span, expansion_chain, desugaring_kind, macro_call_site, transparency}` (rustc `DesugaringKind`'s 13 variants, rust-analyzer `HirFileId`/`ExpansionInfo`, MLIR `FusedLoc` for multi-origin). This is the degradation quality signal: consumers pattern-match on the tier and never fabricate an `Absent` fact.

Complexity O(N log N). Determinism: same source + same producers → same anchors → same canonical frames → same `body_hash`.

## 13. Edit-stable local identity and cross-version continuity

The stored `.nb` uses **bracket nesting + line order** for structure (no positional ids ⇒ inserting a statement inserts one line). Cross-linking within a body:

- **User bindings** = `name` + `shadow-ordinal` (count of same-name bindings lexically enclosing; deterministic). Stable unless a shadow is added/removed (a real semantic change).
- **Temporaries** = `t:<blake3(rvalue-skeleton)>` scoped to the enclosing region (the SIV §4.3 disambiguator trick at body grain, = a HyperAST content-hash of the defining expression). A temp for `hash(user_input)` keeps its name under unrelated edits; when its defining expression changes it is genuinely a new value. **Never sequential SSA numbers** (they renumber on insert, destroying diff-stability).

**Cross-version continuity is NOT GumTree.** Two mechanisms, both already in the design: (a) the anchor `struct_path` is stable under sibling-preserving edits and its `content_hash` matches moved-but-unchanged subtrees, giving deterministic node correspondence across versions; (b) pijul line history (`log_for_path({intro}.nb)` at line grain) *is* the blame — "which change last touched this frame." No heuristic tree-diff, no stored continuity edges, and the failure mode (a structural edit changes `struct_path`) is caught by `content_hash` corroboration + span containment, exactly as in the intra-version merge. Durable identity stops at the entry IntroId + named bindings (the Unison finding — locals are not independently content-addressed — accepted deliberately).

## 14. EDB/IDB: what is stored, and the summary ruleset

**Stored (EDB):** the `.nb` frames of §8, plus small per-function summary frames — the inter-procedural composition interface, stored because callers must compose them without loading callee bodies (CodeQL Models-as-Data shape):

| Summary frame | Fields | Purpose |
|---|---|---|
| `sum.flow` | `<in-ap>` `<out-ap>` `<kind:taint\|value>` `@<tier>` | value/taint propagation (argument→return TITO) |
| `sum.src` | `<out-ap>` `<src-label>` `@<tier>` | function originates a source |
| `sum.sink` | `<in-ap>` `<sink-label>` `@<tier>` | function has a sink |
| `sum.san` | `<ap>` `<sink-label>` `@<tier>` | sanitizer/barrier for a sink kind |
| `sum.err` | `<in:ap\|internal>` `<out:ret\|ap>` `@<tier>` | error propagation (the `?`/exception spine) |
| `sum.own` | `<arg-idx>` `<effect: capability>` `@<tier>` | per-arg ownership effect |

Access paths (`ap`) = `arg:<i>[.proj]* | ret[.proj]* | this[.proj]*`, the same projection vocabulary as §9.

**Summary computation (the exact ruleset).** Summaries are computed by a Datalog/Ascent program over the stored EDB, bottom-up over call-graph SCCs in reverse topological order (Pysa/IFDS discipline). The core rules (Soufflé-style; the full ruleset is the taint research appendix):

```
taint(AP, K, F)              :- isSource(AP, F, K).
taint(To, K, F)             :- taint(From, K, F), assign(To, From, F).
taint(Base.field, K, F)     :- taint(Val, K, F), store(Base, field, Val, F).
taint(To, K, F)             :- taint(Base.field, K, F), load(To, Base, field, F).
taint(RetAP, K, F)          :- taint(ArgAP, K, F), call(S,Callee,I,ArgAP,F),
                                sum_flow(Callee, param(I), _), ret(S, RetAP, F).
vuln(F, AP, Ks, Kk)         :- taint(AP, Ks, F), isSink(AP, F, Kk).
sum_flow(F, param(I), Ret)  :- taint_from_param(param(I), Ret, F), ret_in_func(F, Ret).
sum_sink(F, param(I), Kk)   :- taint_from_param(param(I), SinkAP, F), isSink(SinkAP,F,Kk).
sum_src(F, Ret, K)          :- isSource(Int,F,K), taint_from_source(Int, Ret, F).
```
CodeQL's `FlowSummaryImpl` compiles each `(input-stack, output-stack, kind)` into atomic `summaryLocalStep`/`summaryStoreStep`/`summaryReadStep`/`summaryThroughStep{Taint,Value}` — the same shape.

**Access-path k-limiting failure and the fix.** k-limiting (bounded path length) fails on builder chains, nested maps, recursive structures, and functional pipelines. The ideal, most-ambitious answer: **k=4 bounded access paths as the primary representation, plus Pysa-style tree widening** (collapse-to-content: when width/depth thresholds are exceeded, collapse children into an `AnyContent` node, tag the flow `broadened` — sound over-approximation, never a false negative), **plus access-path fragments** for summary composition (a summary maps `Argument[0] → ReturnValue.foo.bar` as a delta `[+foo, +bar]` applied to whatever path the argument had), **plus on-demand Boomerang/SPDS** for specific alias queries at call sites (avoiding whole-program cost). This is decidable, tractable, and precise on >95% of real flows.

**Derived (IDB), never stored, computed behind the query adapter (§17), cached as disposable projections keyed by `body_hash`:** reaching-defs, SSA, def-use chains, dominators (per body from `bnd`/`asn`/`use` + region tree); flat CFG (lowered from the region tree when an algorithm needs it); call graph (from `cal` targets); taint reachability (the ruleset above, an Ascent fixpoint); usage/reverse-reference index; error-origin paths (backward def-use over `ret err` + `sum.err`).

## 15. The source/sink/sanitizer ontology and soundness posture

**Frozen label vocabularies** (CodeQL/OWASP-aligned, CWE-mapped):

- **Source kinds:** `remote` (network — default-enabled) · `file` · `commandargs` · `database` · `environment` · `stdin` · `windows-registry` · `android` · `view-component-input`. Grouped under threat models (`remote` default; `local` subcategories opt-in).
- **Sink kinds:** `sql-injection`(CWE-89) · `command-injection`(78) · `code-injection`(94) · `path-injection`(22) · `xss`/`html-injection`(79) · `js-injection`(79) · `url-redirection`(601) · `unsafe-deserialization`(502) · `log-injection`(117) · `ssrf`(918) · `ldap-injection`(90) · `xpath-injection`(643) · `header-injection`(113) · `template-injection`(94) · `regex-injection`(1333).
- **Summary kinds:** `taint` (derived) · `value` (exact copy/alias).
- **Sanitizer categories:** `html-sanitizer` · `sql-sanitizer` · `path-sanitizer` · `url-sanitizer` · `hash-sanitizer` · `validation-sanitizer` · `command-sanitizer`, each keyed to the sink kind it neutralizes.

**Default sources attach via model packs** (§16): framework types (Flask `request.*`, Django `HttpRequest.body/GET/POST`, servlet `ServletRequest`) are `sourceModel` rows with `subtypes:true` so a whole class hierarchy is covered by one row. The most-ambitious tier is framework-native recognition (Semgrep-Pro-style: the engine understands the request lifecycle so view-parameter `request` objects are sources without an explicit row).

**Confidence tiers on every summary** (first-class, degrade to the lowest fidelity in the transitive closure):

| Tier | Condition | Consumer use |
|---|---|---|
| `definitive` | all callees resolved from source; no widening; full types | high-priority finding |
| `inferred` | some callees from model packs; no widening | normal finding |
| `speculative` | ≥1 unmodeled callee (conservative taint-through) or widening occurred | low-priority; human triage |
| `syntactic_only` | summary from tree-sitter-only IR (name-heuristic access paths) | documentation hint only; never a security alert |

**Soundness posture (stated honestly):** *sound within modeled scope, explicitly scoped.* "When a function and its transitive callees are covered by IR and models, the summary is complete; for unmodeled callees the analysis applies conservative taint-through (selective: taint-through for same-package/known-container patterns, taint-drop for opaque third-party) and tags the result `speculative`." Never claim global soundness; never silently drop without a tier.

**Dual-fidelity summaries:** a syntactic-only body yields name-based `syntactic_only` summaries (a documentation hint, not an alert); when the same function is later analyzed at oracle fidelity, the higher-fidelity summary supersedes it (`superseded_syntactic` provenance). A semantic caller composing a syntactic-only callee summary is demoted to `speculative`.

## 16. Models are packages (the extension point, with ops)

A model for code you do not control (opaque native/FFI/dynamic libraries) is a **package** on `overlay/models`: entries carrying `sum.*` frames whose access paths target foreign `StableRef`s. Ops, fully specified:

- **Who publishes:** IP ID-26 trust — enrolled/mTLS principals publish model packs; anonymous clients are read-only consumers.
- **Precedence (deterministic, by the confidence lattice):** a computed **Oracle-tier** summary from a real analyzed body > an **authored** model summary > a **syntactic-only** summary. When a real body later appears, its Oracle summary supersedes the authored model automatically (`superseded` provenance) — no manual override.
- **Merge:** max-confidence per `(function, in-ap, out-ap)`; ties broken by provenance order (local channel > overlay). Identical to the U-6 producer merge, one rule for the whole system.

---

# Part III — The Query Plane

## 17. One Trustfall adapter over all planes

Open-ended questions are answered by **one Trustfall adapter** over catalog + declaration plane + implementation plane + derived indexes — the cargo-semver-checks architecture (proven at 245-lint scale), generalized. The schema is the stable, major-versioned public API; it absorbs IR-format churn. Vertices span planes (`Package → FunctionDecl → Region → CallSite → Use`); edges cross plane boundaries in `resolve_neighbors`. **Laziness is the architecture:** expensive edges (`reaches_sink`, dominance) fire only when traversed; fixpoint analyses live **behind** the adapter as cached Ascent/Salsa computations keyed by `body_hash`, **never** simulated with `@recurse` (which is bounded traversal, not iteration to fixpoint).

## 18. Policies-as-queries: one surface for semver, security, and navigation

Because cargo-semver-checks already *is* Trustfall queries over rustdoc, **semver lints, security/dataflow lints, navigation, and reverse-impact are all queries over the same adapter**, differing only in which plane they touch. Semver Packs A/B/C query the declaration plane; taint/error-origin lints query the implementation plane; "who breaks if this ships" joins the usage index with semver findings. A lint is a data file (RON: id, query, `required_update`/severity, witness) — the public extension point every successful system shares (stable schema, zero-code authoring, isolated testability, performance in the adapter not the query text). SIV amendment A4 (§20): the semver engine adopts this surface so semver and security share one mechanism.

---

# Part IV — Products over the planes

**U-8 The graph is the reverse index over StableRef positions, served as routes.** No graph system, no client engine. Nodes = IntroIds; edges = the StableRef grains + `cal`/`use`/`cap` + `Member` + `Lineage` (continuity ops) + package-grain `DependsOn`. Backend materializes a disposable outbox-fed reverse index; frontends (web JS, anything) consume routes (`Query{Usages}`, `/symbols/:ref/{implementors,mentions,callers,reaches}`, `/expand`). Wire = domain. `cal` edges reach the graph only at `≥ Index` confidence; Suffix never becomes an edge.

**U-9 Examples = usage query × the semver plane; staleness is computed.** Usage rows filtered `conf ≥ Import`, owner `source_kind ∈ {test,example,doc}`, clustered by body region-skeleton, window-extracted, ranked, and staleness-badged by the target's `api_surface_hash` + `ApiReport` findings (Exact / Compatible+detail / Breaking-since-recorded+typed FindingDetail / rename-note). Doc-only edits never invalidate examples; a signature change invalidates exactly the examples pointing at it — the fourth consumer row of SIV's hash-class table.

**U-10 Frontend wire contracts** are route responses: `RefsPage`←usage rows; precision badge←`Confidence`; `ImplsPage`←implementors route; `LineageEvent`←`IrOp` + `Finding`; usage-examples feed←U-9; "in a loop / which arm / fields read / moved-or-cloned"←implementation-plane routes over region ancestry, places, and capability tuples; graph view←`/expand` + routes (no client engine).

---

## 19. Scorecard: the eight weak axes and the example questions

| Weak axis | Rev 4 mechanism |
|---|---|
| Scope | region tree; scope = enclosing `rgn` ancestry (§7) |
| CFG | structured region kinds; flat CFG derived (§7/§14) |
| Local identity | name+shadow / `t:<skeleton-hash>`; cross-version via anchor + pijul line history (§13) |
| Dataflow | `asn`/`use`/`cal` + places+projections (§9) + capability tuples (§10) [EDB]; reaching-defs/taint [IDB] (§14) |
| Typing in body | `lty` typerefs on `bnd`, StableRef, confidence-tagged (§8/§12) |
| Dual-tier alignment | the anchor-join merge algorithm (§12) — a deterministic join, not "same line" |
| Semantic history | companion `.nb` file; `log_for_path` splits impl from API history (§6) |
| Open-ended Trustfall | one adapter, fixpoints cached behind it, policies-as-queries (§17/§18) |

| Question | Answer |
|---|---|
| "Where does this error originate?" | backward def-use over `ret err` place + `sum.err` composition (§14) |
| "Does user input reach this sink?" | taint reachability composing `sum.src`→`sum.flow`*→`sum.sink` with the §15 ontology (Ascent fixpoint behind the adapter) |
| "Is this value moved or cloned into the spawn?" | the compound predicate `escape ≥ thread ∧ transfer ∈ {move,clone}` over `cal`/`cap` capability tuples (§10) — same query all 8 languages |
| "What fields of `req` are read?" | `use` frames rooted at `req`, `Field` projections (§9) — structural |
| "Is `handle_api` only on the `/api` branch?" | every `cal handle_api` dominated by an `if`/`arm` region whose scrutinee tests the condition (§7 region query) |
| "What's assigned to `path` before its first use?" | reaching-def over `asn`/`use` for `path`'s binding (§14 IDB) |
| "Is this call inside a loop / async / unsafe / `#[cfg]`?" | `rgn` ancestry for kind / `attr`/`cfg` predicate (§7/§11) |
| "Which match arm contains this?" | nearest enclosing `arm` sub-record of a `match` region (§7) |

---

## 20. Proposed SIV amendments

| ID | Amendment |
|---|---|
| **A1** | §9.7 per-config `ApiSurface` snapshots cease to be sealed artifacts; `surface(ir, policy, cfg)` is a reflection cached as a disposable projection. |
| **A2** | §9.1 `DepSurfaceProvider` → `DepIrProvider` (serves dep IR at the pin). |
| **A3** | Retract Rev 2's `occ` key. Add the **`.nb` companion body file** (`nudox.body.v1`; §7–§16 grammar/algorithms), the `body_hash` class (`nudox.bodyhash.v1`), and lowering-contract duty (d) = dual-fidelity body + summary emission. `ResolutionStats` joins `GenerationMeta`. |
| **A4** | §9 semver adopts the shared **Trustfall query surface** (Part III); semver and security lints share one policy mechanism. |

## 21. The dead list

Rev 2 `occ` frame / `OccurrenceSeal` / `generation_artifacts` (subsumed by the body plane); `Occurrence.role`/`.enclosing`/`.anchored` and `occurrence.rs` (unrepresentable); flat `Vec<Occurrence>` model (region-structured body); Rust-only `mv|cp|rs|rm|cl` modes (capability tuple §10); numeric `FieldIdx` place identity (StableRef §9); "same line" as a merge story (the §12 algorithm); `ApiSurface` structure / per-config snapshots / `DepSurfaceProvider` (§U-4); `SymbolTable`/`symtab.rs`; `Extraction`/`RawDefinition`/`RawReference` boundary types; `CodebaseInput`; bespoke `UsageCorpus`; **Ladybug/GD-34/client graph engine/KuzuDB dependency** (archived upstream); graph IRIs / UUIDv5 `SymbolId` identity / `~extern`; `occurrences_ref` on `BlobManifest`; stored reaching-def/SSA/CFG edges (the Joern regret); legacy `CstSet`/`ResolvedReference`/`references_ref`.

## 22. Phasing (rides existing roadmaps)

| Slice | Contents | Rides |
|---|---|---|
| **UR-1 decl wire** | StableRef vocabulary; treesitter-as-producer emitting declaration frames | SIV P1 |
| **UR-2 resolve-in-session** | host resolve between stage and finish; σ over targets; `occ-v2`; `GenerationMeta` stats | SIV P2 |
| **UR-3 reflections** | `exported`/`monikers`/`boundary` + `DepIrProvider`; ladder rung 3 | SIV P4 (A1/A2) |
| **UR-4 body skeleton** | `.nb` file; region taxonomy §7; frame grammar §8; places §9; `cfg`-per-node §11; edit-stable naming §13; treesitter emission | SIV P1/P2 (A3) |
| **UR-5 dual-tier merge** | CANON_MAP tables; the §12 anchor-join algorithm; `Typed<T>`/provenance; `body_hash` determinism | SIV P2 |
| **UR-6 oracle enrichment** | duty (d): RA/OXC/… fill `lty`, capability tuples §10, resolved `cal`, `cap` kinds, `susp` | SIV P7 |
| **UR-7 summaries + models** | `sum.*` computation (§14 ruleset), access-path widening, the §15 ontology + confidence tiers, `overlay/models` packs (§16) | SIV P7 + IP overlays |
| **UR-8 query plane** | one Trustfall adapter (§17); Ascent/Salsa fixpoints behind it; usage/graph/impl routes | IP IP-1/IP-3 |
| **UR-9 policies-as-queries** | lint RON format; semver + security + reverse-impact on one surface; witnesses | IP IP-3 (A4) |
| **UR-10 examples + wire** | ranker, computed staleness, `RefsPage`/`UsagePage`/`LineageEvent` responses | IP IP-3 + GUI |

## 23. Acceptance

| Test | Assert |
|---|---|
| O-1 body locality | 10k symbols, edit 3 bodies → only 3 `.nb` re-diff; `.nir` byte-identical; `api_surface_hash`/`embed_hash` unmoved |
| O-2 σ-through-body | rename a type referenced in 50 bodies → all `cal`/`use`/`lty`/place targets rewritten by σ; files byte-identical to from-scratch |
| O-3 merge determinism | two hosts, same source + producers → identical `.nb` incl. anchor-join result; identical `body_hash` |
| O-4 desugar attribution | `for`/`?`/`async` → synthetic nodes attributed to originating span; syntactic view intact; no faked 1:1 |
| O-5 call-chain | `uri().path()` → syntactic node keeps full span; `CallChainGroup` carries 2 typed steps |
| O-6 dual-tier degradation | syntactic-only body → typed fields `Absent`, `BodyConfidence=SyntaxOnly`; oracle pass fills fields, no structural rewrite |
| O-7 cross-language capability | "moved-or-cloned-into-spawn" via `escape≥thread ∧ transfer∈{move,clone}` returns the right sites in Rust/Go/JS/Python fixtures |
| O-8 field identity | `req.body.name` place uses `Field(StableRef)` when oracle ran, `Field(None,name)` at treesitter; equality holds across the two |
| O-9 structural answers | in-loop / which-arm / fields-read / moved-or-cloned answered from stored frames with **no fixpoint** |
| O-10 summary compose | user-input→sink composes `sum.*` across the call graph; unmodeled callee ⇒ `speculative` tier, never silent drop |
| O-11 access-path widening | deep builder chain → widened summary tagged `broadened`, sound (no FN) |
| O-12 model precedence | authored `overlay/models` summary merges max-confidence; real Oracle body supersedes it (`superseded` provenance) |
| O-13 route conformance | usage/implementors/mentions/reaches/lineage routes match a direct fold over frames; `AsOf` returns historical sets |
| O-14 no-binary paranoia | pathological body frames always take the text path (`has_binary_files == false`) |

## 24. Ledger

| ID | Rule |
|---|---|
| **U-IR-Universe** | Resolution + implementation are IR frames or pure reflections/derivations over them. No sibling artifact planes, no reified surfaces, no sidecar contracts |
| **U-Two-Planes** | Declaration (`.nir`) and implementation (`.nb`) share one IntroId, one vocabulary, one σ, one channel; split by hash class and history |
| **U-Store-See-Derive-Compute** | Store what a producer sees (structure, places, capabilities, targets, summaries); derive SSA/reaching-defs/dominators/taint. Never store the derived |
| **U-One-Algebra** | Every reference-shaped fact — type ref, reexport, doc link, symbol link, body call/use/capture, **and field identity in a projection** — speaks StableRef |
| **U-Regions-Not-Edges** | Control flow is a region tree; containment is ancestry; arms are sub-records; suspensions are ops; flat CFG is derived |
| **U-Capability-Tuple** | A value-flow mode is an orthogonal axis tuple {write,alias,transfer,escape,sync}; each language fills its subset; cross-language queries are compound predicates over axes |
| **U-Anchor-Join-Merge** | Dual-tier bodies merge by a deterministic join on a shared anchor key (+ span-containment residue), at record time; "same line" is never assumed |
| **U-Confidence-First-Class** | Every body fact is `Typed<T> = Present(T,tier) \| Absent`; `Absent` is a sentinel; degradation is an emitted quality signal, never a fabrication |
| **U-Edit-Stable-Names** | User bindings = name+shadow; temps = skeleton-hash; never positional SSA numbers; cross-version continuity = anchor + pijul line history, not tree-diff |
| **U-Summaries-Are-The-Interface** | Per-function `sum.*` frames (CodeQL-MaD shape) are the stored inter-procedural interface; taint reachability composes them behind the query plane; k-limit failure handled by widening + fragments + on-demand SPDS |
| **U-Honest-Soundness** | Sound within modeled scope, explicitly scoped; unmodeled ⇒ conservative taint-through tagged `speculative`; syntactic-only ⇒ documentation hint, never a security alert |
| **U-Models-Are-Packages** | Library dataflow models are `sum.*` frames in overlay packages; precedence by the confidence lattice; real bodies supersede authored models |
| **U-One-Query-Surface** | Semver, security, navigation, examples are Trustfall queries over one adapter; fixpoints live behind it, never in `@recurse`; policies are data files |
| **U-Graph-Is-Routes** | The graph is the backend's reverse index over StableRef positions, served as routes; frontends hold view state, never a graph model |
| **U-Computed-Staleness** | Example freshness derives from the target's `api_surface_hash` + typed findings — never string matching |
