# INDEX-CAPABILITY.md — does the index/search/ingest plane scale, and are its results good?

**Date:** 2026-08-05 · **Branch:** `canonical` · **Author:** synthesis agent
**Question asked:** *Does this system scale, and are its search results actually good?*

---

## 0. The answer, in three sentences

**Storage scales; latency is not the bottleneck; the results are not good.** Catalog
storage settles to ~1,036 bytes per real package across 155 real packages in 7 ecosystems
(re-measured 2026-08-08 on the real DoltLite engine; the previously published ~293 B
figure was taken against a rusqlite fake and described stock SQLite, a storage engine this
product does not ship — see §2.2), the pack layer compresses real source to 13–19% of raw
and is byte-deterministic, and
every measured search over a real crate completed in under 3.1 ms with **zero disk I/O**.
But search *ranking* has no notion of visibility or kind, its ties are broken by
`HashMap` iteration order — so the same query against the same corpus returns a
**different order on every process launch**, proven below across four runs — and the
semantic section is a hardcoded empty array.

The honest summary is that this program has been measuring the wrong risk. The scaling
risk was assumed to be throughput. The measured risk is **correctness of ordering and
absence of a delta representation**, and neither gets better with a faster machine.

---

## 1. Read this before quoting any number

Up to twenty agents were compiling concurrently against one shared 170 GB `target/`
directory while every measurement below was taken. `AGENTS-DOCTRINE.md` §8 records a
**2× swing** on identical real-crate lowering work (21.9 s idle vs 44.6 s under load),
and this program has separately observed a **4.6× swing** on identical work under this
load. `Blocking waiting for file lock on build directory` fired repeatedly during these
runs.

| Class of figure | Trust | Why |
|---|---|---|
| Row counts, op counts, entry counts, hit counts | **Trustworthy** | Deterministic functions of real input; reproduced identically across runs. |
| Catalog / pack / `symbols_proj` byte counts | **Trustworthy** | Reproduced byte-identical across 3+ independent full runs. |
| Result-correctness assertions | **Trustworthy** | Assert on real symbols in real crates, resolved back through the IR — not on `is_ok()`. |
| Git-repo on-disk byte counts | **Indicative ±0.01%** | See §2.4 — these drifted between runs. Do not quote as exact. |
| **All wall-clock / latency figures** | **Indicative at best** | Taken under the load described above. Never quoted as fact below, and no conclusion in this document rests on one. |

Every latency figure in this document is marked. If you find yourself about to paste a
millisecond number into a plan or a customer-facing claim, it is not one of the
trustworthy rows above.

---

## 2. What is now measurable that was not

### 2.1 `workspace/index` compiles

`LIMITATIONS.md` L6 records that the index crate has **never** compiled — 17 errors,
which is precisely why it was excluded from every build gate and stayed broken. It
compiles now. Verified independently for this document:

```
$ RUSTC_BOOTSTRAP=1 cargo check -p index --lib
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.29s

$ RUSTC_BOOTSTRAP=1 cargo test -p index --lib
test result: ok. 731 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

16 of the 17 errors were mechanical (a module file named `lib.rs` instead of `mod.rs`;
an undeclared `zip` dependency; two `format!`-vs-typed-error mismatches; four
`format_args!`-vs-`format!` sites). The 17th was a genuinely missing curated data asset,
`ecosystem/assets/cpp_alias_seed.ron` — now 477 hand-authored rows, **flagged as a
disclosed weakening in §6** because it was authored from knowledge rather than
reconciled against vcpkg/Conan/Repology.

**L6 should be marked RESOLVED.** Three errors that rustc had never type-checked were
hidden behind the unresolved `transport` module gate; one of them is the subject of §4.4.

### 2.2 Catalog storage over 154 real packages

Ingested every entry in `corpus/manifest.toml` — 154 real version entries across all
seven declared ecosystems (crates.io, npm, pypi, go, maven, nuget, cpp), as real
`UpsertPackage`/`UpsertVersion` ops with dependency edges parsed from real `Cargo.toml`
files. Reproduced by this agent; identical to the prior run.

> **Re-measured 2026-08-08 (`bcc320c`). Every figure in the table below moved,
> and the old ones should not be quoted again.** They were measured against
> `MemoryEngine` — a rusqlite-backed fake — because `index` defaulted to
> `test-engine`. So they described **stock SQLite**, a storage engine this
> product does not ship. The real DoltLite engine is now the default and the
> only engine the tests run on.

| Checkpoint | Packages added | Disk delta (was → is) | Bytes/package |
|---|---:|---:|---:|
| migration only (schema v4, 23 tables + 4 indexes) | 0 | 208,896 → **238,172 B** | — |
| **cumulative** | **154 → 155** | **45,056 → 160,602 B** | **292.6 → 1,036.1** |

The catalog is **~3.6× larger** on the real engine, which is the expected shape
rather than a regression: a versioned, content-addressed store keeps history that
a plain SQLite file does not.

**The four leading 0-byte checkpoints are gone.** That was a SQLite artifact —
whole 4,096-byte page allocation meant up to 30 real packages fit inside pages
the migration had already claimed, so the marginal cost was a step function and
"bytes per package" was meaningless below the floor. On the real engine the
shape inverts: per-package cost *falls* as the catalog grows, because chunks are
shared. That is a genuinely different scaling story, and it is the product's.

The correctness checks are unchanged and still hold: `memchr`'s row returns
`name_canonical == "memchr"`, and memchr 2.8.3's edges round-trip to exactly
`["core", "log"]`, independently checkable in `.real-crates/memchr-2.8.3/Cargo.toml`.

(The 154 → 155 package count is unrelated — `corpus/manifest.toml` gained
`org.reactivestreams:reactive-streams` on the Java track, `5683571`.)

### 2.3 Projection and pack layers

| Layer | Real input | Rows / members | Bytes (was → is) | Rate |
|---|---|---:|---:|---:|
| `symbols_proj` | memchr 2.8.3 real `src/` | 180 stored (273 scanned) | 40,960 → **1,481,075** | 227.6 → **8,228.2** B/row |
| `symbols_proj` | syn 1.0.109 real `src/` | 975 stored (1,030 scanned) | 200,704 → **10,628,633** | 205.9 → **10,901.2** B/row |
| NDPK pack | memchr 2.8.3 `src/` | 45 members | 106,536 **(unchanged)** (raw 566,996) | **18.8%** of raw |
| NDPK pack | syn 1.0.109 `src/` | 52 members | 187,515 **(unchanged)** (raw 1,457,008) | **12.9%** of raw |
| NDPK determinism | syn, re-ingested, independent store | 52 | 187,515 — **identical `ObjectPackId`** | — |

**The NDPK rows staying byte-identical is the load-bearing part of this table.**
The pack layer never touches the catalog engine, so if its numbers had moved
alongside the projection numbers, the whole re-measurement would have been
measurement drift rather than the engine swap. They did not move, so the
projection change is attributable.

The projection layer is ~36× larger per row on the real engine. That is a real
cost and should be read as one, not explained away: a versioned store retains
per-row history. Whether that is acceptable at corpus scale is an open question
this document does not answer.

Compression ratio is **not** a constant: syn's raw source is 2.57× memchr's but its pack
is only 1.76× memchr's. Repetitive parser code compresses better. A test that asserted a
fixed >2× pack-scaling ratio failed against real data and was corrected to assert
monotonic growth — the right call, and exactly doctrine §4's point in miniature.

The scan/store gap (273→180, 1,030→975) is **not loss** — it is a real moniker collision
documented in §4.3.

### 2.4 A caveat this document is obliged to raise

The git-repo on-disk byte counts did **not** reproduce exactly. Prior run vs mine:

| n real tags | prior run | this run | delta |
|---:|---:|---:|---:|
| 1 | 29,027 | 29,027 | 0 |
| 8 | 33,541 | 33,539 | −2 |
| 32 | 51,179 | 51,175 | −4 |

Real git object/pack bytes vary slightly with commit timestamps and zlib framing. The
catalog, pack and `symbols_proj` byte counts reproduced **byte-identical**; the git-repo
ones are ±0.01%. Report the former as exact and the latter as approximate. *(This is the
kind of thing that gets quoted back as a regression six weeks later.)*

---

## 3. Are the search results actually good? No — and the reason is architectural

This is the part of the answer that matters, and it is the part nobody could reach until
the crate compiled.

### 3.1 Latency is not the problem (indicative figures, per §1)

Real corpus: memchr-2.8.3 + log-0.4.33 + itoa-1.0.18, lowered through the real Rust
producer. Every figure in this table is **indicative only**.

| Query | Corpus | Hits | Outer wall | Engine-reported | `disk_delta_bytes` |
|---|---|---:|---:|---:|---:|
| name `"memchr"` | 1 pkg | 22 | 203 µs | 155.3 µs | **0** |
| prefix `"mem"` | 1 pkg | 35 | 303 µs | 253.3 µs | **0** |
| kind keyword `"fn"` | 1 pkg | 432 | 3.05 ms | 2.73 ms | **0** |
| name `"memchr"` | 3 pkg | 22 | 187 µs | 150.5 µs | **0** |
| cold (first) vs warm (mean of 4) | 3 pkg | 22 | 217.7 µs vs 217.0 µs | — | **0** |

The one **trustworthy** row here is the last column: `disk_delta_bytes = 0` on every
single measured search. Search is pure in-memory work. Cold and warm are
indistinguishable, which is correct and expected — `PackageIndexes` are built eagerly at
package-load time, so there is no warm-up left for a query to pay.

**What these numbers do not support:** the 3-package corpus measured *faster* than the
1-package corpus. That is the opposite of what `search.rs`'s own fan-out shape predicts
(§4.1). At sub-millisecond scale under this load, scheduler jitter exceeds the signal.
**There is no trustworthy corpus-size scaling measurement in this document.** Getting one
needs tens of packages and a quiet machine.

### 3.2 Ranking is non-deterministic across process launches — proven

Query `"memchr"` against real memchr-2.8.3. Nine entries tie at **score exactly 1.000**.
Four independent process launches, identical input:

| Run | `fn memchr()` (Public Function) | `arch::aarch64::memchr` (Crate-vis Module) | `arch::generic::memchr` (Crate-vis Module) |
|---|---:|---:|---:|
| A | **#6** | #1 | #3 |
| B | **#6** | #3 | #7 |
| C | **#3** | #6 | #2 |
| D | **#3** | #5 | #4 |

**A user who relaunches the app and types the same query sees a different result order.**

Mechanism, traced through the source rather than inferred:

1. `search.rs:401` sorts by `score`, then by `display_name` — which at that moment is the
   **leaf** name (qualification happens later, in `finalize_candidates`). Eight entries
   share leaf name `"memchr"` at score 1.0, so they compare **fully equal**.
2. `sort_by` is a **stable** sort. Fully-equal elements keep their input order.
3. Input order is `PackageIndexes::build`'s single pass over `view.entries()`
   (`nudox-store/src/package.rs:215`).
4. `IrView::entries()` is `self.table.iter()` (`workspace/ir/model/src/view.rs:154`) →
   `PristineIntroTable::iter()` is `self.map.iter()` (`apply.rs:289`) over
   `map: HashMap<IntroId, StoredEntry>` (`apply.rs:107`).

A `std::collections::HashMap` with a per-process randomized seed decides what a user sees
first. This is a **correctness** bug, not a polish item.

### 3.3 Ranking ignores visibility and kind entirely

From a real run — the full ranked set for `"memchr"`:

```
  # 0 score=1.000 kind=Record   vis=Public  name="Memchr"
  # 1 score=1.000 kind=Module   vis=Crate   name="memchr.memchr.arch.aarch64.memchr"
  # 2 score=1.000 kind=Module   vis=Public  name="memchr"
  # 3 score=1.000 kind=Module   vis=Crate   name="memchr.memchr.arch.generic.memchr"
  ...
  # 6 score=1.000 kind=Function vis=Public  name="memchr.memchr.memchr.memchr"
  # 8 score=1.000 kind=Module   vis=Private name="memchr.memchr.memchr"
```

The single most important public symbol in the crate — the function users came to find —
ranks **behind two crate-private implementation modules**, and one *fully private* module
also ties with it at 1.000. `collect_name_hits` scores a case-folded exact match at flat
1.0 with no visibility or kind input at all.

`Memchr` holds #0 in all four runs for an incidental reason: its leaf name differs, and
`'M'` (0x4D) sorts before `'m'` (0x6D) in a plain byte comparison. That accident is the
only thing keeping *something* public at the top.

### 3.4 Three further real defects, all reproduced on a real crate

- **Display names repeat path segments 3–4 times.** Five real names in memchr:
  `memchr.memchr.memchr.memchr`, `…memchr_raw`, `…memchr2_raw`, `…memchr3_raw`,
  `memchr.memchr.memchr`. Tracked as task #14 against `qualified_display_name` — but that
  function is a pure passthrough of `PackageIndexes::path_of` whenever the first segment
  equals the package name, which for memchr is always. **The defect is upstream in
  `workspace/ir/model`'s ancestor-chain construction, not in `nudox-engine`.** Task #14 is
  pointed at the wrong file.
- **Re-export aliases are structurally unreachable.** `pub use crate::memchr::{memchr, …}`
  produces a crate-root alias entry (`EntryInner::Reference`) alongside the physical
  definition. `collect_name_hits` filters on `kind().discriminant()`, which is `None` for
  a `Reference`. Verified: exactly 1 such alias exists for `"memchr"`, and **0** are
  reachable by search. This is *why* §3.3's public function displays its ugly private path
  — the clean public one is invisible by construction.
- **Semantic search returns a hardcoded empty array.** `search.rs:225–230` is literally
  `rows: Arc::from([] as [HitRow; 0])`, and `SearchQuery` still has no `mode` field
  (`{ text, kinds, limit }`). L41 remains accurate and open.

### 3.5 There is no relevance measurement anywhere in this repository

Not weak measurement — **none**. No held-out query set, no graded relevance judgments, no
precision/recall/nDCG harness, for either the local symbol search or the remote vector
plane. What exists is self-consistency: tests asserting that a hand-written scoring
formula orders results the way that same formula's author intended. That cannot detect
§3.2 or §3.3, and did not.

---

## 4. Cross-cutting findings

*A bug in one place is a bug. The same bug in three places is an architectural finding.*

### 4.1 ⚠️ No delta representation in any hot path — cost tracks accumulated state, not change

Three independent sites, all verified in source and two of them measured:

| Site | Behaviour | Evidence |
|---|---|---|
| `ingest/enumerate.rs:108` `enumerate_git_versions` | Re-lists the **entire** ref set on every poll. No cursor, no delta concept — the function has no parameter for one. | **Measured:** 5 existing tags + 1 new = **6** `UpsertVersion` ops, not 1. |
| `store/apply.rs:109` `emit_outbox_row` | Fires **unconditionally** per `UpsertVersion`, with no content-change check. `ON CONFLICT DO UPDATE` still notifies. | **Measured:** `applied=20` → `outbox_rows=20` on a pure re-apply of unchanged content. |
| `search.rs:158,349,452` `run_search` | `corpus.packages()` clones every package `Arc`, then loops all P packages **twice**. No corpus-wide merged index — only P independent ones queried in sequence. | Source-verified. Per-query cost is O(P × per-package cost). |

**This is the real scalability answer.** A repo with 300 releases re-emits 300 upsert ops
and 300 outbox notifications to every downstream sink — the text index, the vector
projection, everything — every single poll, forever, because one tag was added. It gets
monotonically worse as any monitored stem accumulates history, and it fans out.

The individual fixes are all local. The finding is that **no layer of this system has a
vocabulary for "what changed"** — and that is a design gap, not three bugs.

### 4.2 ⚠️ `HashMap` iteration order treated as insertion order — including where a comment guarantees the opposite

§3.2 proved the search-ranking case. It is not the only one. `crates/nudox-engine/src/runtime.rs:710–714`, the doc comment on `package_root_key` (line 718):

> *"this takes the first in declaration order so the answer is deterministic even if a
> producer ever emits a second unparented entry (the `IrView` iteration order is the
> insertion order of the sealed table, **not a hash order**)."*

That comment is **false**. The chain `IrView::entries()` → `PristineIntroTable::iter()` →
`HashMap::iter()` is exactly a hash order, verified above. So the function that picks a
package's **root entry** — the top of the documentation tree — would pick a different root
per process launch under precisely the condition the comment claims is safe.

This is doctrine §8's *"a comment describing behaviour is a claim, and claims rot"*, in
its most expensive form: the comment does not merely mislead, it **tells the next reader
not to check**. Note also doctrine §8's rule that suspicious *agreement* deserves scrutiny
— here it is suspicious *reassurance*.

Both sites share one root cause: `PristineIntroTable` exposes an unordered iterator as if
it were ordered. Per doctrine §2, the fix belongs on that type — make `iter()` yield a
defined order (or rename it so callers cannot mistake it) — not on the two call sites.

### 4.3 Moniker collisions silently drop rows via last-write-wins

`symbols_proj`'s `(module_path, identifier)` moniker is not impl-block-qualified. memchr's
`arch/{x86_64,aarch64,wasm32}/**/memchr.rs` each define several SIMD matcher structs, all
carrying real `find`/`new`/`rfind`/`is_available` methods. These collapse onto the same
primary key `(gen_stamp, intro_id)` and **the last upsert wins**: 93 of 273 scan hits
collide for memchr, 55 of 1,030 for syn. Silent data loss in the projection, not an error.

*(Noted honestly: an initial assertion of `stored == scanned` failed against real data and
was corrected to assert the real collision pattern rather than loosened to pass.)*

### 4.4 ⚠️ Folding a standalone crate into a module left stale crate-root paths three times — once resolving silently to the wrong module

| Fold | Residue | Severity |
|---|---|---|
| `transport` → `index::transport` | Content file never renamed `lib.rs`→`mod.rs`, so rustc never parsed the directory. Behind that gate, `announce.rs` used `crate::blob` / `crate::endpoint` / `crate::frame` — correct when `transport` was a crate root. | **`crate::blob` does not error inside `index` — it silently resolves to `index::blob`, a real but entirely unrelated package-manifest module.** This would have compiled and been wrong. |
| `vector` → `registry::vector` | All 16 files under `tests/vector/` still `use vector::…`. No crate named `vector` exists (`grep '^name = "vector"' Cargo.lock` → nothing). | Would fail to compile — but see §4.5, they never get the chance. |
| (planned) `ingestor` | `ingest/main.rs:21–22` and `ingest/mod.rs` reference a crate `ingestor::` that has never existed. The package is `index`, the module is `index::ingest`. | Documentation only, but actively misleading. |

The root `Cargo.toml` and `index/lib.rs:26–30` both describe the first fold in the **past
tense** — *"has been folded"* — while the module could not be parsed at all. The near-miss
is the finding: this refactor is performed by hand, has no mechanical check, and has
already produced one silent mis-resolution to a real-but-wrong module.

### 4.5 ⚠️ Tests outside a build target are indistinguishable from tests that pass

`workspace/registry/tests/vector/` holds 16 substantial adversarial test files —
depshard install/evict, cross-shard fanout, hotset admission, lock/compaction, pack/unpack
round-trips, scheduler/stage/weights, live ONNX embedding. **Zero of them are in any build
target, under either build system.**

- **Cargo** autodiscovers `tests/*.rs` (top level) or `tests/<name>/main.rs`. `find workspace/registry/tests -maxdepth 1 -type f` returns **nothing**. `cargo metadata --no-deps` lists exactly one target for `registry`: the lib. `cargo test -p registry --test onnx_live` → *"no test target named `onnx_live`"*.
- **Buck** — `build/rust.bzl:211` is `for src in native.glob(["tests/*.rs"])`. Same top-level-only glob. Zero targets generated.

`workspace/index/tests/object_pack/` had the **identical** defect; it was fixed this
session by explicit `[[test]]` entries in `index/Cargo.toml`. Two crates, one defect, and
in registry's case the files also import a crate that no longer exists — so they have
never run *and* could not.

L41 cites *"`workspace/registry`'s vector plane exists and is heavily tested
(`tests/vector/`, 15 files)"* as evidence the plane is ready. **That citation is
unsupported.** The count is 16, and none have ever executed. L41 needs amending.

### 4.6 Default configurations that are fakes — 3 of 5 closed, 2 stand

> **Re-scoped 2026-08-08.** The original heading was "**Every** measurable
> subsystem's default configuration is a fake". That is no longer true, and a
> row that is 80% true is more dangerous than one that is false, because it
> survives spot-checks. Corrected per-row rather than deleted.

| Subsystem | Default | Status |
|---|---|---|
| Catalog engine | ~~`test-engine`~~ → **`dolt-engine`** | **CLOSED** (`bcc320c`). `default = ["dolt-engine"]`; `engine::Configured` decides once, and the real engine wins even under Cargo feature unification. `test-engine` is opt-in only. 811 tests green on the real engine — up from 808, because `tests/dolt_engine.rs` had never been compiled. |
| Local vector store | `registry`'s `local` (qdrant-edge) | **CLOSED** (`9aa0ae6`). The vendored SIMD kernels and the 817,859-byte tokenizer model are restored; what was on disk had been a 4-byte file containing the ASCII text `STUB` (L7). |
| Semantic search | hardcoded `[]` | **OPEN, and now scoped.** Blocked two ways: no Cargo edge from `nudox-engine` to `registry` and doctrine §1 forbids adding one; and there is no model artifact in the tree (zero `.onnx` files). Decision taken: bridge the GUI to `driver`, which already has a working implementation, rather than build a second embedding pipeline. See LIMITATIONS.md L41. |
| Feed transport | `FixtureTransport` | **STANDS.** Still the only non-test `impl FeedTransport` in the crate. |
| Watermark store | `MemoryWatermarkStore` | **STANDS.** Still the only `impl WatermarkStore` anywhere. |

The compounding argument this section used to make is **dead, and its death is
the interesting part.** It read: `test-engine` is `:memory:`, so
`disk_delta_bytes` is 0 by construction, which is why §2.2 needed a special
`MemoryEngine::open_at_path` to obtain any bytes at all; and `test-engine`'s
`dolt_merge` never conflicts while its `query_rows_at` ignores the commit
reference and returns tip — so the green tests made **no verified claim about
versioning**, which is the catalog's entire reason to exist.

Every clause of that was correct, and all of it is now moot: the tests run on the
real engine, `open_at_path` is gone, and `tests/dolt_engine.rs` exercises a
genuine `dolt_at_<table>(ref)` historical read. Two defects surfaced the moment
the fake stopped being load-bearing, both fixed rather than worked around:
`store_apply.rs`'s AsOf test drove `stage_commit_time` — a hook that existed
**only on the fake**, so the test could never have run against the product — and
then asserted nothing (`let _ =` on all three cases); and `MemoryEngine` stamped
a logical counter into the field `resolve_as_of_time` compares against unix
milliseconds, two units in one field, so any real-instant query matched every
commit.

Doctrine §8 is explicit that `cargo check` never links, and `rusqdoltlite`'s `build.rs`
detects the missing amalgamation and **returns early with a `cargo:warning`** — a build
script that returns early instead of building something, which doctrine §6 lists verbatim
as a weakening. It is a deliberate, documented degrade, but its consequence is that
`cargo check` passes and any binary that links it dies at load with undefined symbols.

---

## 5. Measured vs inferred — the explicit boundary

| Claim | Status |
|---|---|
| `index` compiles; 731 lib tests pass | **Measured** — commands and output in §2.1 |
| Catalog ≈ 1,036.1 B/package at 155 real packages | **Measured** on the real DoltLite engine (bcc320c). The superseded 292.6 figure was measured against a rusqlite fake and described stock SQLite, not this product. |
| Pack ratio 12.9–18.8%, deterministic `ObjectPackId` | **Measured** — reproduced byte-identical |
| `symbols_proj` 8,228–10,901 B/row | **Measured** on the real engine, but only 2 data points — weaker than the package figure. Superseded 205.9–227.6 was the fake. |
| postcard wire cost 97–98.4 B/version, linear | **Measured** at n = 1/8/32 |
| Ranking is non-deterministic across launches | **Measured** — 4 runs, §3.2 |
| Public API outranked by private modules | **Measured** — full ranked set, §3.3 |
| `disk_delta_bytes = 0` on every search | **Measured** |
| Search latency figures | **Indicative only** — see §1 |
| Corpus-size scaling of latency | **NOT MEASURED.** The 1-vs-3-package result is noise. |
| Re-enumeration re-lists everything at any N | **Inferred, but safely** — `enumerate_git_versions` has no cursor parameter; true by construction, not scale-dependent |
| Catalog storage under the **real** DoltLite engine | **NOT MEASURED, unmeasurable here** — different storage engine entirely |
| Versioning correctness (merge conflicts, historical reads) | **NOT MEASURED, unmeasurable here** — §4.6 |
| Behaviour beyond 154 packages / 32 tags / 3 loaded packages | **NOT MEASURED.** Flat-per-package above 60→154 is suggestive, not asymptotic. |
| Vector plane correctness | **NOT MEASURED** — §4.5, and 2 of registry's 170 tests fail today |
| `driver` (the real long-running server) | **NOT MEASURED** — 115 errors, blocked by `ir-vcs` (`cannot find function pijul_err`), out of scope |

### Two live regressions found in passing, not caused by this work

1. **memchr lowers to 1,835 entries.** L28 and `real_memchr_generic_return.rs`'s
   hard-coded baseline both record **11,361** with dependencies resolving. I verified the
   fixture is *not* in L28's degraded state — `cargo metadata --offline` in
   `.real-crates/memchr-2.8.3` resolves 8 packages with a `resolve` section present. So
   this is a **new ~84% collapse**, almost certainly from the in-flight edits to
   `workspace/compiler/languages/rust/src/ra/*` and `workspace/ir/model/src/*` visible in
   `git status` (both off-limits to me). **No current entry-count baseline should be
   trusted until this is diagnosed** — including task #7's re-baseline.
2. **`cargo test -p registry` → 168 passed, 2 failed** —
   `vector::core::store::tests::payload_value_ordering_stability_btreemap_determinism`
   (`store.rs:298`) and `vector::remote::voyage::tests::token_cap_exact_boundary_math_1188_fits`
   (`voyage.rs:476`). Latent bugs exposed by registry becoming compilable; both in
   `vector::*`, unrelated to the `Ref::Foreign` fix that unblocked it.

---

## 6. Disclosed weakenings carried forward

Reproduced here so they are not lost between reports. Per doctrine §6, hiding one is the
single unforgivable failure.

- **`workspace/index/ecosystem/assets/cpp_alias_seed.ron` (477 entries) was authored by
  hand from model knowledge.** It was **not** bulk-imported from or reconciled against
  vcpkg's port index, Conan Center, Homebrew's API, or Repology. This is doctrine §6's
  *"a fabricated fixture, asset, or default standing in for real data."* Mitigations: the
  asset is *by definition* hand-curated (`AliasConfidence::Curated`), so curating it is the
  work rather than a fake; every row is an independently checkable factual claim, not an
  opaque blob; the file's header documents provenance, names 5 rows most likely stale from
  upstream host moves (xz, libpng, bzip2, OpenBLAS, oneTBB), and lists deliberate
  omissions (GSL, Curses, Readline, GMP, Qt) rather than guessing. **It needs review before
  it is used for real resolution.** It currently has **zero non-test consumers** —
  `load_seed()` is called only by its own four tests (verified) — so nothing today acts on
  a wrong row. The four guarding tests were not weakened.
- **Scope:** `workspace/registry/graph/reverse_index.rs` and the root `.gitignore` were
  edited from outside their owning task. Both were load-bearing (registry was broken, so
  nothing downstream was verifiable; and the root `/**/*` ignore rule meant the new `.ron`
  would never have been committed, making the L6 fix real only on one disk). Disclosed
  rather than filed as ordinary work.
- **Two nudox-engine tests are committed deliberately red**
  (`memchr_function_specifically_outranks_internal_module_collision`,
  `memchr_query_display_names_do_not_triple_repeat_a_path_segment`). They are guards for
  §3.2/§3.4, `#[ignore]`-gated for the same real-checkout reason as every other real-crate
  test in that crate — not to hide the failure. Neither assertion was softened after it
  failed.

Explicitly **not** weakened: no test narrowed, skipped, deleted or stubbed; no assertion
loosened; no timeout raised; no `#[allow]`/`unwrap`/wildcard arm added to dodge a
diagnostic. Two assertions were made **stricter** (`is_err()` → typed
`PackError::ProviderRefused` match; panic-absence → "zero strict prefixes may open"). The
`dolt-engine` feature remains off.

---

## 7. Proposed `LIMITATIONS.md` entries

*House format. **Not** written to the file — the orchestrator consolidates.*

---

### L6 — The `index` crate has never compiled → **RESOLVED, 2026-08-05**

**Status:** RESOLVED. 16 of 17 errors were mechanical: `transport/lib.rs` renamed to
`transport/mod.rs`; `zip` added to root `[workspace.dependencies]` (`index` and `driver`
now share one version, deliberately — both parse untrusted archives, so a version skew
would land as a security difference); `PackError::TocEncode` split from `TocDecode` so
each carries its typed `postcard::Error`; four `format_args!` sites replaced by typed
error variants (`FrameDecode { offset, source }`, `FrameLengthMismatch { offset, expected,
actual }`, `ProviderRefused { reason }`) rather than by `format!`. The 17th was the missing
`ecosystem/assets/cpp_alias_seed.ron`, now curated — **see the disclosed weakening in
`INDEX-CAPABILITY.md` §6.** Verified: `cargo check -p index --lib` clean;
`cargo test -p index --lib` → 731 passed, 0 failed. Three further errors were revealed
behind the transport gate; one is now **L46**.

---

### L43 — No layer of the system has a delta representation; cost tracks accumulated state

**Blast radius:** ingest throughput, outbox fan-out to every derived sink (text index,
vector projection), and per-query search cost. Worsens monotonically as any monitored stem
accumulates history — this is the system's principal scaling risk.

**Evidence:** three independent sites. (1) `ingest/enumerate.rs:108`
`enumerate_git_versions` re-lists the entire ref set every poll and has no cursor
parameter — **measured**: a repo with 5 existing tags plus 1 new one yields **6**
`UpsertVersion` ops, not 1. (2) `store/apply.rs:109` calls `emit_outbox_row`
unconditionally per `UpsertVersion` with no content-change check — **measured**:
`applied=20` → `outbox_rows=20` on a pure re-apply of unchanged content. (3)
`nudox-engine/src/search.rs:158,349,452` clones every package `Arc` and loops all P
packages twice; there is no corpus-wide merged index, only P package-local ones queried in
sequence.

**Diagnosis:** not three bugs — one missing abstraction. Nothing in ingest, apply, or
search can express *"what changed."* Individual patches (a ref cursor, a content hash
before notify, a merged name index) would each be local, but the shape recurs because the
vocabulary is absent. Per §2, the fix belongs at the type level: a delta/changeset type
that the enumerate, apply and index paths all consume.

**Status:** OPEN. Site (1) and site (2) measured directly; site (3) source-verified.

---

### L44 — `HashMap` iteration order reaches the user, in one place under a comment guaranteeing it does not

**Blast radius:** search result ordering on every tied query (all four measured runs), and
`package_root_key` — which chooses the root of the documentation tree.

**Evidence:** `PristineIntroTable::iter()` (`workspace/ir/model/src/apply.rs:289`) is
`self.map.iter()` over `map: HashMap<IntroId, StoredEntry>` (`apply.rs:107`), surfaced
unchanged as `IrView::entries()` (`view.rs:154`). `nudox-engine/src/search.rs:401` sorts by
score then by **leaf** name; for query `"memchr"` against real memchr-2.8.3, nine entries
tie at score 1.000 and eight share the leaf name, so `sort_by` (stable) falls through to
input order — i.e. hash order. **Measured across four process launches:** the public
`fn memchr()` ranked #6, #6, #3, #3. Separately,
`crates/nudox-engine/src/runtime.rs:710–714` documents *"the `IrView` iteration order is
the insertion order of the sealed table, not a hash order"* and relies on that claim for
`package_root_key`'s determinism (function at line 718). **The comment is false.**

**Diagnosis:** an unordered iterator exposed as if ordered. Bounding the tie-break in
`search.rs` would fix one symptom and leave `package_root_key` and every future caller
exposed — §2's "fix the abstraction" case exactly. Give `PristineIntroTable::iter()` a
defined order (`IntroId`, or declaration order) or rename it so no caller can mistake it.
The false comment is the more expensive half: it directs the next reader away from real
evidence (§8, *"a comment describing behaviour is a claim"*).

**Status:** OPEN. Root cause is in `workspace/ir/model`, not in `nudox-engine`.

---

### L45 — Search ranking has no visibility or kind input; results are ordered by accident

**Blast radius:** every search in the product. The most-wanted public symbol in a crate is
routinely outranked by crate-private implementation detail.

**Evidence:** `collect_name_hits` scores a case-folded exact match at flat 1.0 regardless
of `Kind` or `Visibility`; prefix hits at `prefix_len/key_len`; type hits at a flat 0.5.
Real run, query `"memchr"`: `fn memchr()` (Public Function) ranks **#6**, behind
`arch::aarch64::memchr` and `arch::generic::memchr` (both `vis=Crate`); a `vis=Private`
module also ties at 1.000. `Memchr` holds #0 only because `'M'` (0x4D) byte-sorts before
`'m'` (0x6D). Compounding: re-export aliases are **structurally unreachable** —
`pub use crate::memchr::memchr` produces an `EntryInner::Reference` whose
`kind().discriminant()` is `None`, which `collect_name_hits` filters out (verified: 1 such
alias exists, 0 reachable), so the clean public path can never be shown and only the long
private one can. And five real display names repeat a segment 3–4× (`memchr.memchr.memchr.memchr`).

**Diagnosis:** there is no relevance model, and — more importantly — **no relevance
measurement anywhere in the repository**: no held-out query set, no graded judgments, no
precision/recall/nDCG. Existing tests assert that a hand-written formula orders results
the way its author intended, which cannot detect this and did not. The display-name
repetition is *not* in `qualified_display_name` (it is a passthrough of
`PackageIndexes::path_of` when the first segment equals the package name, always true for
memchr) — **task #14 is pointed at the wrong file**; the defect is in
`workspace/ir/model`'s ancestor chain.

**Status:** OPEN. Two guard tests committed deliberately red in
`crates/nudox-engine/tests/real_search_benchmarks.rs`.

---

### L46 — Folding a standalone crate into a module has left stale crate-root paths three times, once resolving silently to the wrong module

**Blast radius:** `index::transport` (was unparseable), `registry::vector` (16 dead test
files), `index::ingest` (documentation only). The first case is a near-miss for a silent
wrong-module bind.

**Evidence:** `transport/lib.rs` was never renamed to `mod.rs`, so rustc never type-checked
the five files behind it. Inside that gate `announce.rs` used `crate::blob`,
`crate::endpoint`, `crate::frame` — correct for a crate root. **`crate::blob` does not
error inside `index`: it resolves silently to `index::blob`, a real but unrelated
package-manifest module.** All 16 files under `workspace/registry/tests/vector/` still
`use vector::…`; no crate named `vector` exists (`grep '^name = "vector"' Cargo.lock` →
nothing). `ingest/main.rs:21–22` and `ingest/mod.rs` reference a crate `ingestor::` that
has never existed. The root `Cargo.toml` and `index/lib.rs:26–30` both describe the first
fold in the **past tense** while the module could not be parsed at all.

**Diagnosis:** the refactor is performed by hand with no mechanical check, and the failure
mode is not "it doesn't compile" but "it compiles against the wrong thing." Fixes were
made module-relative (`super::`) rather than re-hardcoded to `index::`, so the module is
correct wherever it is mounted.

**Status:** `index::transport` FIXED. `registry::vector` OPEN (see L47).
`ingest::ingestor` OPEN (docs only).

---

### L47 — 16 adversarial vector tests are in no build target, under either build system

**Blast radius:** all claimed coverage of the local (qdrant-edge) vector plane — depshard
install/evict, cross-shard fanout, hotset admission, lock/compaction, pack/unpack
round-trips, scheduler/stage/weights, live ONNX embedding.

**Evidence:** Cargo autodiscovers `tests/*.rs` at top level only;
`find workspace/registry/tests -maxdepth 1 -type f` returns nothing, and
`cargo metadata --no-deps` lists exactly one `registry` target (the lib).
`cargo test -p registry --test onnx_live` → *"no test target named `onnx_live`"*. Buck's
`rust_tests()` macro (`build/rust.bzl:211`) globs `["tests/*.rs"]` — same top-level-only
glob, zero targets. The files would also fail to compile if wired (L46). `onnx_live.rs`'s
own header prescribes `cargo test -p vector-embed --test onnx_live` — no such package, no
such target. `workspace/index/tests/object_pack/` had the identical defect and was fixed
this session with explicit `[[test]]` entries.

**Diagnosis:** a test outside a build target is indistinguishable from a test that passes,
and both build systems fail the same way, so neither catches the other. **L41 cites these
files as evidence the vector plane is "heavily tested"; that citation is unsupported** —
the count is 16, not 15, and none has ever executed. Related: `cargo test -p registry` now
runs and shows **168 passed, 2 failed** (`payload_value_ordering_stability_btreemap_determinism`
at `vector/core/store.rs:298`; `token_cap_exact_boundary_math_1188_fits` at
`vector/remote/voyage.rs:476`) — latent bugs exposed by registry becoming compilable.

**Status:** OPEN. Fix pattern proven on `index`: explicit `[[test]]` plus rewriting
`use vector::` → `use registry::vector::`.

---

### L48 — Every measurable subsystem's default configuration is a fake, so the 731 green tests make no product claim

**Blast radius:** every storage, versioning, semantic-search and ingest figure this program
has produced or could produce in this checkout.

**Evidence:** `index`'s `default = ["test-engine"]`, documented in-crate as *"NEVER a
product mode: its versioning calls are honest fakes"* — its `dolt_merge` **never
conflicts** (`memory.rs:344`) and its `query_rows_at` **ignores the commit reference**
(`memory.rs:387`, the parameter is `_`-prefixed and unused), so merge-conflict handling and
historical reads are untestable by construction. It is `:memory:`, so `disk_delta_bytes` is
0 by construction — §2.2's catalog figures required adding `MemoryEngine::open_at_path` to
obtain any real bytes. `dolt-engine` cannot be built: `workspace/vendor/doltlite/doltlite.c`
is absent, and `rusqdoltlite`'s `build.rs:39–47` detects this and **returns early with a
`cargo:warning`** — so `cargo check` passes and any binary that links it dies at load with
undefined symbols (§8, *"`cargo check` is not a sufficient gate for anything with C FFI"*).
Likewise: `SECTION_SEMANTIC` is hardcoded `[]`; `FeedTransport` has exactly one impl
(`FixtureTransport`); no durable `WatermarkStore` exists and `feed_watermarks` is touched
by nothing outside `migrations/ddl.rs`, while `git_watermarks` is write-only.

**Diagnosis:** the fakes are honestly labelled in-code — that part is right. The problem is
that "all tests pass" and "the product works" have no overlap here, and nothing in the
build gate expresses the difference. A build-script early return is doctrine §6's own
listed weakening; it is deliberate and documented, but its blast radius (a green
`cargo check` for a binary that cannot load) is exactly what §6 exists to surface.
Measured consequence: a no-op poll has **no representation** in the real `git_watermarks`
table — `TickOutcome::Unchanged` carries zero `CatalogOp`s, so `last_checked_at` stays
frozen and a liveness monitor could never distinguish "checked, nothing new" from "the
poller died."

**Status:** OPEN. Blocked on vendoring (`doltlite.c`, `qdrant-edge/cpp/`), not on code.

---

## 8. The single highest-value next fix

> **Give `PristineIntroTable::iter()` a defined order, and delete the comment in
> `runtime.rs:711` that claims it already has one.**
> `workspace/ir/model/src/apply.rs:289` — sort by `IntroId`, or carry declaration order in
> the sealed table.

**Why this one, ahead of larger and more obviously important work:**

1. **It is a live correctness bug that ships to users today.** Not a missing feature, not
   a perf ceiling. Four process launches, one corpus, one query, four different result
   orders (§3.2). Every other finding in this document describes something that is absent
   or slow; this describes something that is *wrong* while appearing to work.
2. **It is currently defended by a false comment.** `runtime.rs:710–714` states the iteration
   order is insertion order, "not a hash order," and rests `package_root_key`'s determinism
   on that claim. Anyone who investigates ordering weirdness will read that comment and
   stop. Doctrine §8 rates this as the more expensive half of a defect, because it sends
   the next reader somewhere real evidence will not be — and it has already cost this
   program a mis-scoped task (#14 aimed at `qualified_display_name`, where the defect is
   not).
3. **Fixing the type fixes every call site at once, including ones not yet written.**
   Doctrine §2's worked example applied verbatim: bounding the tie-break in `search.rs`
   would fix one symptom and leave `package_root_key` — and `chunk/plan.rs`,
   `chunk/head.rs`, `chunk/sections.rs`, and every future consumer of `entries()` — exposed
   to the identical failure. The bad fix is one call site; the good fix is one method.
4. **It unblocks measurement of everything else.** Ranking quality (L45) cannot be
   improved, benchmarked, or regression-tested while the baseline reorders itself between
   runs. Any relevance harness built on top of a non-deterministic ordering measures noise.
   This fix is a prerequisite for the work that actually makes results good.
5. **It is small, local, and low-risk.** One method in one file, in a crate whose consumers
   are all in-workspace and recompiled together.

**Runner-up, and why it loses:** L43 (no delta representation) is the larger scaling
finding and will eventually matter more — a stem with 300 releases re-notifying every sink
on every poll is the failure this system will actually hit in production. But it is a
design change across three subsystems, it is not yet hurting anyone at current corpus
size, and it can be measured honestly today. L44 cannot be measured around, is wrong right
now, and is defended by a comment telling the next person not to look.

**Not chosen, deliberately:** vendoring `doltlite.c` would unlock the largest *volume* of
unmeasurable surface (L48), but it is an asset-acquisition task rather than an engineering
one, and it does not make a single existing result correct.

---

*Every command in this document was run by its author and its output observed. No figure
was carried over from another report without independent reproduction; where reproduction
disagreed (§2.4), the disagreement is reported rather than reconciled.*
