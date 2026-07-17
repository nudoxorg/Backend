# Exhaustive Plan: Resolve Arbitrary Codebases Against the IR

**Research date:** 2026-07-16  
**Status:** Reconciled plan from [01 internal audit](./01-internal-architecture-audit.md) + [02 prior art](./02-prior-art-resolution.md).  
**Goal:** Take tree-sitter (and, when available, oracle) analysis of *any* consumer codebase and bind occurrences to documentation IR symbols — powering example usage, instances, and cross-package references across Rust, TypeScript, Python, Go, Java, C#, Nix (and future languages).  
**Non-goals:** Implementing in this document; redesigning the whole registry; replacing oracles for library package compilation.

---

## 0. Executive recommendation

**Ship a hybrid confidence ladder; do not bet on stack-graphs or pure tags.**

| Priority | Decision |
|---|---|
| **Baseline (now→near)** | Extend existing `LanguageSpec` + `resolve` ladder with **multi-package SymbolTables** and **CAS-persisted `OccurrenceSet`**. |
| **Precise (when project model exists)** | Optional **oracle-on-consumer** path emitting `Confidence::Oracle` into the same occurrence contract. |
| **Do not build** | Full tree-sitter-stack-graphs port; Kythe ingestion; LSIF moniker graphs. |
| **Product** | Examples panels rank `>= Import` (prefer tests/docs), cluster by AST skeleton, label package versions. |

This matches industrial consensus (GitHub fuzzy+precise, Sourcegraph SCIP+search) while reusing infrastructure already landed in-tree.

```
 Consumer tree (may be broken)
        │
        ▼
 ┌──────────────────┐     optional      ┌─────────────────────┐
 │ LanguageSpec     │                  │ Per-lang oracle      │
 │ extract (TS)     │                  │ on consumer project  │
 └────────┬─────────┘                  └──────────┬──────────┘
          │                                       │
          ▼                                       ▼
 ┌────────────────────────────────────────────────────────────┐
 │ resolve ladder against MergedSymbolTable (deps' Indexes)   │
 │ Confidence: Syntactic < Suffix < Index < Import < Oracle   │
 └────────────────────────────┬───────────────────────────────┘
                              ▼
                     OccurrenceSet (CAS)
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
        Graph edges     Examples ranker    Honesty metrics
        (>= Index)      (>= Import)        (stats buckets)
```

---

## 1. Ground truth (what exists today)

### 1.1 Strengths already paid for

1. **Language-neutral occurrence contract** — `Occurrence { span, target: NudoxPath, kind, role, enclosing, anchored, confidence }` (`workspace/ir/syntax/occurrence.rs`).  
2. **Confidence enum with correct total order** including reserved `Oracle`.  
3. **Per-language `LanguageSpec`** extractors (defs/imports/refs) for all 7 languages.  
4. **Resolution ladder** in `generate/resolve.rs`: self-receiver → root markers → import → lexical → package exact → unique suffix.  
5. **SymbolTable** exact / suffix / by_module over one `Index`.  
6. **Graph projection policy** — only `Role::Reference` with graph-worthy kind and `confidence >= Index` (`from_ir.rs`).  
7. **Honesty meter** — unresolved tallied, not faked as hits.

### 1.2 Hard blockers for the product goal

| # | Blocker | Evidence | Why it blocks "examples of symbol X" |
|---|---|---|---|
| H1 | **Per-package SymbolTable only** | `symtab.rs` builds from one `Index` | Consumer `use serde::Serialize` never joins to serde's IR entry |
| H2 | **OccurrenceSet not in CAS** | `BlobManifest` has `references_ref` (legacy), not occurrences | Cannot serve multi-package usage queries |
| H3 | **No consumer-codebase input path** | `occurrences::build(&PackageInput, …)` assumes registry package | Cannot run on arbitrary repos |
| H4 | **Receiver / overload / generics** | Treesitter emits name chains; no types | Method call examples systematically incomplete without oracle |
| H5 | **`Confidence::Oracle` unused** | No emit site | No precise tier wired |

### 1.3 Soft gaps (solvable on current architecture)

- Glob imports across packages once dep tables exist.  
- Java `java.lang.*` synthetic import.  
- TS multi-hop barrel re-exports.  
- C# `using Namespace` + library index.  
- Go dot-import / embedding (oracle-preferred).  
- Persist + query path for occurrences (replace or complement `references_ref`).  
- Version policy for External joins.

Full inventory: [01 §6–8](./01-internal-architecture-audit.md).

---

## 2. Target architecture

### 2.1 Artifact model (stable)

Keep **one** consumer-facing artifact:

```text
OccurrenceSet  (files[] of Occurrence, + ResolutionStats)
```

Producers:

| Producer | Confidence ceiling | When |
|---|---|---|
| Treesitter + resolve ladder | `Import` / `Index` / `Suffix` / `Syntactic` | Always (dumb tier) |
| Oracle-on-consumer mapper | `Oracle` | Project model available |

Consumers of occurrences **must not** branch on producer type — only on `confidence` and `target` (already designed this way in occurrence.rs docs).

### 2.2 MergedSymbolTable

```text
MergedSymbolTable
  - local: SymbolTable              // consumer package/workspace Index if any
  - deps:  Map<DepCoord, SymbolTable>  // IR Indexes for resolved dependency versions
  - exact_global: optional secondary index for unique cross-dep suffixes (careful!)
```

Resolution changes:

1. Import head → `ImportSource::External { dependency, path }` → look up `deps[dependency].resolve_exact(path++tail)` → if hit, upgrade target from *unbound External path* to **canonical library `NudoxPath`** (Local-in-that-package or whatever IR uses) **or** keep External but with verified path segments matching IR.  
2. Prefer **exact package coordinate from lockfile** when present; else configured "docs default version".  
3. Suffix tier must **not** search all deps by default (collision explosion) — suffix stays local-package only unless uniquely identifiable via import prefix.

### 2.3 Consumer workspace model

Introduce a coordinate-agnostic input (name bikeshed):

```rust
struct CodebaseInput {
    root: PathBuf,
    language: Language,           // or multi-lang workspace
    package_name: String,         // synthetic module root label
    // optional:
    lockfile_deps: Vec<DepPin>,   // name + version + ecosystem
    project_model: Option<ProjectModel>, // cargo/npm/go.mod/… for oracle tier
}
```

`occurrences::build` becomes:

```text
build(codebase: &CodebaseInput, surface: Option<&Index>, deps: &DepIndexes) -> OccurrenceSet
```

Registry package compile remains a special case that fills `surface` from oracle IR.

### 2.4 Persistence

| Field | Content | Action |
|---|---|---|
| `BlobManifest.ir_ref` | Package IR Index | Keep |
| `BlobManifest.references_ref` | Legacy `ResolvedReference` | **Deprecate** after migration |
| **`BlobManifest.occurrences_ref` (new)** | Postcard `OccurrenceSet` + `RESOLVER_VERSION` | **Add** |
| Optional consumer corpora | Not package blobs — separate `UsageCorpus` keyed by (symbol, source_repo, commit) | New serving path for examples |

Serving:

- Library package blob: occurrences *inside the library*.  
- Usage index: inverted index `NudoxPath → [UsageSite]` built offline from consumer corpora (CI, docs crawler, partner repos).

### 2.5 Oracle tier (optional path)

Per language, map oracle xref → `Occurrence`:

| Language | Suggested consumer oracle | Map to |
|---|---|---|
| Rust | rust-analyzer / existing `compile/rust/ra` | `NudoxPath` via same moniker rules as library compile |
| TypeScript | OXC / tsserver project | same |
| Python | Pyrefly / Pyright | same |
| Go | go/types | same |
| Java | ECJ / javac + classpath | same |
| C# | Roslyn workspace | same |
| Nix | best-effort snix / treesitter only | often treesitter-only |

Emit `Confidence::Oracle`. On failure, **fall back** to treesitter ladder (never hard-fail the corpus).

### 2.6 What we explicitly will not do

1. Depend on `github/stack-graphs` (archived).  
2. Author full `.tsg` rulesets for 7 languages as primary resolver.  
3. Require consumer code to fully compile before any examples appear.  
4. Promote `Suffix` matches into graph edges or default example panels.  
5. Silent cross-version joins without labels.

---

## 3. Phased delivery plan

### Phase 0 — Documentation & contracts (0.5–1 day)

- [x] Internal audit recovered & verified → `01-…`  
- [x] Prior art completed → `02-…`  
- [x] This plan → `03-…`  
- [ ] Land a short `REFERENCES` design note in-repo pointing at these (optional).  
- [ ] Freeze: occurrence schema changes require `RESOLVER_VERSION` bump.

### Phase 1 — Multi-package resolution (core correctness)

**Outcome:** consumer file with `import/use` of a known library binds refs to that library's IR paths at `Confidence::Import`.

Work items:

1. `MergedSymbolTable::from(local, deps: impl Iterator<(DepId, &Index)>)`.  
2. Extend `resolve_via_import` External branch to query dep tables.  
3. Unit tests: Rust `use serde::Serialize`, TS `import { readFile } from 'fs'`, Python `from flask import Flask`, Go `import "fmt"`, Java `import java.util.List`, C# `using System.Text`, as fixtures with tiny fake Indexes.  
4. Document version selection API (`DepPin`).

**Exit criteria:** e2e fixture package resolves ≥ N external names to dep IR with `Import` confidence; honesty meter shows residual unresolved.

### Phase 2 — Persistence & serving

**Outcome:** occurrences are queryable after compile.

1. Add `occurrences_ref` to `BlobManifest` (or dual-write with `references_ref`).  
2. Emit path in generate pipeline after `resolve`.  
3. Registry/runtime: load occurrences for `get_references`-class queries without Terminus when cold ([20 graph-over-IR](../librarification/20-graph-over-ir.md) alignment).  
4. Migration: backfill or accept "occurrences missing ⇒ empty usages".

**Exit criteria:** compile package → blob → read back OccurrenceSet round-trip; graph projection identical to today for `>= Index`.

### Phase 3 — Consumer codebase path

**Outcome:** CLI/API can point at an arbitrary repo root + dep set.

1. `CodebaseInput` + walk (reuse `occurrences.rs` skip lists).  
2. Multi-language workspace detection (optional later; start single-lang roots).  
3. Wire lockfile parsers (Cargo.lock, package-lock, go.sum, …) incrementally — **start with explicit dep list JSON** to unblock.  
4. Integration test on a small consumer fixture depending on in-tree IR fixtures.

**Exit criteria:** `resolve-codebase --root examples/demo --deps serde@1.0.x` produces OccurrenceSet with real serde targets.

### Phase 4 — Examples productization

**Outcome:** "Examples" panel quality, not just raw refs.

1. Filter: default `confidence >= Import` (config: `>= Index` for denser).  
2. Prefer paths matching `tests/`, `test/`, `examples/`, `*_test.go`, `*.test.ts`, doc comments.  
3. Cluster by AST skeleton hash (treesitter subtree shape + call arity).  
4. Window extract: imports used + ±K lines around span.  
5. Version labels on each example.  
6. Dedup across consumers.

**Exit criteria:** golden panel snapshots for 3 symbols with human-reviewed quality.

### Phase 5 — Oracle tier (precision)

**Outcome:** when project model exists, method receivers and overloads resolve.

1. Define `OracleOccurrenceProvider` trait → `Vec<Occurrence>` or merge into Extraction.  
2. Implement for 1–2 languages first (Rust RA, TypeScript OXC) using existing compile helpers.  
3. Merge policy: per-span keep max(confidence); never downgrade Oracle to treesitter.  
4. Metrics: delta in `ResolutionStats` buckets on real open-source consumers.

**Exit criteria:** method-call resolution rate uplift on a fixed corpus; no regression on treesitter-only path.

### Phase 6 — Language soft-gap cleanup (ongoing)

Priority order (impact × ease):

1. Java `java.lang.*` synthetic glob.  
2. TS barrel multi-hop (package-local file graph).  
3. Python relative import edge cases.  
4. C# namespace usings against dep tables.  
5. Go embedding / dot-import via oracle only.  
6. Nix `with` — remain best-effort / unresolved.

### Phase 7 — Scale & crawl (optional product)

- Offline crawler of example corpora (curated orgs, not whole internet first).  
- Inverted usage index storage (sqlite/tantivy per [10]/[11](../librarification/)).  
- Rate limits, license filters, PII — product policy.

---

## 4. Per-language effort model (future languages)

To add language L:

| Component | Effort | Required? |
|---|---|---|
| tree-sitter grammar in arborium | Existing / vendor | Yes |
| `LanguageSpec` defs/imports/refs | 1–3 days typical | Yes (dumb tier) |
| Root markers / import fold rules in resolve | Hours–1 day | Yes |
| Oracle library compile (IR) | Already the hard part for docs | Yes for docs |
| Oracle consumer xref mapper | 2–10 days | Optional (precise tier) |
| Stack-graph TSG ruleset | Weeks–months | **No** |

**Bound:** dumb tier always; precise tier when oracle ROI justifies.

---

## 5. Confidence & product policy (normative)

| Confidence | Graph edge | Default examples | UI label |
|---|---|---|---|
| `Oracle` | Yes | Yes (preferred) | Precise |
| `Import` | Yes | Yes | Resolved via import |
| `Index` | Yes | Yes (secondary) | Same package |
| `Suffix` | **No** | Opt-in "approximate" | Ambiguous name |
| `Syntactic` | No | No | Unbound name |

Unresolved: never shown as examples; contribute to honesty dashboards only.

---

## 6. Version skew policy (normative)

1. If consumer lockfile pins `dep@V` and IR for `V` exists → join to `V`.  
2. Else if IR for compatible range exists → join to **chosen** docs version; **label** snippet with both consumer pin (if known) and IR version.  
3. Else keep `NudoxPath::External` unbound; do not invent a target.  
4. Cross-version "same API" continuity uses existing symbol identity work ([05](../librarification/05-symbol-identity-academic.md)/[06](../librarification/06-symbol-identity-industrial.md)/[19](../librarification/19-symbol-moniker-rfc.md)) — **out of band** from occurrence resolve.

---

## 7. Testing strategy

| Layer | What |
|---|---|
| Unit | Ladder steps, MergedSymbolTable, each LanguageSpec fixture |
| Package e2e | Existing `references_e2e.rs` / `treesitter_extraction.rs` extended |
| Consumer e2e | New fixtures: mini apps depending on mini libraries with known IR |
| Oracle e2e | Gated on toolchains; snapshot Occurrence confidence mix |
| Product golden | Examples panel markdown/JSON snapshots |
| Metrics | ResolutionStats histograms per language on corpus |

Never allow tests that treat `Suffix` as success for external APIs.

---

## 8. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Suffix false positives pollute docs | Graph + examples policy exclude Suffix |
| Dep table memory blowup | Lazy load dep Indexes; cache by ContentHash |
| Oracle cost on huge monorepos | Cap files; treesitter-first; oracle on demand for hot packages |
| Version labeling UX debt | Force version field in UsageSite schema from day one |
| Dual `references_ref` / `occurrences_ref` confusion | Single write path; deprecate legacy in one release |
| Scope-graph temptation rewrite | This plan forbids; re-open only with new maintainer evidence |

---

## 9. Mapping to prior-art lessons

| Lesson ([02](./02-prior-art-resolution.md) §9) | Plan response |
|---|---|
| No receivers without types | Phase 5 oracle; Phase 1–4 honest gaps |
| Precise = compiler reuse | Oracle tier, not TSG |
| Always ship dumb tier | Phases 1–3 treesitter path |
| Self-describing join keys | `NudoxPath` + dep coords |
| Avoid archived stack-graphs | Explicit non-goal |
| Store confidence | Already in Occurrence; enforce policies §5 |
| Version skew is product | §6 normative |
| Persist occurrences | Phase 2 |
| Multi-package tables | Phase 1 |
| Examples ≠ find-refs | Phase 4 |
| Locals ≠ resolver | Optional polish only |
| Uniformity via contract | Keep LanguageSpec boundary |
| Two-phase External join | Phase 1 External bind |
| Future language cost model | §4 |
| Honesty > silent wrong links | Stats + policies |

---

## 10. Suggested implementation order (PR slices)

1. **PR-A:** `MergedSymbolTable` + resolve External bind + unit tests.  
2. **PR-B:** `occurrences_ref` blob field + emit/load.  
3. **PR-C:** `CodebaseInput` + CLI/lib entrypoint.  
4. **PR-D:** Usage inverted index MVP (in-process).  
5. **PR-E:** Examples ranker + golden tests.  
6. **PR-F:** Oracle occurrence provider for Rust.  
7. **PR-G:** Oracle for TS/Python as ROI demands.  
8. **PR-H:** Language soft gaps (java.lang, barrels, …).

Each PR must keep `RESOLVER_VERSION` discipline and ResolutionStats compatibility.

---

## 11. Open questions (need product input, not blockers for Phase 1)

1. Which consumer corpora are in-scope for v1 (only package's own tests/examples vs crawled GitHub)?  
2. Default examples confidence floor: `Import` vs `Index`?  
3. Multi-language monorepos: one OccurrenceSet or per-language partitions?  
4. Should unbound External still appear in "possible usages" UI?  
5. Desktop vs server: full dep Indexes on client or query-time server join?

**Recommendation defaults:** (1) package-local tests/examples first, (2) `Import`, (3) per-language partitions under one corpus id, (4) no by default, (5) server-side join for INDEX; desktop loads deps on demand.

---

## 12. Success metrics

| Metric | Target (initial) |
|---|---|
| External import bind rate on fixture corpus | ≥ 80% of import-headed refs at `Import` |
| Method-call bind rate treesitter-only | Track only; no false success |
| Method-call bind rate with oracle (Rust) | ≥ 70% on selected apps |
| Example panel precision (human sample n=50) | ≥ 90% correct symbol |
| p95 resolve time small package | Comparable to current occurrences stage |
| Graph edge false positive rate | No increase vs today (Suffix still excluded) |

---

## 13. Provenance

| Source | Role |
|---|---|
| Claude agent `afaa7790f40fb2b50` | Complete internal audit (verified) |
| Claude agent `af03c686d58e1338b` + angles | Incomplete prior art → finished in `02` |
| Session `17cc2883-6b32-44ea-abc5-c1c9f07d0bf2` | Parent user request |
| Grok recovery 2026-07-16 | Verification, synthesis, this plan |
