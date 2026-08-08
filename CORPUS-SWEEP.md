# CORPUS-SWEEP.md — seven-ecosystem sweep, synthesized

Seven agents swept the 140-package / 154-version-entry corpus concurrently, one
per ecosystem, each running real third-party packages through the real
`nudox_producer::produce()` pipeline (invoke → lower → `Lowering::finish` → seal).

This document synthesizes their seven reports. Every cross-cutting claim below was
re-verified against the source tree by the synthesizing agent — not taken on faith
from the reports — and the verification command is given inline. Where a report's
claim did not survive checking, that is stated.

Build gate at time of writing, clean:

```
RUSTC_BOOTSTRAP=1 cargo check --workspace --all-targets \
  --exclude index --exclude registry --exclude ir-vcs --exclude driver
Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.78s
```

---

## Headline

| | |
|---|---:|
| Version entries lowered | **110 / 154 (71.4%)** |
| Live IR entries produced | **420,231** |
| Version entries that "passed" while producing no usable API surface | **26 (16.9%)** |
| Real producer defects found and fixed this sweep | **7**, across 5 producers |
| Ecosystems hit by the *same* declaration-identity defect | **5 of 7** |

The 26 is the number to worry about, not the 44 failures. Failures are loud and
get fixed. Those 26 entries returned `Ok` with a stub or near-empty table, passed
their own harnesses, and are counted as successes in five of the seven reports.

---

## Per-ecosystem

| ecosystem | producer | lowered | entries | failures | defects fixed this sweep |
|---|---|---:|---:|---:|---|
| crates.io (Rust) | `nudox-producer-rust` (in-process rust-analyzer) | 23/23 | 79,273 | 0 | 1 producer + 1 fixture |
| go | `nudox-producer-go` (Go oracle subprocess) | 22/22 | 105,785 | 0 | 1 producer (unblocked 4 pkgs) |
| nuget (C#) | `nudox-producer-csharp` (Roslyn via dotnet) | 21/22 | 101,818 | 1 | 0 (1 pinned as expected-fail) |
| cpp | `nudox-producer-clang` (libclang via dlopen) | 21/21 | 79,090 | 0 | 0 (verification-only pass) |
| npm (TypeScript) | `nudox-producer-typescript` (OXC in-process) | 18/22 | 36,011 | 4 | 3 producer |
| maven (Java) | `nudox-producer-java` (javac doclet subprocess) | 5/22 | 18,232 | 17 | 1 producer |
| pypi (Python) | `nudox-producer-python` (pyrefly) | **0/22 real** | 22 | 0 crashes, 22 silent no-ops | 1 producer (attrs) |

Notes that change how the table should be read:

- **npm's 18 includes 4 degenerate results.** `lodash` 4.17.21 and 4.17.20 lower
  to **2 entries each**, `debug` to 2, `ws` to 3 — packages with hundreds of real
  functions. Substantive npm coverage is **14/22**, not 18/22.
- **pypi's 22 entries are 22 root modules.** Every package returns exactly 1 entry
  because `PythonProducer::invoke` discards `src` and returns
  `PythonOracle::default()` without the `pyrefly` feature — the only configuration
  this workspace can build (blocked by L43's `blake3` conflict, independently
  reproduced this sweep).
- **cpp is 21 entries, not 20**: 20 packages plus the `nlohmann-json` v3.10.5
  lineage pair. 23+22+22+22+22+21+22 = 154, reconciling with `corpus/manifest.toml`.

---

## Cross-cutting defects

This is the part worth acting on. A bug in one producer is a bug; the same bug in
five is an architectural finding.

### CC-1 — Declaration identity is a lossy string projection, in 5 of 7 producers

**The single most productive finding of this sweep.** In one pass, five producers
independently failed the same way: the id a producer mints for a declaration is an
ad-hoc string encoding of a structurally richer language-level identity, and the
encoding is not injective. Two distinct declarations collapse onto one id, and
`Lowering::finish` rejects the package.

| producer | what the id discarded | packages hit | status |
|---|---|---|---|
| Go | Go's blank identifier `_` is a *legal, repeatable* param/result name; `member_name: format!("param:{}", …)` only fell back to a positional index when the name was `is_empty()`, so `func (h discardHandler) Handle(_ Context, _ Record)` minted `param:_` twice | viper, prometheus/client_golang, go-redis/v9, go-cmp (**4**) | FIXED |
| Java | `type_erase` rendered a `TypeMirror::Typevar` as its bare source letter `T` rather than the erasure of its bound (JLS 4.6), so commons-lang3's four unrelated `<T> Validate.notEmpty` overloads erased to one `JavaId` | commons-lang3 (**1**) | FIXED |
| TypeScript | `TsId` omitted the enclosing `namespace` chain, so two sibling namespaces declaring the same member collided (`zod`'s `objectUtil::identity` vs `util::identity`; `@types/node`'s dozens of `fs.d.ts` `__promisify__`) | zod ×2, @types/node (**3**) | FIXED |
| TypeScript | star re-export fan-out had no dedup when one name is reachable via two convergent barrels (`date-fns` `longFormatters` via both `format.js` and `parse.js`) | date-fns (**1**) | FIXED |
| C# | the Roslyn docId is unique *per compilation*, but the oracle is MSBuild-free by design and feeds `JsonReaderHelper.netstandard.cs` **and** `JsonReaderHelper.net8.cs` — mutually exclusive TFM variants — into one compilation | System.Text.Json (**1**) | pinned as expected-fail |
| Rust | `impl_display_name` leaked `<T as IntoParallelIterator>::Item` — a `::` inside a generic arg — into `Symbol.name`, violating the name index's "names are identifiers" invariant | rayon (**1**) | FIXED |

**11 version entries across 5 ecosystems, from one sweep.** Verified id types:

```
rust: RaId   go: GoId(enum)   java: JavaId(String)   typescript: TsId(struct)
csharp: String   clang: Usr   python: PythonId
```

Two sub-causes, worth separating because they need different fixes:

- **(a) Lossy projection** — Go, Java, TypeScript. Note Go and TypeScript use
  *structured* id types (a `GoId` enum, a `TsId` struct) and still collided,
  because a *field* of the structure is a formatted string that dropped a
  distinction. Structure at the top level does not buy injectivity underneath.
- **(b) Input over-collection** — C#. The identity is fine; the oracle's file
  *selection* is wrong. Fixing this means either MSBuild integration (explicitly
  out of scope in the oracle's own docs) or a TFM-preference rule.

**What makes this an architectural finding rather than six bugs:** `Producer::Id`
carries no obligation that the projection be injective, and nothing type-checks it.
The only thing standing between a lossy id and a silently-wrong table is
`Lowering::finish`'s `LoweringError::Duplicate` (`workspace/ir/model/src/lower.rs:404`).

**That gate is the hero of this sweep** and deserves saying plainly: it caught
five producers' identity bugs on real code that a combined **47 (Go) + 27 (TS) +
20 (C#) hand-authored fixture tests had all passed**. This is doctrine §4's
fixture parable reproduced at corpus scale — every one of those suites was 100%
green against a defect that made real packages unlowerable.

### CC-2 — Foreign type identity: L39's open half, reconfirmed corpus-wide

L39 already records this as root cause. This sweep independently reconfirms its
coverage table is still exactly right. Verified by counting `refer_import` /
`ForeignKey` call sites per producer:

```
rust: 13   go: 7   java: 7   csharp: 7   typescript: 0   clang: 0   python: 0
```

Four emit foreign keys; three do not — matching L39's table and task #12 exactly.
New corpus evidence for the open half:

- **clang** (`lower.rs:503-509`): `OracleType::Named { args: [] }` → `Type::TypeVar(name)`,
  under a comment saying "use TypeVar as a string ref to avoid the overhead of a
  full Nominal RawRef resolution. The caller can post-process." No caller does.
  Real in **all 20** cpp packages — every `std::string` renders indistinguishably
  from a resolved type.
- **Python** — non-builtin names fall to `Type::Any`; the recognized-builtin list
  is **6 names** (`int`, `float`, `bool`, `str`, `bytes`, `None`), so `dict[str,int]`
  becomes `Apply { base: Any, args: [str, int] }`: the arguments survive, the
  container's identity does not. `Dict|dict[` matched **137 files** across 6 fixtures.
- **Go is the counter-example and the existence proof**: it correctly routes
  package-external types through `refer_import`, with 53 (x/text), 60
  (client_golang), 49 (gin), 48 (go-redis) unlinked-but-*typed* foreign refs
  reported in `SealReport.unlinked`. The work is well-defined and known-doable.

L39's TypeScript/clang cases remain **worse than `Type::Any`**: a consumer cannot
distinguish them from a genuinely resolved type.

### CC-3 — The typed error contract exists and is unused by 6 of 7 producers

`ProducerError::DependenciesUnresolved` is a **shared** variant in
`workspace/compiler/producer/src/lib.rs:206`, with a doc comment that describes
this sweep's Java situation almost word for word — "the resulting table looks
complete and is not, which is why it needs a variant of its own."

Verified: **only the Rust producer ever constructs it.**

Java's 17 failures are *precisely* this failure mode. `JavaProducer::invoke`
(`producer.rs:99-119`) builds `javadoc -quiet -doclet … <every .java file>` with
**no `-classpath` and no `--module-path` at all** — confirmed by reading the arg
vector. Maven `-sources.jar` packages one artifact's own source and never its
dependencies, so any package referencing a type from another artifact is
structurally unlowerable. That surfaces as a terse generic `OracleExit` carrying
a capped 100-line javac dump, not as the precise variant that already exists.

Worse, in the same enum: **`ProducerError::UnsupportedConstruct` is constructed
exactly once in the entire workspace** — as the catch-all `other =>` arm at
`workspace/compiler/languages/rust/src/lib.rs:168`. Every non-`DependenciesUnresolved`
Rust producer error, including genuine lowering bugs, is relabelled "unsupported
construct" on the way out. That is doctrine §8's `map_err(|_|)` shape wearing a
different hat, in the crate whose own error enum documents the variant as carrying
"the canonical path of the offending symbol."

### CC-4 — "Success" is indistinguishable from "produced nothing" in 3 ecosystems

The 26 entries from the headline. Same failure shape as L28 (20/22 fixtures
silently degraded), L38 (88% phantom entries), and L39 (`insert_live` silently
dropping collisions) — the program's most expensive recurring bug, still recurring.

- **Python, 22 entries.** `invoke` returns an empty oracle; `produce()` returns
  `Ok`; the table has 1 root module. No error, no warning, no diagnostic.
- **TypeScript, 4 entries.** CommonJS is invisible to the module graph: `graph.rs`
  follows only `module_record.requested_modules`, which OXC populates from ES
  syntax alone. `require()` is an ordinary call expression and creates zero edges.
  `lodash`'s UMD IIFE → 2 entries; `debug`'s `module.exports = require(…)` branch → 2;
  `ws` → 3, one of which is **actively wrong**: `const WebSocket = require('./lib/websocket')`
  becomes `Const { ty: Any, value: Some("\"require('./lib/websocket')\"") }` — a
  consumer reads a string constant whose value is unevaluated source text.
- **Rust.** Skipped/degraded items during the walk report only via `tracing::warn!`,
  and neither the harness nor the engine installs a subscriber. A non-`Cancelled`
  panic inside one item's lowering costs entries with no signal but a slightly
  lower count. There is no `cost case=`-style structured line for "N items skipped."

**Rust is also the only producer with the cure.** Its `DependenciesUnresolved`
gate fired correctly this sweep on `lazy_static-1.4.0`, refusing to produce a
degraded 20-entry table and naming the real cause (`--all-features` activates the
`spin` optional dep; provisioning validated with plain `cargo metadata --offline`,
a strictly weaker check than the producer's actual policy). That is the gate
working. Six producers have no equivalent.

### CC-5 — No const-expression subsystem: value-level information is dropped by 6 of 7

Every producer that meets a compile-time constant either drops it or stringifies it.

| producer | what is lost |
|---|---|
| Java | **all** annotation arguments — `annotation_attrs()` always sets `AttrTok::arg = None`. Confirmed on real jakarta.validation-api: `@Target`, `@Retention`, `@Constraint` survive as bare token names; `@Size(min=…, max=…)`, `@Pattern(regexp=…)` lose exactly the payload that *is* the API of a bean-validation library |
| C# | attribute ctor args and param defaults as pre-rendered display strings. Self-flagged at `lower.rs:1200`: `// KNOWN IR GAP: Param needs a default: Option<String>` |
| C/C++ | default param values dropped entirely — `OracleParam` has no field. CLI11's `get_subcommand(int index = 0)` loses the `= 0` |
| TypeScript | `Const.value` holds unevaluated source text (see CC-4's `ws` case) |
| Rust | lifetime and const-generic arguments dropped from `Type::Apply.args` — `Foo<'a, N>` keeps only type-shaped args |
| Python | `has_default` → `ParamAttribute::Optional`; the value is gone |

Two independent producers wrote "once the const-expression subsystem exists" in
their own comments. It does not exist, and it has a clear owner: `nudox-ir`.

### CC-6 — `ParamAttribute` is a dumping ground: distinct concepts collapsed onto one variant

Verified against `workspace/ir/model/src/kinds/param.rs:35-63` — the enum has
`Inout, Mutable, Consuming, Borrowing, Isolated, Variadic, Optional, KeywordOnly`.
No `Out`. No positional-only. One `Variadic`.

- **C#** (`lower.rs:1173-1174`): `"ref" => Inout` and `"out" => Inout`. Two
  different caller contracts, one variant. `out` gets an unstructured `"out parameter"`
  breadcrumb appended to its *doc string*; nothing typed distinguishes them.
  Dapper alone has 15 `ref` and 11 `out` params.
- **Python** (`emit/mod.rs:389-390`): `ParamKind::Varargs => Variadic` and
  `ParamKind::Kwargs => Variadic`. **`*args` and `**kwargs` are indistinguishable
  in the IR.** *This was not reported by any of the seven agents — found during
  verification of this synthesis.*
- **Python**, positional-only: previously emitted `Inout` "as closest available",
  actively misreporting a Python param as pass-by-mutable-reference. Fixed this
  sweep to emit nothing — honest, but still unrepresentable.
- **C/C++**: `T&&` lowers to `Primitive::Reference { mutable: true }`, identical to
  a mutable lvalue `T&` (spdlog's move constructor is indistinguishable from a
  mutable-ref overload); `constexpr` collapses onto `FnModifier::Const`, the same
  variant as `__attribute__((const))`.
- **Rust**: higher-ranked binders erased (`for<'a> Fn(&'a T)` renders as `Fn(&T)`);
  lifetime bounds (`'a: 'b`, `T: 'a`, `dyn Trait + 'static`) have no slot at all.

### CC-7 — Source locations are architecturally absent, in 5 of 7 producers

Counting real span-producing calls vs `PathBuf::new()` / `0..0` placeholders:

```
                placeholders   real-span calls
rust                20              11   (free functions only)
typescript          14              17
go                   4               0
java                12               0
csharp              16               0
clang                7               0
python              22               0
```

**Five producers emit no source location for anything.** Rust emits one only for
free functions (`source::fn_source_range`, `item.rs:652`); structs, enums, traits,
impls, consts, statics, aliases, fields, macros and re-exports all get
`source: PathBuf::new(), span: 0..0`.

This is a **root-cause upgrade for L31** ("No per-item source jumping", currently
filed as a GUI-level unknown, "may work and merely be unreachable from the
keyboard"). It cannot work. The GUI has nothing to jump to for ~90% of item kinds
in Rust and for *every* item in five other languages. L31 should be re-diagnosed
as a producer gap, not a GUI one.

### CC-8 — A structural gap the corpus does not yet exercise (recorded so it is not rediscovered)

The Go agent fixed the blank-identifier collision for params and results, then
checked whether the identical shape survives elsewhere and found it does:
`lower_struct` (`lower/types.rs:32,65`) builds a field's id as `member_name: f.name.clone()`
with **no positional fallback at all**. Go permits multiple blank struct fields
(`type T struct { _ [8]byte; X int; _ [8]byte }` — a real padding idiom). The
agent scanned all 22 lowered packages' oracle JSON, found zero instances, and
deliberately did not write an unverified fix. Correct call, and worth preserving:
the first real package with two blank struct fields reproduces CC-1 exactly.

---

## What the performance data supports — and what it does not

Source: 166 `cost case=` lines from all seven sweeps, parsed with
`.config/scripts/perf-report.nu` via the `nu --stdin` form (the piping form
silently yields zero rows). Total measured wall: **1,649.6 s**.

| ecosystem | rows | total (s) | mean (s) | median (s) | min (s) | max (s) |
|---|---:|---:|---:|---:|---:|---:|
| rust | 23 | 988.9 | 43.00 | 38.60 | 22.85 | 98.0 |
| cpp | 21 | 573.3 | 27.30 | 18.97 | 0.65 | 146.1 |
| csharp | 24 | 39.3 | 1.64 | 1.30 | 0.60 | 4.7 |
| java | 31 | 33.6 | 1.08 | 0.95 | 0.29 | 5.4 |
| go | 23 | 11.9 | 0.52 | 0.38 | 0.20 | 2.3 |
| npm | 22 | 2.5 | 0.11 | 0.03 | 0.001 | 0.9 |
| python | 22 | 0.0 | 0.00 | 0.00 | 0.000 | 0.0 |

### These numbers do NOT support

**Seven agents built and tested concurrently on one `target/` and one build lock.**
Doctrine §8 records a **4.6× swing on identical work** under exactly this
condition, and the agents observed it directly this sweep (hashbrown: ~20 s idle
vs the 98.0 s recorded here; C++ `argparse` 39.9 s vs the similarly-sized
`cxxopts` 8.0 s — pure contention noise).

Specifically, do not quote this data for:

- **Any absolute timing.** Every wall figure is an upper bound of unknown tightness.
- **Any cross-ecosystem comparison at small ratios.** Java's 1.08 s mean vs C#'s
  1.64 s is inside the noise floor and means nothing.
- **Any before/after or optimization claim.** No baseline was taken on a quiet
  machine; `perf-report.nu --baseline` was not used because no comparable
  baseline exists.
- **Comparison against L1 / `CORPUS-REPORT.md`.** Entirely different machine state.
- **Per-package RSS.** *This is a data-shape defect, not just noise, and it is not
  flagged in any of the seven reports.* `rss_bytes` is a **monotonic process
  high-water mark**, not a per-package attribution: nine consecutive Go rows
  (go-multierror through zap) all read exactly `441729024`, npm plateaus at
  `138543104` for its last four rows, and python at `3932160`. Reading
  "go-redis uses 421 MB" from this table is wrong — 421 MB is where the *test
  binary* peaked, and every subsequent package inherits it. Only the *first* peak
  in each sweep is attributable.
- **"Python is fastest."** Python's 0.00 s is the measurement of a stub that
  ignores its input by construction. It is proof of CC-4, not of speed.

### These numbers DO support

Ratios taken **within a single run** are defensible, because load contention
inflates both terms and largely cancels.

**1. L1's central diagnosis survives the dependency re-baseline.** L1's own banner
says its µs/entry column is untrustworthy and must be re-derived with dependencies
resolving. This sweep is that re-derivation — every Rust row ran under
`DependencyResolution::Full`, enforced by a gate that hard-fails otherwise:

| fixture | entries | wall | ms/entry |
|---|---:|---:|---:|
| lazy_static 1.4.0 | 20 | 22.8 s | 1142.3 |
| unicode-width 0.1.11 | 37 | 27.6 s | 746.0 |
| thiserror 1.0.40 | 48 | 34.7 s | 722.5 |
| itoa 1.0.18 | 154 | 35.6 s | 231.4 |
| once_cell 1.20.2 | 332 | 34.9 s | 105.3 |
| serde 1.0.196 | 9,136 | 53.1 s | 5.8 |
| syn 1.0.109 | 15,544 | 80.9 s | 5.2 |
| **libc 0.2.161** | **18,864** | **30.7 s** | **1.6** |

`libc` produces **943× the entries of `lazy_static` in 1.35× the wall time** — a
714× spread in cost-per-entry across one run. The cost is rust-analyzer boot
(`ProjectWorkspace::load` + `run_build_scripts` + `parallel_prime_caches`), paid
once per package; the HIR walk is a rounding error. **L1 stands, now on
dependency-resolved data.**

**2. Architecture, not language, sets the cost — by two orders of magnitude.**
Rust: 79,273 entries in 988.9 s = **80 entries/s**. Go: 105,785 entries in 11.9 s
= **8,890 entries/s**. Go produces *more* entries in 1.2% of the wall time — a
**111× throughput ratio**. Even assuming the worst case that Rust was maximally
penalized and Go maximally favoured by contention (4.6× each way, ~21× compounded),
111× survives comfortably. The difference is architectural: Go runs a purpose-built
oracle subprocess that reads what it needs; Rust boots a full IDE per package.

**3. Two producers own 95% of the corpus wall time.** Rust (988.9 s) + cpp
(573.3 s) = **1,562 s of 1,649.6 s (94.7%)**, from 44 of 166 rows (26.5%). Both are
"load the whole world per package" designs — rust-analyzer workspace load, and
libclang re-parsing each package's full header closure. The other five ecosystems
together cost 87.3 s. Any corpus-wide throughput work should start and probably
end here.

---

## Proposed LIMITATIONS.md entries

House format, for the orchestrator to paste. **Not written to LIMITATIONS.md by
this agent** — that file is the orchestrator's to consolidate. Numbers L44+ assume
L43 is the current maximum (44 entries present at time of writing).

### L44 — Declaration identity is a lossy string projection in 5 of 7 producers

**Blast radius:** every producer, every package. Eleven version entries across five
ecosystems were unlowerable in a single sweep; the defect class scales with every
producer and every new language construct.

**Evidence:** one corpus sweep, five producers, all surfacing through
`LoweringError::Duplicate` (`workspace/ir/model/src/lower.rs:404`):

| producer | discarded distinction | packages |
|---|---|---|
| Go | blank identifier `_` is legal and repeatable; `format!("param:{}")` fell back to a positional index only when `is_empty()` | viper, client_golang, go-redis/v9, go-cmp |
| Java | `type_erase` used a type variable's bare letter, not the erasure of its bound (JLS 4.6) | commons-lang3 (4 `Validate.notEmpty` overloads → 1 id) |
| TypeScript | `TsId` omitted the enclosing `namespace` chain | zod 3.22.4, zod 3.23.8, @types/node |
| TypeScript | star re-export fan-out had no dedup across convergent barrels | date-fns |
| C# | Roslyn docId is unique per *compilation*; the MSBuild-free oracle feeds two mutually exclusive TFM source files into one | System.Text.Json 8.0.2 |
| Rust | `impl_display_name` leaked `<T as Trait>::Assoc` into `Symbol.name`, violating the name index's identifier invariant | rayon 1.9.0 |

Go and TypeScript used *structured* id types (`GoId` enum, `TsId` struct) and
collided anyway, because a string *field* inside the structure dropped the
distinction. Structure at the top level does not buy injectivity underneath.

**Diagnosis:** `Producer::Id` carries no obligation that the id projection be
injective, and nothing type-checks it. Each producer independently invents a
string encoding of a richer language identity. The only thing standing between a
lossy encoding and a silently-wrong table is `Lowering::finish`'s duplicate gate.

That gate caught all six defects on real code that **47 (Go) + 27 (TypeScript) +
20 (C#) hand-authored fixture tests had all passed** — doctrine §4's fixture
parable, reproduced at corpus scale.

**Status:** OPEN as a class. Six of the seven instances were fixed individually
this sweep (Go, Java, both TypeScript, Rust); C#'s is pinned as an expected-failure
regression test. Nothing prevents the seventh.

---

### L45 — 26 corpus entries "succeed" while producing no usable API surface

**Blast radius:** every entry count, the corpus total, and the GUI. A user opening
`lodash` or any Python package sees an empty page and no error.

**Evidence:** 26 of 154 version entries (16.9%) returned `Ok` with a stub or
near-empty table and were counted as successes by their own harnesses:

- **Python, 22 entries, 1 entry each.** `PythonProducer::invoke`
  (`producer.rs:46-59`) discards `src` and returns `PythonOracle::default()`
  without the `pyrefly` feature — the only configuration this workspace can build
  (L43). Identical output for all 22 packages regardless of content.
- **TypeScript, 4 entries.** CommonJS is invisible to the module graph: `graph.rs`
  follows only `module_record.requested_modules`, which OXC populates from ES
  syntax. `require()` creates zero edges. lodash 4.17.21 → 2 entries; lodash
  4.17.20 → 2; debug → 2; ws → 3. `ws` is additionally *wrong*, not merely empty:
  `const WebSocket = require('./lib/websocket')` becomes
  `Const { ty: Any, value: Some("\"require('./lib/websocket')\"") }`.
- **Rust.** Items skipped mid-walk report only via `tracing::warn!`; no subscriber
  is installed by the harness or the engine. There is no structured
  "N items skipped" signal anywhere.

**Diagnosis:** the same failure shape as L28, L38 and L39 — a real loss converted
into an apparent success with nothing in the type system forcing anyone to notice.
The corpus harnesses' own `entry_count > 0` assertions pass on 2-entry stubs,
which doctrine §4 names explicitly ("a test that would pass against a stub is not
a test").

**The cure already exists in one producer.** Rust's `DependenciesUnresolved` gate
fired correctly this sweep on `lazy_static-1.4.0`, refusing a degraded 20-entry
table and naming the cause. Six producers have no equivalent.

**Status:** OPEN. Highest-value next fix — see CORPUS-SWEEP.md.

---

### L46 — `ProducerError`'s typed variants are declared but not constructed

**Blast radius:** every producer failure diagnosis. Turns five-second triage into
an hour, which doctrine §8 already records as a recurring cost.

**Evidence:**

- `ProducerError::DependenciesUnresolved`
  (`workspace/compiler/producer/src/lib.rs:206`) is a **shared** variant whose doc
  says "the resulting table looks complete and is not, which is why it needs a
  variant of its own." **Only the Rust producer constructs it.**
- Java's **17 of 22 failures are exactly this failure mode.** `JavaProducer::invoke`
  (`producer.rs:99-119`) builds `javadoc -quiet -doclet … <every .java file>` with
  **no `-classpath` and no `--module-path`**. Maven `-sources.jar` packages one
  artifact's own source only, so any package referencing another artifact's types
  cannot compile. It surfaces as a generic `OracleExit` with a capped 100-line
  javac dump.
- `ProducerError::UnsupportedConstruct` is constructed **exactly once** in the
  whole workspace — the catch-all `other =>` arm at
  `workspace/compiler/languages/rust/src/lib.rs:168`. Every non-`DependenciesUnresolved`
  Rust error, including genuine lowering bugs, is relabelled "unsupported construct".

**Diagnosis:** doctrine §8's `map_err(|_|)` shape. The typed contract was designed
correctly and then routed around; a variant nobody constructs is documentation,
not a contract.

**Status:** OPEN.

---

### L47 — Source locations are absent from 5 of 7 producers; L31 is a producer gap, not a GUI gap

**Blast radius:** an explicit deliverable — "full hyperlink support and source
jumping". Currently impossible for every item in five languages.

**Evidence:** real span-producing call sites per producer, vs `PathBuf::new()` /
`0..0` placeholders:

```
                placeholders   real-span calls
rust                20              11   (free functions only)
typescript          14              17
go                   4               0
java                12               0
csharp              16               0
clang                7               0
python              22               0
```

Rust emits a real `Symbol.source`/`span` only for free functions
(`source::fn_source_range`, `item.rs:652`). Structs, enums, unions, traits, impls,
consts, statics, type aliases, fields, macros, generic params and re-exports are
all declared with `source: PathBuf::new(), span: 0..0`.

**Diagnosis:** L31 currently reads "It may work and merely be unreachable from the
keyboard, in which case this collapses into L16." It cannot work. The GUI has no
location to jump to for ~90% of Rust item kinds and for *every* item in Go, Java,
C#, C/C++ and Python. This is a producer-side data gap, not a rendering or
input-handling one.

**Status:** OPEN. **Supersedes the diagnosis in L31**; L31 should be re-pointed here.

---

### L48 — No const-expression representation: value-level API information is dropped by 6 of 7 producers

**Blast radius:** the "richness" half of the docs.rs comparison, in every language.

**Evidence:**

| producer | lost |
|---|---|
| Java | **all** annotation arguments — `annotation_attrs()` always sets `AttrTok::arg = None`. On real jakarta.validation-api, `@Target`/`@Retention`/`@Constraint` survive as bare names while `@Size(min,max)` and `@Pattern(regexp)` lose the payload that *is* the API |
| C# | attribute ctor args and param defaults as pre-rendered strings; self-flagged `// KNOWN IR GAP: Param needs a default` at `lower.rs:1200` |
| C/C++ | default param values dropped entirely — `OracleParam` has no field. CLI11 `get_subcommand(int index = 0)` loses `= 0` |
| TypeScript | `Const.value` holds unevaluated source text (`"require('./lib/websocket')"` for ws) — wrong, not merely missing |
| Rust | lifetime and const-generic args dropped from `Type::Apply.args` |
| Python | `has_default` → `ParamAttribute::Optional`; value discarded |

Two producers independently wrote "once the const-expression subsystem exists" in
their own source comments.

**Diagnosis:** `nudox-ir` has no constant-expression representation, so every
producer independently degrades to `None`, a display string, or nothing. A display
string is the worst of the three: it survives into the IR looking structured.

**Status:** OPEN. Owner is `nudox-ir`, not any producer.

---

### L49 — `ParamAttribute` collapses distinct calling conventions onto shared variants

**Blast radius:** every rendered signature in C#, Python and C/C++.

**Evidence:** `workspace/ir/model/src/kinds/param.rs:35-63` declares
`Inout, Mutable, Consuming, Borrowing, Isolated, Variadic, Optional, KeywordOnly`
— no `Out`, no positional-only, one `Variadic`.

- C# `lower.rs:1173-1174`: `"ref" => Inout` **and** `"out" => Inout`. `out` gets an
  unstructured `"out parameter"` breadcrumb appended to its *doc string*; nothing
  typed distinguishes "caller must initialize" from "callee must assign". Dapper
  alone: 15 `ref`, 11 `out`.
- Python `emit/mod.rs:389-390`: `Varargs => Variadic` **and** `Kwargs => Variadic`.
  **`*args` and `**kwargs` are indistinguishable in the IR.** *(Found during
  verification of the sweep synthesis; not reported by any ecosystem agent.)*
- Python positional-only previously emitted `Inout`, actively misreporting a
  Python parameter as pass-by-mutable-reference. Fixed to emit nothing this sweep
  — honest, still unrepresentable.
- C/C++: `T&&` → `Primitive::Reference { mutable: true }`, identical to `T&`
  (spdlog's move ctor is indistinguishable from a mutable-ref overload);
  `constexpr` → `FnModifier::Const`, the same variant as `__attribute__((const))`.
- Rust: HRTB binders erased; lifetime bounds (`'a: 'b`, `T: 'a`, `dyn Trait + 'static`)
  have no slot at all.

**Diagnosis:** producers reach for the "closest available" variant when the exact
one is missing, which reports a *wrong* contract rather than an absent one. An
honest omission is strictly better than a plausible wrong value, and the type
should make the wrong value unrepresentable.

**Status:** OPEN.

---

### L50 — The ledger's own Rust entry counts disagree three ways; the corpus total has no authority

**Blast radius:** every entry count quoted anywhere, including the corpus total the
GUI displays.

**Evidence:** three different figures for the same three fixtures, all in-tree:

| source | memchr 2.7.6 | memchr 2.8.0 | memchr 2.8.3 |
|---|---:|---:|---:|
| L1 banner table | 1,322 | — | 1,325 |
| L38 "true counts" table | 1,344 | 1,347 | 1,347 |
| this sweep (`DependencyResolution::Full`) | **1,825** | **1,835** | **1,835** |

The sweep's figures are internally consistent (a small real inter-generation
delta, no longer L38's flat 11,329-for-all-three defect) and were taken under the
hard `DependenciesUnresolved` gate, so they are not the degraded-measurement
artifact L38 was about. The ~480-entry gap against L38 has not been re-derived.
Relatedly, L1's banner asserts a corpus total of 55,449 while this sweep measured
**79,273 for crates.io alone**.

**Diagnosis:** unknown. Recorded rather than explained away, per doctrine §8:
"suspicious disagreement deserves the same scrutiny as suspicious agreement."
Candidate causes worth eliminating first: a changed `documented_package_names`
set, the L39 `Ref::Foreign` landing (which explicitly "recovered 510 declarations
that previously vanished"), or a different feature/target selection.

**Status:** OPEN. Blocks task #7 (re-baseline the corpus) — no re-baseline is
meaningful until one number is authoritative.

---

## The single highest-value next fix

**Promote the degraded-extraction gate from a Rust-local convention to a
`Producer` trait obligation: every producer's `invoke` must return a typed
completeness verdict, and `produce()` must fail rather than return `Ok` with a
stub table.**

Why this one, over the plausible alternatives:

- **It is the program's most expensive recurring bug, and it is still recurring.**
  L28 (20 of 22 fixtures silently degraded for most of the project, invalidating
  every number taken), L38 (88% phantom entries), L39 (`insert_live` discarding
  collisions silently) — and now L45's 26 entries and L50's three-way number
  disagreement. Same shape every time: a real loss converted into an apparent
  success, with nothing in the type system forcing anyone to notice.
- **The design is already proven in-tree, so this is not speculative work.**
  `ProducerError::DependenciesUnresolved` exists in the *shared* enum with a doc
  comment that describes the Java case almost verbatim. Rust's implementation is
  the template, and it demonstrably works: it caught `lazy_static` this sweep and
  refused to emit a degraded table. The task is to make six producers construct
  what one already constructs, not to invent a mechanism.
- **It converts the 26 invisible failures into visible ones across three
  ecosystems at once**, and it makes Java's 17 failures report the variant that
  already describes them instead of a capped javac dump.
- **It is the precondition for everything else on the list.** Task #7 (re-baseline
  the corpus) cannot produce a trustworthy number while 17% of the corpus reports
  success without content and the ledger disagrees with itself three ways (L50).
  Fixing measurement integrity first makes every subsequent number — including the
  ones used to justify the *next* fix — worth taking.
- **It is cheap relative to its blast radius.** The variant, the doc contract, and
  a working reference implementation all exist today.

Scope it as: add the completeness verdict to the `Producer` trait so the obligation
lands on the *producer definition* with a reason, not on distant call sites —
doctrine §2's worked example, applied. Python's verdict is trivially "stub, no
oracle" and should make its 22 silent successes fail loudly on the first run.

**Runner-up, and why it loses:** Java classpath/module-path resolution would
unblock 17 packages — the largest single package-count win available. It loses on
two counts. It is a substantial new subsystem (POM parsing with parent inheritance,
transitive graph resolution, jar fetching and caching) already scoped as Phase 7 /
Phase 10 work, and — decisively — **nothing about it is being silently
mismeasured.** Those 17 failures are loud, correctly attributed, and safe to leave
until the measurement floor is trustworthy. The 26 silent ones are not.

**Also worth doing early, cheaply:** finish task #12 (emit `ForeignKey` from
TypeScript, clang and Python), which closes L39's explicitly-open half. Go is the
in-tree existence proof that the work is well-defined, and the TypeScript and
clang cases are currently *worse* than `Type::Any` because a consumer cannot tell
them from a resolved type.
