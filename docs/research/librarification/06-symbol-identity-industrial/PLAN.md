# Industrial Symbol Identity Systems

**Research date:** 2026-07-16  
**Scope:** How production systems identify symbols, track them across versions, and invalidate dependent indexes.  
**Companion:** Academic survey lives in `05-symbol-identity-academic/` (parallel agent).  
**Purpose:** Ground nudox’s multi-generation IR identity architecture in systems that ship.

**Core question:** *When are two symbols, across package generations, the same symbol?*

---

## 0. Executive framing for nudox

nudox lowers multi-language packages into a shared typed IR. Each published package version is a new **generation**. The pipeline is becoming incremental: only changed symbols re-embed / re-index / re-publish, and TerminusDB (or equivalent lineage store) holds **LINEAGE** edges across generations.

Industrial practice converges on a **layered** answer — never a single primitive:

| Layer | What it answers | Industrial exemplars |
|-------|-----------------|----------------------|
| **Moniker / FQN** | “Same named entity in the API surface?” | SCIP symbols, LSIF monikers, Glass symbol IDs, cargo-semver-checks paths |
| **Content hash** | “Same implementation / signature body?” | Unison AST hashes, Nix fixed-output, git blob OID, Cursor chunk hashes |
| **Structural location** | “Same place in module/file graph?” | Kythe path+signature, r-a ItemLoc, git path + rename detection |
| **Explicit lineage** | “A evolved into B despite moniker/hash mismatch?” | Unison patches, Glean stacked DBs + ownership, git rename edges |
| **Graceful fallback** | “Stale or missing precise ID?” | Sourcegraph search-based nav, BM25 over embeddings at scale |

**Hard lesson across all systems:** moniker equality is **not** continuity. Continuity requires either (1) moniker stability by design, (2) content-address sameness, or (3) an explicit successor edge. Most industrial systems implement (1) poorly for renames and leave (3) to the consumer.

---

## 1. SCIP (Sourcegraph Code Intelligence Protocol)

### 1.1 Role and status

SCIP (“skip”) is Sourcegraph’s language-agnostic index format, introduced to replace LSIF at scale (>45k repos, thousands of daily uploads). Canonical sources:

- Protocol home: [https://scip-code.org/](https://scip-code.org/)
- Announcement / design rationale: [https://sourcegraph.com/blog/announcing-scip](https://sourcegraph.com/blog/announcing-scip)
- Grammar + protobuf: [https://github.com/sourcegraph/scip/blob/main/scip.proto](https://github.com/sourcegraph/scip/blob/main/scip.proto)
- Rust crate **`scip` v0.9.0** (Apache-2.0, MSRV 1.81): [https://crates.io/crates/scip](https://crates.io/crates/scip) (published ~2026-06-29; docs [https://docs.rs/scip/0.9.0](https://docs.rs/scip/0.9.0))

### 1.2 Exact symbol grammar

From `scip.proto` (BNF as documented in the schema comments):

```
<symbol>              ::= <scheme> ' ' <package> ' ' (<descriptor>)+ | 'local ' <local-id>
<package>             ::= <manager> ' ' <package-name> ' ' <version>
<scheme>              ::= UTF-8 (spaces escaped as double space); non-empty; MUST NOT start with "local"
<manager>             ::= UTF-8 or '.' for empty
<package-name>        ::= same as manager
<version>             ::= same as manager
<descriptor>          ::= <namespace> | <type> | <term> | <method>
                        | <type-parameter> | <parameter> | <meta> | <macro>
<namespace>           ::= <name> '/'
<type>                ::= <name> '#'
<term>                ::= <name> '.'
<meta>                ::= <name> ':'
<macro>               ::= <name> '!'
<method>              ::= <name> '(' (<method-disambiguator>)? ').'
<type-parameter>      ::= '[' <name> ']'
<parameter>           ::= '(' <name> ')'
```

Descriptor suffix table:

| Suffix | Token | Meaning |
|--------|-------|---------|
| Namespace | `/` | package / module / namespace |
| Type | `#` | class / interface / enum / struct |
| Term | `.` | function, value, field |
| Method | `().` | method; optional disambiguator for overloads |
| TypeParameter | `[name]` | generic param |
| Parameter | `(name)` | formal parameter |
| Meta | `:` | metadata |
| Macro | `!` | macro |

**Invariant:** the descriptor chain is a fully qualified name *within* the package triple. Local symbols (`local <id>`) are document-scoped and must not be referenced outside their Document.

### 1.3 Concrete examples

| Ecosystem | Example symbol string |
|-----------|----------------------|
| TypeScript / npm | `scip-typescript npm @sourcegraph/scip-typescript 0.2.0 src/FileIndexer.ts/scriptElementKind().` |
| Rust / cargo | `rust-analyzer cargo ra-test 0.1.0 Point#` |
| Java / maven | `scip-java maven org.slf4j/slf4j-api 1.7.36 com/example/MyClass#myMethod().` |
| Go / gomod | `scip-go gomod example.com/api v1.2.3 Handler().` |

Placeholder `.` is used when manager/name/version is unknown (e.g. unpublished local crates).

### 1.4 Stability across versions

**SCIP symbols are version-pinned by design.** The package triple `(manager, name, version)` is embedded in every global symbol string. Consequences:

1. `lib@v1.0.0` and `lib@v2.0.0` produce **different** symbol strings even for an unchanged definition.
2. There is **no protocol-level cross-version alias** (“this moniker continues that moniker”).
3. Cross-repo navigation works by matching exact symbol strings at pinned dependency versions — consumers navigate to the **dependency’s indexed version**, not “latest.”
4. Go bindings expose `SymbolFormatter` helpers that can strip version for *ad hoc* version-agnostic matching ([https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip](https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip)) — this is a consumer convenience, not protocol semantics.
5. Cross-repo design notes (scip-clang): [https://github.com/sourcegraph/scip-clang/blob/main/docs/CrossRepo.md](https://github.com/sourcegraph/scip-clang/blob/main/docs/CrossRepo.md)

**For nudox:** SCIP monikers are excellent **tier-0 coordinates within a generation**. They are a poor sole answer for “same symbol across generations” unless you define a **version-stripped moniker** (drop package version; keep scheme/manager/name/descriptors) as the continuity key.

### 1.5 Stale indexes and cross-commit navigation

Sourcegraph’s production strategy ([precise code navigation docs](https://sourcegraph.com/docs/code_intelligence/explanations/precise_code_navigation), [cross-repo blog](https://sourcegraph.com/blog/cross-repository-code-navigation)):

1. Prefer precise SCIP for the nearest indexed commit.
2. Between indexed commits, or when a line was created/edited since the nearest index, **degrade to search-based navigation**.
3. Cross-repo gaps (A indexed, dependency B missing or different version) degrade on the missing side.
4. SCIP’s document-centric string IDs enable **incremental re-indexing of changed files** — unlike LSIF’s globally incrementing vertex IDs — shrinking the staleness window.

### 1.6 Moniker design lessons (why SCIP killed LSIF monikers)

Sourcegraph’s critique ([announcing SCIP](https://sourcegraph.com/blog/announcing-scip)):

1. **No static schema** — LSIF is a dynamic graph; validation and tooling errors dominate.
2. **Opaque global IDs** — numeric vertex IDs cannot be inspected, merged across subprojects, or partially updated.
3. **Incremental indexing impossible** — global ID ordering forces full regeneration.
4. **Import/export moniker fragility** — silent navigation breakage if export vs import classification is wrong (especially C++ with no explicit export, shared headers, templates).
5. **Performance** — large in-memory graphs for both write and read.

SCIP’s replacement insight: **self-describing human-readable strings** (SemanticDB-inspired) replace the moniker graph. Export/import status need not be decided at index time for linking; the string itself is the join key.

---

## 2. LSIF moniker system (historical substrate)

### 2.1 Graph model

Specs: [LSIF 0.4.0](https://microsoft.github.io/language-server-protocol/specifications/lsif/0.4.0/specification/), [LSIF 0.5.0](https://microsoft.github.io/language-server-protocol/specifications/lsif/0.5.0/specification/).

- **Moniker vertex**: `kind` ∈ {export, import, local}, `scheme` (e.g. `tsc`, `npm`), `identifier` (e.g. `lib/index:Emitter.emit`)
- **PackageInformation vertex**: `name`, `manager`, `version`, optional `repository`
- **`packageInformation` edge**: moniker → package
- **`nextMoniker` (0.4)** / **`attach` (0.5)**: chain tool moniker → package-manager moniker
- **`unique` (0.5)**: uniqueness scope ∈ {document, project, group, scheme, global}

External moniker workflow:

1. Compiler emits tool moniker (`tsc` scheme).
2. Post-process to package-manager moniker (`npm` scheme).
3. Attach packageInformation with version.
4. Cross-repo join = exact scheme+identifier match between import and export.

### 2.2 Failure modes still relevant to nudox

- **Exact-match join with silent failure** — any format drift between indexers breaks navigation without errors ([lsif-node#44](https://github.com/microsoft/lsif-node/issues/44) on C++ export/import).
- **Directional moniker encoding was confusing** — 0.5 abandoned direction for `unique` + `attach`.
- **Version still pinned** — no better cross-generation story than SCIP.

**Takeaway:** keep SCIP’s self-describing string model; do **not** reintroduce LSIF’s multi-vertex moniker graph.

---

## 3. Kythe VName design

### 3.1 VName 5-tuple

Sources: [Kythe URI spec](https://kythe.io/docs/kythe-uri-spec.html), [schema](https://kythe.io/docs/schema/), [writing an indexer](https://kythe.io/docs/schema/writing-an-indexer.html).

| Field | Role | Stability notes |
|-------|------|-----------------|
| **corpus** | repo/project identity (hostname-like) | deployment-defined |
| **root** | subdivision (branch, build variant); often empty | variable |
| **path** | file path relative to corpus+root | file-level stable anchor |
| **language** | `c++`, `java`, `go`, … | stable |
| **signature** | entity-unique string within (corpus,root,path,language) | **consistent within one indexer run; not required stable across input versions** |

URI form: `kythe:[corpus]?[lang=…][path=…][root=…]#[signature]` with fixed attribute order, percent-escaping, NFKC.

Corpus semantics are intentionally **not** specified — each deployment maps paths → corpus/root via `vnames.json`.

### 3.2 Cross-version continuity

Kythe has **no built-in supersedes / generates / version-of** edge type for entity lineage. Spec language is explicit: signatures need not be stable across different versions of the input. File nodes are called out as more stable linking points across discrete indexer runs.

Pragmatic continuity = re-index same inputs → same VNames (determinism). Code changes → new signatures with **no protocol link** to old ones. Version encoding (commit SHA in corpus/root) is a **deployment convention**, not a schema feature.

### 3.3 Development state (verified 2026-07-16)

**Actively maintained, not dormant.**

- Latest release: **v0.0.75** (2026-03-10 / 2026-03-12) — [GitHub Releases](https://github.com/kythe/kythe/releases), [RELEASES.md](https://github.com/kythe/kythe/blob/master/RELEASES.md), [pkg.go.dev](https://pkg.go.dev/kythe.io)
- Prior: v0.0.74 (2025-11), v0.0.73 (2025-08) — ~2–3 month cadence
- Recent work includes Rust extractors, Go/C++/Java tooling
- Still Google-origin OSS; not a consumer-facing code nav product like SCIP+Sourcegraph

**For nudox:** adopt Kythe’s **path as stable file anchor** + **language field** ideas; do **not** adopt VNames as primary monikers (signature instability across versions is the wrong default for generation lineage). Avoid depending on Kythe protos as a runtime dependency unless already needed — prefer SCIP-shaped strings.

---

## 4. Unison: content-addressed definitions and patches

### 4.1 Hash = identity

Sources: [The Big Idea](https://www.unison-lang.org/docs/the-big-idea/), [Hashes reference](https://www.unison-lang.org/docs/language-reference/hashes/), [FAQ](https://www.unison-lang.org/docs/faq/), [SoftwareMill deep dive](https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/).

- Every definition identified by a **512-bit SHA3** digest of its **normalized AST**.
- Normalization:
  1. Names erased → positional / de Bruijn indices.
  2. Dependencies replaced by **their hashes** (Merkle over transitive closure).
  3. Whitespace/comments excluded.
- Same logic, different names → **same hash**. Same name, different body → **new hash**.
- Codebase is an **append-only content-addressed store**. Definitions are never overwritten.

Unison 1.0 announced Nov 2025 ([InfoWorld](https://www.infoworld.com/article/4100673/futuristic-unison-functional-language-debuts.html)); releases continue (1.1.0 Jan 2026; 1.3.0 May 2026 per GitHub).

### 4.2 Names as metadata

Names live in a **separate namespace layer** (name → hash). Consequences:

- Rename is free / non-breaking at the hash layer.
- Aliases (many names → one hash) are natural.
- “Updating” a function rebinds the name to a new hash; old hash remains resolvable forever.

### 4.3 Patch / update model = first-class lineage

From [update workflow](https://www.unison-lang.org/docs/usage-topics/workflow-how-tos/update-code/) and [reducing churn](https://www.unison-lang.org/blog/reducing-churn/):

1. Edit produces new hash H2; old H1 remains.
2. Name rebinding + **patch** = set of **hash→hash replacements**.
3. Type-preserving edits: automatic propagation through dependents.
4. Type-breaking edits: open scratch with unresolved dependents; human fixes; resume propagation.
5. Library authors **publish patches** with releases; consumers apply them.

**This is the most sophisticated industrial continuity model surveyed.** Lineage is **data**, not convention.

### 4.4 Lessons for nudox IR

1. **Decouple content identity from name identity.** Hash = what it *is*; moniker = what humans/API call it.
2. **Body change ⇒ new content node + lineage edge** — never mutate content IDs.
3. **Patches are explicit supersedes maps** — Terminus lineage edges should look like Unison patches at IR scale.
4. **Propagation automation needs type/signature compatibility gates** — same as Unison’s type-preserving vs breaking split.
5. Full Unison-style name erasure is too language-specific for multi-language IR; use **normalized signature hashes** + optional body hashes instead of pure de Bruijn AST hashes.

---

## 5. rust-analyzer / salsa: intra-session stability (not cross-version)

### 5.1 Architecture relevant to IDs

Sources: [r-a architecture book](https://rust-analyzer.github.io/book/contributing/architecture.html), ItemTree docs, Salsa red-green notes, durability essays.

**ItemTree**

- Per-file lowered summary of items.
- **Invalidation barrier:** typing inside function bodies typically does not change ItemTree → name resolution / item data stay valid.
- Condenses SyntaxTree into body-stable structure.

**DefMap**

- Module tree + scopes for a crate (`crate_def_map_query`).
- Intermediate `block_def_map_query` extracts submodule names so body edits don’t thrash full module trees.

**Interning**

- `Intern` / `Lookup`: bidirectional append-only map location ↔ integer ID.
- Definitions keyed by `ItemLoc` = (module ID, item ID in module) — **positional among items**, not byte offsets.
- Explicit design: avoid text ranges / syntax trees as query keys.

**AstIdMap**

- Bidirectional map between position-independent `AstId`s and position-dependent syntax nodes.

### 5.2 Salsa mechanics

**Red-green / revision model**

- DB revision increments on input change.
- Tracked functions cache result + dependency set + last-changed revisions.
- Reuse if no input changed; else recompute.
- **Backdating:** recompute dependency produces equal output → treat as unchanged, stop cascade.

**Durability**

- Inputs classified (e.g. volatile user files vs durable stdlib).
- Version vector per durability tier; low-durability edits skip validating high-durability subgraphs.
- Historically removed ~300ms of false invalidation work on stdlib-heavy queries in r-a.

### 5.3 What transfers to cross-VERSION identity

| Transfers well | Does **not** transfer |
|----------------|----------------------|
| Body vs signature separation (ItemTree barrier) | Integer interned IDs (session-local, allocate from 0) |
| Positional item indices as soft keys within a file snapshot | Durability (optimization, not identity) |
| “Never use text ranges as identity” | Salsa revision numbers |
| Structural invalidation barriers for incremental IR | Macro `HirFileId` recursion as published moniker |

**Critical:** r-a IDs are **intra-session** handles for incremental recompute. Across package versions / process restarts they are meaningless. nudox must **not** publish salsa-style u32s as generation-stable IDs. Publish monikers + content hashes; use interned IDs only inside a producer run.

**Transferrable design pattern:** multi-layer IR where body edits don’t invalidate signature monikers — mirror ItemTree as “surface item summary” vs body AST for hashing.

---

## 6. Meta Glean: facts, stacking, Glass symbol IDs

### 6.1 Fact model

Sources: [glean.software](https://glean.software/), [incremental blog](https://glean.software/blog/incremental/), [Meta eng blog 2024-12-19](https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/), [GitHub facebookincubator/Glean](https://github.com/facebookincubator/Glean).

- **Predicates** ≈ tables; **facts** ≈ immutable rows.
- Facts form a DAG; automatic dedup of identical terms.
- Query language: **Angle** (Datalog-like).
- Schema is **per-language** (C++, Hack, Haskell, JS/Flow, …) plus cross-language abstractions (e.g. `codemarkup`).
- Efficient lookup requires a **prefix of key fields** (e.g. name + namespace for a function) — millisecond-scale point queries.

### 6.2 Incremental indexing via stacked immutable DBs

From [Incremental indexing with Glean](https://glean.software/blog/incremental/):

1. Base DB is immutable.
2. Incremental create: `glean create --repo <new> --incremental <old> --exclude A,B,C` — hide units (typically files/modules) from base, stack new facts on top.
3. Client sees a single logical DB.
4. **Ownership sets**: which units own which facts; Elias-Fano + interval maps; ~7% size overhead.
5. Derived facts visible iff **all** source facts visible (ownership intersection).
6. Stacks can form trees; intermediate nodes remain queryable.
7. Overhead: +2–3% index (Python), <10% typical query; heavy search can be ~3×.

Goal: index cost **O(changes)** (practically **O(fanout)** for C++ headers).

### 6.3 Glass symbol IDs

From Meta’s open-source writeup:

> Glass assigns every symbol a *symbol ID*, a unique string that identifies the symbol. For example, the symbol ID for `folly::Singleton` would be something like `REPOSITORY/cpp/folly/Singleton`.

- Stable enough for **permanent documentation URLs** even when definition moves.
- Format **varies per language**.
- Join key for find-refs, docs, ownership.
- C++Now / talks note practical navigation often **matches symbol IDs without full commit discipline** — can jump to latest; file identity keyed on commit is an acknowledged gap for true cross-revision fidelity.

### 6.4 Lessons for nudox

1. **Immutable stacked generations** map cleanly onto package IR generations + Terminus layers.
2. **Unit ownership** (file/module) is the right granularity for hide/replace on re-index.
3. **Glass-style stable string IDs** ≈ SCIP version-stripped monikers — path/name hierarchical, language-prefixed.
4. Glean does **not** magically solve renames: symbol ID continuity when names change still needs lineage edges or alias facts.
5. Schema flexibility (per-language predicates + common markup) matches nudox’s multi-frontend design.

---

## 7. Semantic / API diff tooling — matching algorithms and rename failures

These tools answer “did the public API break?” not “is this the same entity for lineage?” — but their **matching** is the industrial default for cross-version item pairing.

### 7.1 cargo-semver-checks (Rust)

Sources: [FOSDEM 2024 notes / Predrag](https://predr.ag/blog/semver-in-rust-tooling-breakage-and-edge-cases/), [2025 year in review](https://predr.ag/blog/cargo-semver-checks-2025-year-in-review/), [crates.io](https://crates.io/crates/cargo-semver-checks), [Rust project goal](https://rust-lang.github.io/rust-project-goals/2025h2/cargo-semver-checks.html).

**Matching:** **public API import path**. Trustfall queries over rustdoc JSON:

1. Collect public functions (etc.) importable in old crate.
2. Look for a corresponding item at the **same public path** in new crate.
3. Absence → breaking removal lint.

**Fails on:**

- Renames (old path gone, new path “added”).
- Module moves without re-export at old path.
- Historically: parameter type changes, some generics/lifetimes (gap being closed via richer rustdoc JSON + witness programs in 2025–2026 goals).
- Cross-crate re-export analysis still a known hard edge.

**Does not implement** structural rename matching. SemVer culture expects `#[deprecated]` + `pub use` re-export for renames ([Cargo SemVer reference](https://doc.rust-lang.org/cargo/reference/semver.html)).

### 7.2 japicmp (Java)

Sources: [japicmp site](https://siom79.github.io/japicmp/), [GitHub siom79/japicmp](https://github.com/siom79/japicmp).

**Matching:** fully qualified class name + method signature (name + descriptor). Binary/source compatibility classification per JLS.

**Fails on:** class renames / package moves → REMOVE + NEW, not “renamed.” No general structural rename heuristic in documented behavior.

### 7.3 go apidiff (`golang.org/x/exp/apidiff`)

Sources: [pkg.go.dev](https://pkg.go.dev/golang.org/x/exp/apidiff), [README](https://go.googlesource.com/exp/+/refs/heads/master/apidiff/README.md).

**Matching:** **name-based** for exported objects + relaxed **type correspondence**:

- Defined types correspond if same name, or rename-compatible via aliases / unexported renames that don’t change exported surface.
- Functions/vars/consts: **same name** + corresponding types/signatures.
- Module: packages by relative path.

**Fails on:**

- Exported renames (T → U without alias) = remove + add.
- Package path moves.
- Behavioral changes (deliberately out of scope).
- Several compile-break edge cases intentionally ignored (unkeyed struct literals, etc.) to reduce false positives.

### 7.4 TypeScript API Extractor (`@microsoft/api-extractor`)

Sources: [architecture notes](https://api-extractor.com/pages/contributing/architecture/), npm package.

**Within a build:**

- `AstSymbol` / `AstDeclaration` simplify TS compiler symbols.
- Follow import/export chains to the “followed symbol.”
- `CollectorEntity` → `ApiItem` tree for `.api.json` / `.d.ts` rollup.

**Across versions:** API review / `.api.md` diffs are effectively **path/name structured item trees**. Documented architecture does **not** describe rename recovery — renames surface as remove+add in review files. Local rename only for disambiguation inside rollup (`_makeUniqueNames`).

### 7.5 Cross-tool summary

| Tool | Match key | Rename handling |
|------|-----------|-----------------|
| cargo-semver-checks | Public import path | Fail → breaking |
| japicmp | FQN + signature | Fail → remove+add |
| go apidiff | Exported name + type correspondence | Fail for exported renames; OK for unexported/aliases |
| API Extractor | API item tree paths | Fail → review remove+add |

**Industry consensus:** API tools treat **path/name identity** as ground truth. **Rename = new identity for compatibility purposes.** nudox lineage must **go beyond** API tools if “same symbol after rename” is a product requirement — matching should feed **lineage edges**, not redefine monikers silently.

---

## 8. GitHub stack graphs and Blackbird

### 8.1 Stack graphs

Sources: [github/stack-graphs](https://github.com/github/stack-graphs) (**archived 2025-09-09**), [intro blog](https://github.blog/open-source/introducing-stack-graphs/), [paper arXiv:2211.01224](https://arxiv.org/abs/2211.01224), [tree-sitter-stack-graphs](https://crates.io/crates/tree-sitter-stack-graphs) (~0.10.0 at archive).

**What they are:** file-incremental, language-agnostic name resolution (go-to-def / find-refs) without build system, based on **scope graphs** (Visser et al.).

**Symbol identity:** **exact string equality** of push/pop symbol nodes — no content addressing. Synthetic symbols (`.`, `()`) encode constructs. FQNs emerge from path stitching through root + exported scopes, not stored as SCIP-style strings.

**Algorithm sketch:**

1. Per-file index: tree-sitter CST → TSG rules → stack graph nodes/edges → **partial paths** in SQLite.
2. Query time: **path stitching** (`ForwardPartialPathStitcher`) concatenates partial paths when nodes + symbol/scope stack preconditions match.
3. Dual stacks: symbol stack + scope stack (type-directed lookups).

**Status:** GitHub archived the Rust stack-graphs monorepo Sep 2025 (also archived related “semantic” work). Treat as **design reference**, not a dependency to adopt for long-term support.

### 8.2 Blackbird (code search)

Source: [Technology behind GitHub’s new code search](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/).

- Separate from stack graphs: **search index**, not precise nav.
- Rust engine; ngram indices for content, **symbols**, paths.
- Ingest includes “a service for extracting symbols from code” — symbol extraction is a pipeline stage, not a rich moniker protocol.
- Sharding by **git blob SHA** — content-addressed blob dedup across repos.
- Query consistency at commit granularity; **no public cross-commit symbol lineage model**.

**For nudox:** Blackbird validates **content-addressed blob identity** + separate symbol ngrams for search. Do not expect Blackbird-style systems to solve generation lineage.

---

## 9. Git-level rename detection and Rust APIs (gix)

### 9.1 git diffcore-rename

Source: [gitdiffcore](https://git-scm.com/docs/gitdiffcore).

**Two-phase pipeline:**

1. **Exact OID match** — deleted and added files with identical blob hash → rename score 100.
2. **Inexact `estimate_similarity()`** — content chunk commonality; score = `(unchanged / max(old,new)) * 100`. Default threshold **50%** (`-M`).

Optimizations:

- Basename-assisted pass (same basename, different directory) before full O(n²).
- Size filter (~within 10%) to prune candidates.
- Greedy matching by descending score.
- `-C` copy detection; `--find-copies-harder` includes unmodified sources.
- `-B` break rewrites into delete+add before rename pairing.
- `git log -L` follows function ranges through renames via pairwise detection.

### 9.2 libgit2

[`git_diff_find_options`](https://libgit2.org/docs/reference/main/diff/git_diff_find_options.html):

- Thresholds: rename 50, copy 50, break_rewrite 60, rename_limit 1000.
- Flags: `FIND_RENAMES`, `FIND_COPIES`, `FIND_REWRITES`, `EXACT_MATCH_ONLY`, …
- Pluggable `git_diff_similarity_metric`.

### 9.3 gitoxide (gix) — preferred for nudox

Sources: [gix::diff](https://docs.rs/gix/latest/gix/diff/index.html), [gix-diff](https://lib.rs/crates/gix-diff), [gitoxide](https://github.com/GitoxideLabs/gitoxide).

APIs:

- `gix::diff::tree_with_rewrites()` — tree diff + rename/copy
- `gix::diff::Rewrites` — thresholds/modes
- `gix::diff::rewrites::Tracker` — rewrite detection
- `gix::diff::resource_cache()` — blob matrix for similarity
- `gix::diff::new_rewrites()` — from git config
- `imara-diff` for line-level diffs (feature-gated)

Completeness (crate-status notes): exact content match, similarity thresholds, basename assist, directory tracking, find-copies-harder — **done**. Deviation: gix may pick first candidate above threshold vs git’s multi-candidate ranking with filename tiebreak. Blame rename tracking still incomplete.

**Workspace note (2026-07-16):** `gix` is **not** currently a dependency under `workspace/`. Adoption would be new; justified for commit-level file rename feed into symbol lineage when packages are git-sourced.

### 9.4 Mapping git renames → symbol lineage

File rename ≠ symbol rename, but:

1. File path is often part of moniker (TS file path descriptors, Kythe path, Glass paths).
2. When path component of moniker changes and content similarity is high, emit **lineage candidates** at file level, then refine to symbols via path-relative FQN match + signature hash.
3. Exact blob match ⇒ strong “same file, new path”; inexact ⇒ probabilistic edge with score.

---

## 10. Content-addressed stores as identity substrate

### 10.1 Nix

Source: [Content-addressing derivation outputs](https://nix.dev/manual/nix/2.28/store/derivation/outputs/content-address.html).

| Mode | Path determined by | Implication |
|------|--------------------|-------------|
| Input-addressed (default) | Hash of derivation (inputs, builder) | Input change → new path even if output bytes identical |
| Fixed-output | Declared content hash | Same content hash → same store path; network allowed; post-build verify |
| Floating CA (experimental) | Post-build content hash | Needs deterministic builds |

**Invariant for nudox:** fixed-output style “same content ⇒ same address” is the right model for **IR symbol bodies** and **embeddings**. Input-addressed style is right for **build artifacts that depend on toolchain** (already partially present via `JobKey` in `workspace/compiler/generate`).

### 10.2 Dolt prolly trees

Sources: [Prolly tree docs](https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree), [structural sharing study](https://www.dolthub.com/blog/2024-04-12-study-in-structural-sharing/).

- Probabilistic B-tree; every node content-addressed.
- Chunk boundaries via CDC/rolling hash on **keys** → **history-independent** structure (same data ⇒ same chunks regardless of insertion order).
- Diff cost ∝ diff size; structural sharing across versions.
- Hierarchy: row chunks → table hash → DB root → commit hash.

**Pattern:** store IR symbol tables in history-independent CAS so generation diffs are cheap and identical symbols share storage.

### 10.3 IPFS / IPLD Merkle-DAG

Sources: [Merkle-DAG](https://docs.ipfs.tech/concepts/merkle-dag/), [content addressing](https://docs.ipfs.tech/concepts/content-addressing/).

- **CID** = self-describing multihash + codec.
- Parent embeds child CIDs → leaf change rewrites ancestor chain.
- Structural sharing of unchanged sub-DAGs.
- Lineage = new root CID + shared subtrees; explicit edges optional.

**Pattern for symbols:** symbol node CID = hash(normalized signature ∥ body ∥ deps); rename = new moniker metadata node pointing at same or new content CID + `supersedes` edge.

### 10.4 Synthesis of CAS pattern

```
same content  → same node ID  → dedup + cache hit
changed content → new node ID → edge: supersedes(old, new, reason, confidence)
```

nudox already has package-level content hashing (`ContentHash`, BLAKE3 in generate/registry). **Extend to per-symbol content hashes** for incremental embed/index.

---

## 11. Vector / index invalidation in production semantic search

### 11.1 Sourcegraph Cody

Sources: [How Cody understands your codebase](https://sourcegraph.com/blog/how-cody-understands-your-codebase) (2024-02), embeddings docs redirects.

- **Historical:** OpenAI ada-002 embeddings; **file-path keyed**; incremental remove/add for changed files (`incremental: true`).
- **Enterprise pivot (2024):** **abandoned embeddings** as primary retrieval for scale (>100k repos), third-party data sending, config burden.
- Replacement: native search with adapted **BM25 + learned signals**.
- Embeddings remain “active research”; autocomplete uses local tree-sitter context.

**Lesson:** at monorepo/enterprise scale, lexical+graph may beat pure embedding retrieval; still need symbol identity for **precise nav**, separate from RAG.

### 11.2 GitHub Copilot (public analyses)

Secondary sources (Medium / architecture blogs; treat as approximate):

- Key: **URI + content version**
- Chunks ~100–250 tokens, ~512-dim vectors, SQLite cache
- FS listeners + debounce re-index changed files only

### 11.3 Cursor

Sources: [Secure codebase indexing](https://cursor.com/blog/secure-codebase-indexing), [Engineers Codex analysis](https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast).

- Embeddings keyed by **chunk content** (path-independent ⇒ renames reuse cache).
- **Merkle tree of SHA-256** over workspace files; ~10 min sync of roots; walk only mismatched subtrees.
- AST chunking via tree-sitter preferred.
- Remote vector DB (Turbopuffer); proof-of-possession via content hashes.
- No long-term server-side raw source retention after request lifecycle (product claim).

### 11.4 continue.dev

Source: [DeepWiki indexing](https://deepwiki.com/continuedev/continue/3.4-codebase-indexing).

- SQLite `tag_catalog`: (directory, branch, artifact) → (path, mtime, hash).
- `getComputeDeleteAddRemove()` classification.
- LanceDB vectors with `cachekey` for chunk-level invalidation.
- No Merkle tree; flat hash table sufficient for single-level file tracking.

### 11.5 Augment Code

Source: [real-time index blog](https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable).

- Seconds-level updates; PubSub + BigTable + GPU embed.
- Proof-of-possession hashes; multi-tenant index RAM sharing; custom code embedding model.

### 11.6 Industry convergence (2025–2026)

1. **Content hash as cache key** (chunk or file) — not mtime alone; prefer content over path for rename resilience.
2. **Merkle or equivalent** for change detection at scale (Cursor).
3. **Chunk-granularity invalidation** inside changed files.
4. **AST-aware chunking** (tree-sitter).
5. **Symbol-level identity still separate** from embedding keys — embeddings are retrieval; monikers are identity.

**nudox recommendation:** key embeddings by **`symbol_content_hash`** (or chunk hash of symbol body+docs), not moniker alone. On moniker rename with same content hash → **reuse embedding**, write lineage edge moniker_old→moniker_new. On content change → new embedding, lineage edge with reason `body_changed`.

---

## 12. Comparative matrix

| System | Primary ID | Cross-version continuity | Stale / incremental strategy |
|--------|------------|--------------------------|------------------------------|
| **SCIP** | scheme+pkg+version+descriptors | Version-pinned; no alias protocol | Search fallback; per-doc reindex |
| **LSIF** | moniker graph + packageInfo | Same as SCIP | Full regenerate (ID order) |
| **Kythe** | VName 5-tuple | Signatures not required stable | Deployment-defined |
| **Unison** | SHA3-512 AST hash | **Patches** (hash→hash) | Append-only; old always resolvable |
| **rust-analyzer** | interned u32 / ItemLoc | **Session-only** | Salsa durability + ItemTree barrier |
| **Glean/Glass** | fact keys + symbol ID strings | Stacked DBs; ID string match | Unit hide/replace O(changes) |
| **API diff tools** | public path / FQN | Path equality only | N/A (pairwise versions) |
| **Stack graphs** | symbol string equality | None built-in | Per-file partial paths |
| **git** | blob OID + path | Rename similarity edges | Commit pairwise |
| **Nix/Dolt/IPLD** | content hash / CID | Structural sharing + new root | Diff ∝ Δ |
| **Cursor et al.** | chunk content hash | Content-keyed cache | Merkle / file hash invalidation |

---

## 13. Recommended identity architecture for nudox

### 13.1 Three-layer model (required)

#### Layer A — SymbolMoniker (tier-0 continuity key)

SCIP-inspired, **version-stripped for continuity**, version-bearing for **generation snapshots**.

**Canonical grammar (proposal):**

```
<nudox-moniker> ::= <scheme> ' ' <ecosystem> ' ' <package-name> ' ' <descriptor>+
<generation-symbol> ::= <nudox-moniker> '@' <generation-id>
```

- `scheme`: language frontend id — `nudox-rust`, `nudox-ts`, `nudox-go`, `nudox-java`, `nudox-py`, `nudox-cs`, …
- `ecosystem`: `cargo` | `npm` | `gomod` | `maven` | `pypi` | `nuget` | …
- `package-name`: registry package id (not local path)
- `descriptor+`: SCIP suffixes (`/`, `#`, `.`, `().`, …)
- `generation-id`: package version or nudox generation UUID/hash

**Within a generation index**, store full `generation-symbol` (debuggable, SCIP-compatible).  
**For lineage joins**, primary key is **version-stripped moniker** + package identity.

Optional Glass-like compact form for URLs:  
`{registry}/{lang}/{package}/{descriptor-path}`.

**Interop:** implement `From`/`Into` with SCIP strings via the **`scip` crate v0.9.x** for import/export of indexes. Do not require Kythe protos.

#### Layer B — Content hashes (change detection + dedup)

Compute at least two hashes per symbol (BLAKE3 preferred for speed; align with existing `ContentHash` in workspace):

| Hash | Input (normalized) | Use |
|------|--------------------|-----|
| **`sig_hash`** | Normalized public signature (name components, types, generics, visibility) with language-specific normalization | API-surface sameness; API-diff affinity |
| **`body_hash`** | Normalized body AST/IR or empty for pure signatures | Implementation change detection |
| **`doc_hash`** (optional) | Doc comment text normalized | Doc-only invalidation |
| **`embed_key`** | `blake3(sig_hash ‖ body_hash ‖ doc_hash ‖ embed_model_id)` | Embedding cache key |

Normalization rules (Unison-inspired, language-adapted):

- Strip local binding names where safe; keep public API names in moniker layer only.
- Canonicalize type paths to monikers where possible (hard; fall back to pretty-printed canonical types).
- Exclude whitespace, comments (except doc_hash), span locations.

**Rules:**

- `sig_hash` equal + moniker equal ⇒ same API entity, possibly body-changed.
- moniker changed + `body_hash`/`sig_hash` equal ⇒ **rename/move candidate**.
- both changed ⇒ new entity unless matcher produces lineage edge.

#### Layer C — Lineage edges (when moniker/hash disagree)

Stored in Terminus (or terminus-store) as first-class edges, Unison-patch-shaped:

```
LineageEdge {
  from: SymbolId,          // generation-qualified
  to:   SymbolId,
  kind: Rename | Move | SignatureEvolve | BodyEdit | Split | Merge | Reexport | Manual,
  confidence: f32,         // 1.0 exact, <1 heuristic
  evidence: [Evidence],    // git rename score, path distance, hash equality, human
  generation_from, generation_to,
}
```

**Matchers feeding edges (ordered pipeline):**

1. **Exact moniker** (version-stripped) → identity, no edge needed (same continuous id).
2. **Exact content** (`sig_hash`+`body_hash`) under same package → rename/move edge if moniker differs.
3. **git file rename** (gix) + relative moniker suffix match → high-confidence move.
4. **API-path tools** (cargo-semver-checks style) → confirm removals/adds; do not invent renames.
5. **Similarity** (signature string edit distance, embedding cosine on old embed) → low-confidence candidates for review / soft links.
6. **Manual / published patch** (library author map) → confidence 1.0.

Never silently rewrite monikers. Continuity is edges + stable moniker policy.

### 13.2 What to adopt / imitate (with versions)

| Artifact | Decision | Version / note |
|----------|----------|----------------|
| **`scip` crate** | **Adopt** for parse/format interop | **0.9.0** (2026-06) |
| SCIP grammar | **Imitate** for monikers | scip.proto mainline |
| LSIF moniker graph | **Reject** | Historical only |
| Kythe VName | **Imitate** path/language fields only | v0.0.75 alive; no hard dep |
| Unison patches | **Imitate** as lineage edge model | Conceptual; no runtime dep |
| rust-analyzer ItemTree | **Imitate** body/signature barrier | Intra-producer only |
| salsa interned IDs | **Do not publish** | Session-local |
| Glean stacking | **Imitate** for generation DB layers | Conceptual; Terminus already layer-like |
| Glass symbol IDs | **Imitate** stable URL strings | Align with monikers |
| cargo-semver-checks matching | **Use as API break oracle**, not lineage sole source | path-based |
| **gix** | **Adopt** for git rename/copy when source is git | Latest 0.7x/0.8x line; **not yet in workspace** |
| libgit2 | Prefer gix in pure Rust monorepo | Optional |
| Nix/Dolt/IPLD | **Imitate** CAS + structural sharing | Align with existing BLAKE3 CAS |
| Cursor Merkle invalidation | **Imitate** for workspace embed sync | File merkle → symbol hashes |
| stack-graphs crates | **Do not depend** (archived 2025-09) | Ideas only |

### 13.3 Relationship to existing nudox code

Observed patterns (compiler audit companion):

- Package snapshot `ContentHash` / BLAKE3 already exists (`generate/mod.rs`, registry blob/CAS).
- JobKey folds producer version + toolchain + source + lockfile.
- Per-file digests in source archive.

**Gap:** no per-symbol moniker grammar, no per-symbol sig/body hashes, no lineage edge producer between package generations. This research’s layers plug into:

1. Producer: emit moniker + hashes into IR Index.
2. Registry CAS: store symbol content objects by `body_hash`/`sig_hash`.
3. Publish pipeline: compare generation N−1 vs N → lineage edges.
4. Embed pipeline: invalidate by `embed_key`.
5. Terminus: store edges + generation nodes.

### 13.4 Worked example — three generations

**Package:** `acme-http` (cargo), symbol starts as `Client::fetch`.

#### Generation G1 — v1.0.0

Source:

```rust
// src/client.rs
impl Client {
    pub async fn fetch(&self, url: &str) -> Result<Response, Error> { /* v1 body */ }
}
```

| Field | Value |
|-------|--------|
| Moniker (continuity) | `nudox-rust cargo acme-http Client#fetch().` |
| Generation symbol | `…Client#fetch().@1.0.0` |
| SCIP-export shape | `nudox-rust cargo acme-http 1.0.0 Client#fetch().` |
| `sig_hash` | `Hsig1 = blake3("async fn(&self, &str) -> Result<Response, Error>")` (normalized) |
| `body_hash` | `Hbody1` |
| `embed_key` | `E1 = blake3(Hsig1‖Hbody1‖Hdoc1‖model)` |
| Lineage | (none; birth) |

#### Generation G2 — v1.1.0 — **rename method** + same signature/body moved

Source change: `fetch` → `get` (body identical).

| Field | Value |
|-------|--------|
| Moniker | `nudox-rust cargo acme-http Client#get().` |
| Generation symbol | `…Client#get().@1.1.0` |
| `sig_hash` | `Hsig1` (unchanged) |
| `body_hash` | `Hbody1` (unchanged) |
| `embed_key` | `E1` (**reuse embedding**) |
| Lineage edge | `from: …fetch().@1.0.0 → to: …get().@1.1.0`, kind=`Rename`, confidence=1.0, evidence=`[hash_equality, optional rustc rename / git]` |

API tools report: **breaking removal of fetch + addition of get** (unless `pub use` alias). Lineage still records continuity for graph/UX.

#### Generation G3 — v2.0.0 — **module move** + **signature change**

Source: move `Client` to `src/http/client.rs` module `http::client`, change signature to add `timeout: Duration`.

| Field | Value |
|-------|--------|
| Moniker | `nudox-rust cargo acme-http http/client/Client#get().` |
| Generation symbol | `…http/client/Client#get().@2.0.0` |
| `sig_hash` | `Hsig2 ≠ Hsig1` |
| `body_hash` | `Hbody2 ≠ Hbody1` |
| `embed_key` | `E2` (**re-embed**) |
| Lineage edges | (1) `…get().@1.1.0 → …http/client/Client#get().@2.0.0`, kind=`Move`, confidence=0.9, evidence=`[gix file rename client.rs→http/client.rs, basename Client, method get]` |
| | (2) same pair also kind=`SignatureEvolve` (or single edge with multi-reason), confidence=1.0 for path match after move + name equality |

Query “history of this symbol” walks edges G1→G2→G3 regardless of moniker churn. Embed pipeline only recomputes E2. Search/nav can show “formerly `Client::fetch`”.

```
G1  moniker: Client#fetch().     sig=Hsig1 body=Hbody1 embed=E1
 |  Rename (hash equal)
 v
G2  moniker: Client#get().       sig=Hsig1 body=Hbody1 embed=E1
 |  Move + SignatureEvolve
 v
G3  moniker: http/client/Client#get().  sig=Hsig2 body=Hbody2 embed=E2
```

---

## 14. Operational policies

### 14.1 When to re-embed / re-index / re-publish

| Change | Moniker | sig_hash | body_hash | Action |
|--------|---------|----------|-----------|--------|
| Comment/whitespace | same | same | same* | none (*if body excludes trivia) |
| Docs only | same | same | same | re-embed if docs in embed_key; doc index only |
| Body only | same | same | new | re-embed; lineage BodyEdit; re-index body facts |
| Signature only | same | new | maybe | re-embed; SignatureEvolve; API diff alert |
| Rename | new | same | same | lineage Rename; **no** re-embed; update moniker index |
| File/module move | new | same | same | lineage Move; path index update |
| Split/merge | multi | mixed | mixed | multi-edge; human/heuristic |

### 14.2 Confidence thresholds

- ≥ 0.95: auto-commit lineage edge
- 0.70–0.95: edge with `provisional` flag; UI may confirm
- < 0.70: suggest only; no auto edge

### 14.3 Multi-language moniker schemes

Keep **scheme** = producer id so different frontends never collide even if descriptors look similar. Shared descriptor alphabet (SCIP) maximizes tooling reuse.

### 14.4 Stale generation navigation

Mirror Sourcegraph:

1. Prefer precise moniker resolution in generation G.
2. If missing, follow lineage edges to nearest generation with facts.
3. Else search-based / BM25 / embedding fallback on package snapshot.

---

## 15. Risks and open questions

1. **Signature normalization hardness** — type pretty-printing differs across rustc/OXC/Roslyn; `sig_hash` instability across producer versions would false-break lineage. Need versioned normalizer id in hash domain (`sig_hash = blake3(normalizer_ver ‖ bytes)`).

2. **Generic/impl method identity** — Rust inherent vs trait methods, Go interfaces, Java bridges: moniker disambiguators must be specified per language (SCIP method disambiguator is a start).

3. **Re-exports and type aliases** — path-based tools treat re-exports as first-class API paths; moniker should record **canonical definition moniker** + **export path aliases** (Glean multi-fact style).

4. **Split/merge** — one function extracted into two: Unison doesn’t auto-solve; need multi-parent edges and product UX.

5. **gix vs git parity** — candidate selection differences may yield different rename sources; pin thresholds and test on real monorepos.

6. **Privacy / multi-tenant** — Cursor-style proof-of-possession if embeddings hosted; symbol hashes may leak structure — threat model TBD.

7. **stack-graphs archival** — don’t build production on archived crates; tree-sitter still fine for CST.

8. **Cody-scale lesson** — embeddings may not be primary retrieval; invest monikers+search first, embeddings as secondary.

9. **Producer version bumps** — changing moniker grammar or normalizer must rewrite or dual-write indices; include `moniker_grammar_version` in package IR.

10. **Cross-package identity** — when code is vendored or copied between packages, content hash can link across packages; policy: allow `Copy` lineage kind with low default confidence.

---

## 16. Implementation sketch (phased)

### Phase 0 — Spec freeze

- Freeze SymbolMoniker grammar + language descriptor tables.
- Document version-strip rules and SCIP mapping.
- Add `moniker_grammar_version = 1` to IR.

### Phase 1 — Producer emission

- Each frontend emits moniker + sig_hash + body_hash into Index.
- Unit tests: golden moniker strings per language fixture.
- Wire `scip` crate for optional SCIP export.

### Phase 2 — Generation differ

- Pair G_{n-1} and G_n by moniker; classify unchanged / sig-change / body-change / added / removed.
- Content-hash reverse index for rename detection among removed×added.
- Emit LineageEdge batch to Terminus.

### Phase 3 — git assist

- Optional gix tree_with_rewrites for VCS-backed packages.
- Map file renames → moniker path rewrites.

### Phase 4 — embed invalidation

- Cache embeddings by embed_key.
- Merkle of symbol hashes per package for incremental publish.

### Phase 5 — author patches

- Allow package authors to upload explicit rename maps (Unison-like patches) for confidence 1.0.

---

## 17. Source index (primary URLs)

**SCIP / LSIF**

- https://scip-code.org/
- https://sourcegraph.com/blog/announcing-scip
- https://github.com/sourcegraph/scip/blob/main/scip.proto
- https://crates.io/crates/scip
- https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip
- https://github.com/sourcegraph/scip-clang/blob/main/docs/CrossRepo.md
- https://sourcegraph.com/docs/code_intelligence/explanations/precise_code_navigation
- https://sourcegraph.com/blog/cross-repository-code-navigation
- https://microsoft.github.io/language-server-protocol/specifications/lsif/0.4.0/specification/
- https://microsoft.github.io/language-server-protocol/specifications/lsif/0.5.0/specification/
- https://github.com/microsoft/lsif-node/issues/44

**Kythe**

- https://kythe.io/docs/kythe-uri-spec.html
- https://kythe.io/docs/schema/
- https://kythe.io/docs/schema/writing-an-indexer.html
- https://github.com/kythe/kythe/blob/master/RELEASES.md
- https://github.com/kythe/kythe/releases

**Unison**

- https://www.unison-lang.org/docs/the-big-idea/
- https://www.unison-lang.org/docs/language-reference/hashes/
- https://www.unison-lang.org/docs/faq/
- https://www.unison-lang.org/docs/usage-topics/workflow-how-tos/update-code/
- https://www.unison-lang.org/blog/reducing-churn/
- https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/

**rust-analyzer / salsa**

- https://rust-analyzer.github.io/book/contributing/architecture.html
- https://github.com/salsa-rs/salsa
- https://hackmd.io/@salsa/B19OUlA71l

**Glean**

- https://glean.software/
- https://glean.software/blog/incremental/
- https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/
- https://github.com/facebookincubator/Glean

**API diff**

- https://predr.ag/blog/semver-in-rust-tooling-breakage-and-edge-cases/
- https://predr.ag/blog/cargo-semver-checks-2025-year-in-review/
- https://doc.rust-lang.org/cargo/reference/semver.html
- https://siom79.github.io/japicmp/
- https://pkg.go.dev/golang.org/x/exp/apidiff
- https://api-extractor.com/pages/contributing/architecture/

**GitHub / git / gix**

- https://github.com/github/stack-graphs
- https://github.blog/open-source/introducing-stack-graphs/
- https://arxiv.org/abs/2211.01224
- https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/
- https://git-scm.com/docs/gitdiffcore
- https://libgit2.org/docs/reference/main/diff/git_diff_find_options.html
- https://docs.rs/gix/latest/gix/diff/index.html
- https://lib.rs/crates/gix-diff

**CAS**

- https://nix.dev/manual/nix/2.28/store/derivation/outputs/content-address.html
- https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree
- https://www.dolthub.com/blog/2024-04-12-study-in-structural-sharing/
- https://docs.ipfs.tech/concepts/merkle-dag/
- https://docs.ipfs.tech/concepts/content-addressing/

**Embeddings / invalidation**

- https://sourcegraph.com/blog/how-cody-understands-your-codebase
- https://cursor.com/blog/secure-codebase-indexing
- https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast
- https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable
- https://deepwiki.com/continuedev/continue/3.4-codebase-indexing

---

## 18. Executive summary (in-file)

Industrial systems never answer “same symbol?” with one ID type. SCIP/LSIF/Glass give **readable monikers** that are excellent join keys **within a pinned version** but either embed version (SCIP) or fail on renames (API tools). Kythe’s VNames prioritize indexer consistency over cross-version stability. rust-analyzer’s interned IDs and salsa durability solve **intra-session incrementality**, not multi-generation identity. Glean’s stacked databases and unit ownership are the best industrial model for **incremental generation layers**. Unison alone treats **lineage as first-class data** (content hashes + name metadata + patches). Git rename detection (and gix in Rust) supplies **file-level** continuity signals. Production embedding systems key caches by **content hash** (Cursor chunk hash, Copilot URI+version), with Sourcegraph famously de-emphasizing embeddings at extreme scale in favor of search.

**nudox should implement a three-layer architecture:** (A) SCIP-inspired version-stripped SymbolMoniker as tier-0 API identity; (B) BLAKE3 sig/body/doc hashes for change detection, dedup, and embed keys; (C) explicit Terminus lineage edges (Unison-patch-shaped) fed by moniker match, content match, gix renames, and optional author patches. Adopt **`scip` 0.9.x** for interop; adopt **gix** for git rewrite detection; imitate Glean stacking and Unison patches; do not publish salsa IDs; do not depend on archived stack-graphs. Worked rename→move→signature evolution across three generations shows moniker churn, hash reuse, and edge kinds without silent moniker rewriting.

---

*End of industrial symbol-identity research report.*
