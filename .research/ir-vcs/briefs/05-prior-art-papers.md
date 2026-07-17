# Prior Art: Version Control of Structured IR / ASTs / Knowledge Graphs & Sync of Structured Package Documentation

> **Status (Rev 3.2):** Research context, non-normative — feeds `design/IR-NATIVE-VCS-DESIGN.md`. The "conflict state as representable object" lesson (§1.4), "conflicts are data" (§5.3), and the conflict branch of the §7 architecture sketch are superseded by K14: package IR channels are single-writer linear; concurrent writes fail closed and are never stored.

**Research brief ID:** 05-prior-art-papers  
**Scope:** Academic and industrial systems for patch-theoretic / content-addressed / structured versioning of code, documents, IR, knowledge graphs, and package documentation.  
**Relevance lens:** Building an **IR-native, Pijul-like store** for multi-language documentation IR (structured, mergeable, queryable, syncable across package ecosystems).  
**Length target:** Exhaustive survey with takeaways, citations, and synthesis table.

---

## 0. Framing the problem

An **IR-native documentation store** sits at the intersection of three historically separate stacks:

1. **Patch theory / DVCS** — how concurrent edits compose, commute, conflict, and identify versions (Darcs, Camp, Pijul; categorical and homotopical formalizations).
2. **Structured document & graph VCS** — how trees, JSON, RDF triples, and tables are versioned without line-based diffs (3DM, DeltaXML, Prolly trees / MSTs, TerminusDB layers).
3. **Compiler IR, code intelligence & package docs** — how ASTs, symbols, docs, and build artifacts are addressed, indexed, cached, and served (Unison, SemanticDB/SCIP/LSIF, Glean, Kythe, rustc queries, docs.rs, rustdoc JSON, DocC, Nix CAS).

The design goal of an IR-native store is roughly:

- **Unit of versioning** is a structured fact/node/edge in a documentation IR (not a line of Markdown or a whole HTML tree).
- **Merge model** is preferably algebraic (commuting patches / CRDT-like), not purely three-way snapshot merge.
- **Storage** is content-addressed with structural sharing, incremental sync, and efficient range/query access.
- **Query** supports symbol/doc lookup, cross-crate/cross-language links, and time-travel.
- **Relevance** is highest when a system either (a) versions *structure* rather than text, (b) has a *sound* concurrent-edit model, or (c) already stores *compiler-derived* documentation IR at package scale.

---

## 1. Patch theory & VCS foundations

### 1.1 Darcs and the classical theory of patches

**Primary system:** Darcs (David Roundy et al., ~2003–present).  
**Canonical theory page:** https://darcs.net/Theory  
**Intro article:** Jason Dagit, “Darcs Patch Theory,” *The Monad.Reader* (tutorial treatment of commute, invert, merge).  
https://www.cs.tufts.edu/~nr/cs257/archive/jason-dagit/tmr-darcs.pdf

**Takeaways.**  
Darcs is a patch-oriented DVCS: the repository is a *set of patches* (with dependencies), not primarily a Merkle DAG of snapshots. Primitive operations are **compose**, **invert**, and **commute**. When two patches commute, they can be reordered without changing effect; when they do not, one *depends* on the other or they *conflict*. Historical evolution of conflict machinery: **mergers** (Darcs 1) → **conflictors** (Darcs 2) → experimental **darcs-3** / Camp-inspired theory. Patch theory is the abstract algebra of edits; the implementation must respect invertibility and commute laws so that cherry-pick, rollback, and merge are not ad hoc.

**Relevance.** The foundational alternative to Git’s snapshot model. Any IR-native store that wants “apply package-doc delta A then B ≡ B then A when independent” is standing in the Darcs tradition.

---

### 1.2 Camp / camp-theory (Ian Lynagh)

**Project:** Camp (“Commute And Merge Patches”) — intended as a minimal, prove-correct patch theory for a future Darcs 3.  
**Paper:** Ian Lynagh, *An Algebra of Patches*, 2006.  
https://urchin.earth.li/~ian/conflictors/paper-2006-10-30.pdf  
**Camp site (historical):** http://projects.haskell.org/camp/  
**Darcs wiki summary:** https://darcs.net/Theory  

**Takeaways.**  
Camp aimed to formalize and prove properties of darcs-like patches (commute, merge, conflictors) in a small core rather than in the full Darcs codebase. The theory is close to Darcs 2 conflictors: patches that cannot commute produce explicit conflict objects that themselves behave as patches. The project was research-scale; its lasting impact is conceptual pressure toward *proof-carrying* patch algebras and toward later systems (Pijul) that sought soundness without exponential conflictor nesting.

**Relevance.** Shows that “prove the patch algebra first, implement second” is a real research program—and that incomplete formalization is the main risk when scaling conflictors.

---

### 1.3 Jacobson: inverse semigroup formalization of Darcs

**Paper:** Judah Jacobson, *A Formalization of Darcs Patch Theory Using Inverse Semigroups*, UCLA CAM Report 09-83, 2009.  
https://ww3.math.ucla.edu/camreport/cam09-83.pdf  
(Also linked from https://darcs.net/Theory)

**Takeaways.**  
Jacobson models patch *effects* as elements of **inverse semigroups** (algebraic structures generalizing partial injective functions). Inverse semigroups naturally capture “apply if context exists” partiality of patches and the existence of unique inverses. This moves patch theory from operational folklore toward standard algebra: commute becomes a relation constrained by semigroup laws; merge can be studied as a construction on semigroup elements. The paper is a technical report, not a systems paper, but it is repeatedly cited by later categorical and homotopical work.

**Relevance.** Provides a mathematical vocabulary for *partial* patches (only valid in certain IR contexts)—critical for documentation IR where e.g. “document method M of type T” only applies if T exists.

---

### 1.4 Mimram & Di Giusto: categorical theory of patches

**Paper:** Samuel Mimram & Cinzia Di Giusto, *A Categorical Theory of Patches*, MFPS 2013 / ENTCS 298:283–307, 2013.  
arXiv: https://arxiv.org/abs/1311.3903  
DOI: https://doi.org/10.1016/j.entcs.2013.09.018  
PDF (LIX): https://www.lix.polytechnique.fr/Labo/Samuel.Mimram/docs/mimram_ctp.pdf  
HAL: https://inria.hal.science/hal-00904156/PDF/patches.pdf

**Takeaways.**  
Rather than verifying an implementation, the authors *define* the correct model by a universal property. They build a **category of files and patches** where merge of coinitial patches is a **pushout**. Because incompatible patches may have no pushout in the plain category, they take the **free finite-colimit completion**: objects become finite sets of labeled lines with a transitive relation (partial order structure that can represent conflicts); morphisms are partial functions preserving labels and relations. Conflicts are first-class objects in the completed category, not error cases bolted on later.

**Relevance.** Highest-value formal companion for an IR store: treat documentation IR nodes as objects, IR-deltas as morphisms, and concurrent multi-package updates as colimits. Suggests that “conflict state” should be a *representable object* in the store, not only a UI marker.

---

### 1.5 Homotopical patch theory (Angiuli, Morehouse, Licata, Harper)

**Conference:** Carlo Angiuli, Edward Morehouse, Daniel R. Licata, Robert Harper, *Homotopical Patch Theory*, ICFP 2014.  
ACM: https://dl.acm.org/doi/10.1145/2628136.2628158  
**Journal expansion:** *Journal of Functional Programming*, 2016.  
PDF (CMU): https://www.cs.cmu.edu/~rwh/papers/htpt/jfp.pdf  
Expanded: https://carloangiuli.com/papers/hpt-expanded.pdf  
Blog: https://homotopytypetheory.org/2014/09/01/homotopical-patch-theory/

**Takeaways.**  
The paper develops patch theory *inside homotopy type theory (HoTT)*. Repository spaces are types; patches are **paths** (identity types); patch laws are **higher paths** (homotopies). Higher inductive types (HITs) specify both repository states (point constructors) and patches (path constructors) plus equations. This cleanly separates a **patch theory** (algebraic specification) from a **model** (interpretation as actual file edits). Examples range from simple counters to text-file repositories with context-sensitive patches. Computational content of equality proofs is essential: different paths between the same endpoints can mean different merge strategies.

**Relevance.** Gives a type-theoretic blueprint for “docs IR as a space, doc edits as paths.” For multi-language documentation IR, one could imagine a HIT whose points are IR graphs and whose paths are typed deltas (add symbol, rewrite doc comment, re-export), with laws encoding independence of orthogonal modules.

**Related student work:** Dick Blankvoort, *Implementing Patch Theories in Homotopy Type Theory*, BSc thesis, Radboud University, 2023.  
https://www.cs.ru.nl/bachelors-theses/2023/Dick_Blankvoort___1056960___Implementing_Patch_Theories_in_Homotopy_Type_Theory.pdf

---

### 1.6 Pijul (Meunier et al.) and the modern sound patch CRDT

**Site:** https://pijul.org/  
**GOTO 2023 talk:** Pierre-Étienne Meunier, *Pijul: Version-Control Post-Git*, GOTO Conferences, 2023.  
https://www.youtube.com/watch?v=7MpdZkGj5AI  
**Design essays:**  
- *Towards 1.0* (2020): https://pijul.org/posts/2020-11-07-towards-1.0  
- *Commutation and scalability* (2020): https://pijul.org/posts/2020-12-19-partials  

**Takeaways.**  
Pijul implements a **sound, CRDT-like theory of patches** intended to fix Darcs’s soundness/performance issues while remaining patch-centric (not snapshot-centric). Core model: a file is a **directed graph of byte-interval vertices** introduced by changes; edges carry *status* (alive/deleted/…) and *introducing change id*. Changes only **add vertices/edges** or **map edge labels** (deletions); they never mutate history. Independent changes **commute**: apply order does not change the resulting graph *or* the version identifier. Conflicts are first-class graph states (unordered concurrent alive vertices; “zombie” vertices with mixed alive/dead edges; file-name / directory-cycle conflicts).  

Scalability rewrite (≈2019–2020): vertices address **byte ranges in compressed change files** (zstd variants with partial decompress); **Sanakirja** provides cloneable B-tree key-value storage; **pseudo-edges** keep the alive subgraph connected after large deletes; multi-file model separates inode vs name vertices so renames commute with content edits. **Version identifiers** use a discrete-log-style group product over change hashes so order-independent identity is hard to forge. Sync uses binary search over version ids then partial change download (skip dead contents). Channels are named evolving sets of changes (lighter-weight than Git branches).

**Relevance.** Closest industrial-research system to an “IR-native Pijul store.” Replace line/byte vertices with IR nodes (symbols, doc fragments, type signatures, cross-refs); keep commute, conflict-as-state, content-addressed changes, and partial sync. GOTO 2023 is the best public exposition of UX + theory for practitioners.

---

### 1.7 Git Merkle DAG vs patch CRDT

**Git model (summary of industry consensus + systems literature):**  
Git stores **content-addressed objects** (blob, tree, commit, tag) in a **Merkle DAG**. Commits are snapshots with parent pointers; diffs are *derived*, not primitive. Structural sharing via identical blob/tree hashes. Merge is three-way (or recursive) on trees; history rewriting (rebase) creates new commit nodes. Excellent for integrity, packing, and “what is the tree at H?”; weak at “does patch A commute with B?” and at semantic identity of an edit moved across renames.

**Patch / CRDT model (Darcs/Pijul lineage + CRDT literature):**  
State is a **commutative monoid (or join-semilattice) of changes**. Concurrent independent updates merge without coordination; conflicts are either impossible by construction (pure CRDT) or reified as data (Pijul conflict graph). Identity of a version is a function of the *set* of applied changes, not a path through a DAG.

**Bridge papers / systems:**  
- **Merkle-CRDTs:** Hector Sanjuán, Samuli Poyhtari, Pedro Teixeira, Ioannis Psaras, *Merkle-CRDTs: Merkle-DAGs meet CRDTs*, Protocol Labs Research, 2020.  
  https://research.protocol.ai/publications/merkle-crdts-merkle-dags-meet-crdts/psaras2020.pdf  
  Combines causal/Merkle structure with CRDT payloads for efficient sync in open networks.  
- **Merkle Search Trees:** Alex Auvolat & François Taïani, *Merkle Search Trees: Efficient State-Based CRDTs in Open Networks*, SRDS 2019.  
  https://inria.hal.science/hal-02303490/document  
  Deterministic, content-defined tree shape enabling efficient set reconciliation—sibling idea to Prolly trees (below).

**Takeaways.**  
For documentation IR: use **Merkle** for integrity, dedup, and “fetch object by hash”; use **patch/CRDT** for concurrent multi-author or multi-pipeline updates (e.g. rustdoc rebuild + hand-written tutorials + cross-crate link fix). Hybrid designs (Merkle-CRDT, prolly-backed tables of patches) are the practical middle ground.

---

## 2. Structured document VCS

### 2.1 XML / ordered-tree 3-way merge: 3DM (Lindholm)

**Thesis / paper:** Tancred Lindholm, *A 3-way Merging Algorithm for Synchronizing Ordered Trees — the 3DM merging and differencing tool for XML*, Helsinki University of Technology, early 2000s.  
ACM DocEng 2004 paper: *A three-way merge for XML documents*, https://dl.acm.org/doi/10.1145/1030397.1030399  
Modern reimplementation: `xml-3dm` (Rust) https://docs.rs/xml-3dm ; https://github.com/06chaynes/3dm-rs

**Takeaways.**  
3DM treats documents as **ordered trees** and performs structure-aware match + 3-way merge using **PCS triples** (parent–child–sibling encoding) and content tuples. Handles moves/rearrangements better than line-based merge; designed for human-authored XML/HTML-like docs. Conflict classes include insert/insert, delete/edit, move conflicts. Influenced later structured merge tools (e.g. Spork for Java ASTs, which reuses 3dm-merge ideas).

**Relevance.** Direct prior art for merging **documentation IR trees** (modules → items → sections) when a full patch algebra is not yet available: 3-way structured merge as fallback.

---

### 2.2 Commercial / enterprise structured diff: DeltaXignia (DeltaXML), Altova DiffDog

**DeltaXignia (formerly DeltaXML):** XML Compare/Merge, DeltaJSON; patented tree-alignment algorithms; n-way XML merge.  
https://www.deltaxignia.com/  
**Altova DiffDog:** XML/JSON-aware diff-merge. https://www.altova.com/diffdog

**Takeaways.**  
Industrial validation that **structure-aware diff** is a product category for legal, technical publishing, and data pipelines. Algorithms optimize for ordered/unordered children, key-based list alignment, and ignore-formatting policies. Not open formal theories, but encode decades of heuristics for “same element under different whitespace.”

**Relevance.** Policy catalog for IR merge: which IR attributes are identity keys vs display-only; when order matters (parameter lists) vs not (attribute bags).

---

### 2.3 Prolly trees (Noms → Dolt) and probabilistic content-addressed B-trees

**Origin:** Aaron Boodman et al., **Noms** (“Git for data”), Attic Labs (mid-2010s).  
Noms intro (Prolly trees section): https://github.com/attic-labs/noms/blob/master/doc/intro.md  
**Dolt:** SQL database with Git-like versioning; production Prolly tree engine.  
https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree/  
Blog: *How Dolt Stores Table Data* (2020) https://www.dolthub.com/blog/2020-04-01-how-dolt-stores-table-data/  
*Efficient Diff on Prolly-Trees* (2020) https://www.dolthub.com/blog/2020-06-16-efficient-diff-on-prolly-trees/  
**Academic-adjacent:** *Accelerating Prolly Trees: Simplified Chunking for Rapid Updates*, CEUR 2024.  
https://ceur-ws.org/Vol-3791/paper8.pdf  
**Related structure:** Auvolat & Taïani **Merkle Search Trees** (SRDS 2019) — independent reinvention with hash-determined levels (see §1.7).

**Takeaways.**  
A **Prolly tree** (probabilistic B-tree) is a content-addressed, sorted map: leaf chunks are split by a **rolling/content-defined hash** (or target size PDF), internal nodes store key delimiters + **hashes** of children. Same key-value sequence ⇒ same tree shape and root hash **independent of insertion history** (unlike classic B-trees). Versions structurally share unchanged subtrees; diff walks both trees and skips equal-hash subtrees → **O(change size)** rather than O(data size). Dolt versions tables/rows as Prolly maps; commits form a Git-like DAG over roots.

**Relevance.** Ideal **storage layer** under an IR fact table: `(symbol_id → IR_node)`, `(crate_version → root)`, content-addressed chunks, cheap cross-version diff for “what docs changed between serde 1.0.1 and 1.0.2.” Complements patch theory (Prolly stores *states*; Pijul-like layers store *changes*).

---

### 2.4 TerminusDB: succinct RDF stores + delta layers

**White paper:** Matthijs van Otterdijk, Gavin Mendel-Gleason, Kevin Feeney, *Succinct Data Structures and Delta Encoding for Modern Databases* (orig. 2020; republished TerminusDB blog 2023).  
https://terminusdb.org/blog/2023-01-05-succinct-data-structures-for-modern-databases/  
GitHub PDF (historical): terminus-server docs/whitepaper  
**Product docs:** Knowledge graph version control https://terminusdb.org/docs/knowledge-graph-version-control/  
**Store:** https://github.com/terminusdb/terminusdb-store  

**Takeaways.**  
TerminusDB versions **RDF triple sets** with Git-like workflow (branch, merge, time-travel, squash) but database semantics. Storage = **immutable layers**: each layer is a delta (triples added + triples deleted) over a parent layer hash; labels (branches) point at tip layer ids. Layers use **succinct structures** inspired by HDT: front-coded dictionaries for IRIs, bitvectors + wavelet trees for triple indexing—near information-theoretic size, mmap-friendly, multi-version concurrency via immutability. Schema (OWL-inspired with closed-world / unique-name assumptions) is checked before advancing head. Diff/patch APIs emit **semantic JSON** (triple/document field ops), not line hunks. Layer rollups compress history when depth hurts query latency.

**Relevance.** Extremely close cousin to “documentation as knowledge graph”: symbols, docs, links as triples/documents; crate release = commit; multi-language IR as multi-named-graph. Layer deltas ≈ patch log; succinct encoding matters at crates.io / npm scale.

---

### 2.5 Unison: content-addressed code (AST-as-database)

**Docs:** *The big idea* https://www.unison-lang.org/docs/the-big-idea/  
GitHub: https://github.com/unisonweb/unison  
LWN: *Programming in Unison* (2024) https://lwn.net/Articles/978955/

**Takeaways.**  
Unison identifies each definition by a **hash of its syntax tree** (content-addressed AST), not by name. Names are mutable metadata pointers into an append-only codebase database. Consequences: no traditional builds (definitions typechecked once); renames never break references; tests cache by dependency hash; merges avoid whitespace/import-order false conflicts because the unit is the hashed term. Code is stored as structured data with textual projection for editing.

**Relevance.** The purest “**IR is the source of truth**” language design. A multi-language docs store can adopt the same principle: hash normalized documentation IR nodes; treat rendered Markdown/HTML as projections; version names/paths separately from content hashes.

---

### 2.6 SemanticDB, LSIF, SCIP — code intelligence indexes as versioned artifacts

#### SemanticDB (Scala ecosystem)
Compiler plugin emits per-source **SemanticDB** files: symbols, occurrences, synthetic information. Inspired SCIP’s design.

#### LSIF (Microsoft, ~2018–2019)
**Spec:** https://microsoft.github.io/language-server-protocol/specifications/lsif/0.6.0/specification/  
**Announcement:** https://code.visualstudio.com/blogs/2019/02/19/lsif  
LSIF is a graph dump of language-server knowledge (vertices/edges for definitions, references, hovers) for **offline** “go to def / find refs” without running a live LS. JSON/TSV streaming graph; moniker-based cross-project linking. Verbose and complex to produce correctly.

#### SCIP (Sourcegraph, 2022–; community governance 2026)
**Announcement:** https://sourcegraph.com/blog/announcing-scip  
**Future / governance:** https://sourcegraph.com/blog/the-future-of-scip  
Protobuf schema centered on **human-readable symbol IDs**, occurrences, relationships; smaller and simpler than LSIF; heavily inspired by SemanticDB. Indexers for many languages; Sourcegraph stores indexes for precise navigation across repos and versions.

**Takeaways.**  
These formats are **snapshots of semantic IR** at a commit, not patch algebras. Versioning is typically “one index blob per commit/repo”; merge of indexes is rare (rebuild instead). Cross-repo linking via package/symbol schemes is the hard part—same hard part as multi-language documentation IR linking.

**Relevance.** SCIP/SemanticDB-like schemas are strong **candidates for the documentation IR fact schema** (symbols + docs + relationships). A Pijul-like store would version *deltas of SCIP-like facts* rather than whole index dumps.

---

### 2.7 Meta Glean — typed fact store for code

**Site:** https://glean.software/  
**Open source:** https://github.com/facebookincubator/Glean  
**Meta eng blog (2024):** https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/  
**Intro docs:** https://glean.software/docs/introduction/

**Takeaways.**  
Glean stores **typed, schema-defined facts** about code (and other artifacts) as an immutable **DAG of terms**, automatically de-duplicated, backed by **RocksDB**. Query language **Angle** (Datalog-style). Indexers for many languages; facts cover defs, refs, types, call graphs, thrift/Buck edges, etc. Designed for Meta scale; used for code browser, search, and doc generation. Ownership of facts by file enables incremental reindex. Glean does not decide the schema—you do—making it a general **fact database**, not only a C++ xref tool.

**Relevance.** Blueprint for **query layer** over a docs IR store: schema for `DocItem`, `DocLink`, `TypeSignature`, `PackageVersion`; Angle/Datalog for “all items linking to Foo across crates.” Combine with patch/layer versioning (Glean itself is more snapshot/index-oriented).

---

### 2.8 Google Kythe — language-agnostic semantic graph

**Site:** https://kythe.io/  
**Overview:** https://kythe.io/docs/kythe-overview.html  
**GitHub:** https://github.com/kythe/kythe  
**Context:** Evolved from internal “Grok”; described in *Software Engineering at Google* (Code Search chapter). https://abseil.io/resources/swe-book/html/ch17.html

**Takeaways.**  
Kythe is a **pluggable, mostly language-agnostic** ecosystem: compilation extractors → language indexers → common **graph schema** (nodes for files, anchors, symbols; edges for defines/refers/typed/… ) → services (xrefs, etc.). Schema deliberately spans languages for cross-language navigation in a monorepo. Indexers exist for C++, Java, Go, etc. Emphasizes **build-system fidelity** (indexing the actual compilation).

**Relevance.** Schema design prior art for multi-language documentation IR: stable VNames, corpus/language/path tickets, cross-language edges. Versioning is again per-build snapshot; interesting research is to put Kythe graphs under layer/patch storage.

---

### 2.9 rustc query system & incremental compilation

**Primary docs:**  
- *Queries: demand-driven compilation* https://rustc-dev-guide.rust-lang.org/query.html  
- *Incremental compilation in detail* https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation-in-detail.html  
**Narrative:** LWN, *Rust’s incremental compiler architecture* (2024) https://lwn.net/Articles/997784/  
**Related IC foundations:** Matthew A. Hammer et al., *Adapton: Composable, Demand-Driven Incremental Computation*, PLDI 2014. http://matthewhammer.org/adapton/adapton-pldi2014.pdf  
Umut A. Acar lineage on self-adjusting computation.

**Takeaways.**  
rustc is organized as a **DAG of pure queries** (demand-driven). Incremental compilation persists query results with dependency tracking and red-green (or try-mark) re-evaluation: if inputs’ fingerprints match, reuse. This is **versioning of derived IR** for a single crate build, not multi-user VCS—but the dependency graph + content fingerprinting is exactly how a docs pipeline should avoid rebuilding the universe when one module’s docs change.

**Relevance.** Operational model for **incremental documentation IR production**: treat `rustdoc_json(crate)`, `scip(crate)`, `html(crate)` as queries over source+cfg fingerprints; store results content-addressed; invalidate via query deps.

---

## 3. IR / documentation systems in the wild

### 3.1 docs.rs archive storage

**Repo:** https://github.com/rust-lang/docs.rs  
**Archive design doc:** https://github.com/rust-lang/docs.rs/blob/main/docs/archive-storage-and-indexes.md  
**About:** https://docs.rs/about  

**Takeaways.**  
docs.rs builds rustdoc HTML (and sources) for every crates.io release. Storage evolution: per-file objects → **ZIP archives per crate version** (one for rustdoc output, one for sources) with **SQLite indexes** mapping path → byte range + compression codec. Serving a single HTML file = load small index (cached) + **HTTP Range GET** of one ZIP entry + decompress. Avoids tar.gz’s whole-stream compression which kills random access. Offline archive download supported (bz2 entries chosen for portability).

**Relevance.** Proven **package-scale documentation blob store** with random access. For IR-native docs: same pattern for “pack all IR nodes for serde 1.0.200 in one content-addressed archive + index,” or store IR in Prolly/Terminus layers instead of ZIP.

---

### 3.2 rustdoc JSON

**RFC 2963:** https://rust-lang.github.io/rfcs/2963-rustdoc-json.html  
**docs.rs hosting:** https://docs.rs/about/rustdoc-json  
**Types crate:** `rustdoc-types` on crates.io  
**Talks/posts:** Luca Palmieri EuroRust 2023; Alona Enraght-Moony, *Rustdoc JSON in 2023* https://alona.page/posts/rustdoc-json-2023/

**Takeaways.**  
`rustdoc --output-format json` emits a **versioned, machine-readable crate API + docs IR**: items, module trees, types, markdown docs, deprecation, etc. Unstable format with `format_version`; ecosystem tools (Roogle, cargo-check-external-types, docs analysis) consume it. docs.rs builds and hosts JSON (from 2025-05-23 onward per about page). This is the **de facto structured documentation IR for Rust**.

**Relevance.** Concrete multi-version corpus of package documentation IR. An IR-native store should ingest rustdoc JSON as a primary producer; normalize to a cross-language schema; version deltas between crate releases.

---

### 3.3 Apple DocC archives

**WWDC21:** *Meet DocC documentation in Xcode* https://developer.apple.com/videos/play/wwdc2021/10166/  
**Distributing docs:** https://developer.apple.com/documentation/xcode/distributing-documentation-to-other-developers  
**Catalogs:** articles, extensions, tutorials combined with compiler-extracted symbol graphs → **`.doccarchive`** bundle (package on disk).

**Takeaways.**  
DocC pipeline: Swift/ObjC compilers emit **symbol graphs**; DocC merges with **documentation catalogs** (Markdown articles, tutorials) into a compiled archive for Xcode or static hosting. Clear separation of **extracted API IR** vs **authored narrative**. Archive is the distribution unit.

**Relevance.** Same two-layer model a multi-language system needs: (1) compiler-extracted IR, (2) human narrative overlays, merged into versioned archives. Overlay merge is where patch theory / structured merge matter.

---

### 3.4 Sourcegraph SCIP store (precise code intel backend)

**SCIP:** see §2.6.  
**Evolution of precise code intel:** https://sourcegraph.com/blog/evolution-of-the-precise-code-intel-backend  
Sourcegraph stores SCIP indexes associated with repositories and commits; serves go-to-def / find-refs / hover at monorepo and multi-repo scale; feeds code search and AI context.

**Takeaways.**  
Production proof that **semantic indexes as first-class versioned artifacts** work at enterprise scale. Index lifecycle: generate in CI → upload → bind to commit → query. Replacement of LSIF by SCIP reduced indexer complexity.

**Relevance.** Operational pattern for docs IR: CI produces IR; store binds IR to package version; query API for renderers and agents.

---

### 3.5 Nix CAS, NAR, narinfo

**Binary cache protocol essays:**  
- Farid Zakaria, *A Nix Binary Cache Specification* (2021) https://fzakaria.com/2021/08/12/a-nix-binary-cache-specification  
- Tweag, *Towards a content-addressed model for Nix* (2020) https://tweag.io/blog/2020-09-10-nix-cas/  
- Nix RFC 0062 content-addressed paths; `nix store make-content-addressed`  
- NAR format: deterministic archive of store paths  

**Takeaways.**  
Classic Nix is **input-addressed** (`/nix/store/<hash-of-drv>-name`); binary caches serve **`.narinfo`** (metadata: store path, NAR URL, compression, references, signatures) + **NAR** payloads. Content-addressed derivations (CA-derivations) move toward **hash-of-output** addressing for better sharing and trust. The protocol is a battle-tested **content-addressed artifact CDN** for build products.

**Relevance.** Template for **documentation IR CDN**: `docinfo` analog of narinfo (package, version, IR schema version, hash, refs to dependency docs, signature); NAR-like deterministic pack of IR nodes; input- vs content-addressing tradeoffs (rebuild identity vs output identity).

---

## 4. Synthesis table

| System | Unit of versioning | Merge model | Storage | Query | Relevance to IR-native Pijul-like docs store |
|--------|-------------------|-------------|---------|-------|-----------------------------------------------|
| **Darcs** | Patch (line/hunk oriented) | Commute + invert; mergers/conflictors | Patch sets + working tree | History/annotate via patches | Conceptual ancestor: patch as primitive |
| **Camp** | Patch (formal core) | Proved commute/merge algebra | Research prototype | N/A | Proof-oriented methodology |
| **Jacobson 2009** | Patch effect (inverse semigroup) | Algebraic partial maps | Paper | N/A | Partial patches for context-sensitive IR |
| **Mimram & Di Giusto 2013** | File line patches as morphisms | Pushout / colimit completion | Categorical model | N/A | Conflicts as objects; universal merge |
| **Homotopical patch theory 2014/16** | Paths in HIT repository space | Higher paths = laws | Type-theoretic | Via path induction | Spec language for IR patch theories |
| **Pijul** | Change (byte-interval graph ops) | CRDT-like commute; conflict-as-graph | Sanakirja + change files (zstd) | Channels; version ids | **Primary design reference** for store |
| **Git** | Snapshot commit (Merkle DAG) | 3-way tree merge | Content-addressed objects/packs | Rev-list, tree walk | Integrity + distribution; weak semantics |
| **Merkle-CRDTs / MSTs** | State + causal Merkle metadata | CRDT join + Merkle sync | Merkle DAGs / search trees | Set reconciliation | Sync protocol for open multi-mirror docs |
| **3DM / structured XML merge** | Ordered tree | 3-way structure-aware | Trees / PCS triples | Diff/merge tools | Fallback merge for tree-shaped IR |
| **DeltaXML / DiffDog** | XML/JSON trees | Proprietary alignment + merge | Vendor engines | Enterprise pipelines | Policy heuristics for keys/order |
| **Noms Prolly trees** | Dataset root hash | Snapshot DAG (Git-like) | Probabilistic content-addressed B-tree | Map/set/list APIs | Structural sharing for IR maps |
| **Dolt** | SQL table rows / commits | Git-like branch/merge on tables | Prolly trees + Noms lineage | SQL + diff/blame | Versioned tabular IR / metadata |
| **TerminusDB** | RDF triple layer deltas | Branch/merge + schema check | Succinct immutable layers | WOQL / GraphQL / documents | **KG + Git semantics** for docs graphs |
| **Unison** | Hashed AST definitions | Semantic (hash equality) | Append-only codebase DB | Namespace tools | IR-as-source; name/hash split |
| **SemanticDB** | Per-file semantic snapshot | Rebuild | Binary SemanticDB files | Scalameta tooling | Symbol schema prior art |
| **LSIF** | Workspace semantic graph dump | Rebuild | JSON/TSV graph stream | LSP-like offline queries | Legacy precise-intel format |
| **SCIP** | Index of symbols + occurrences | Rebuild | Protobuf index | Sourcegraph / scip CLI | **Best practical symbol IR schema** |
| **Glean** | Typed facts (DAG terms) | Incremental reindex by ownership | RocksDB fact store | Angle (Datalog) | Query engine for docs facts |
| **Kythe** | Language-agnostic semantic graph | Per-build index | Graph store / services | Xrefs, Code Search | Multi-language node/edge schema |
| **rustc queries** | Derived compiler IR nodes | Fingerprint invalidation | Incremental cache on disk | Demand-driven queries | Incremental IR production model |
| **docs.rs** | Crate-version HTML/source archives | Replace on rebuild | ZIP + SQLite range index on S3 | HTTP path → file | Package-scale docs CDN pattern |
| **rustdoc JSON** | Crate API+docs IR snapshot | Replace on rebuild | Versioned JSON | Tools via `rustdoc-types` | **Primary Rust docs IR producer** |
| **DocC** | `.doccarchive` (symbols + catalog) | Rebuild archive | Bundle on disk | Xcode / static host | Extracted IR + narrative overlay |
| **Sourcegraph SCIP store** | Index@commit | Rebuild/upload | Multi-tenant index service | Precise nav + search | Ops model for IR@version |
| **Nix + narinfo** | Store path / NAR | Substituters (no merge) | CAS + signed metadata | Realization by hash | Artifact CDN + addressing theory |

---

## 5. Cross-cutting design lessons for an IR-native Pijul-like documentation store

### 5.1 Choose two layers, not one

Successful systems separate:

1. **State representation** — content-addressed snapshots of structured data (Prolly roots, Terminus base layers, Git trees, SCIP indexes, rustdoc JSON).  
2. **Change representation** — algebraic deltas (Pijul changes, Terminus add/delete triple sets, patch theories).

An IR-native store should **record Pijul-like changes over an IR graph**, while **materializing** Prolly/Terminus-like snapshots for query and CDN performance. Sync exchanges changes; renders read snapshots.

### 5.2 Identity must be structural

From Unison, Pijul, and Prolly trees: identity of a documentation node should be a **hash of normalized IR**, not a filesystem path or HTML id. Names (`std::vec::Vec::push`) are indexes into the hash space. This eliminates false conflicts from formatting and enables cross-language stable links when normalization is shared.

### 5.3 Conflicts are data

Mimram’s colimits, Darcs conflictors, and Pijul’s conflict graphs agree: **do not erase conflicts into “merge failed.”** Represent conflicting doc states (two concurrent summaries for the same symbol; broken cross-crate link targets) as queryable IR so automated agents and humans can resolve later.

### 5.4 Partiality and context

Jacobson’s inverse semigroups and Pijul’s dependency edges both say patches are **partial**. A doc patch “link method M to type T in crate C v1.2” depends on those nodes existing. The store must track **strict dependencies** vs **known contexts** (Pijul’s refinement) so multi-package pipelines detect true conflicts.

### 5.5 Sync protocol = metadata first, payload second

Nix narinfo, Pijul version-id binary search, docs.rs indexes, Merkle-CRDT reconciliation: always exchange **small capability metadata** (hashes, ranges, version ids) before bulk IR. Partial download of *live* doc nodes (Pijul skips dead change contents) maps cleanly to “only fetch IR still referenced by current release set.”

### 5.6 Multi-language means shared schema + per-language producers

Kythe, SCIP, Glean, DocC, rustdoc JSON: **producers** are language-specific; **schema and store** are shared. Invest in a Kythe/SCIP-inspired documentation schema (`Package`, `Symbol`, `DocBlob`, `Relationship`, `Diagnostic`) with language attributes, not N parallel doc databases.

### 5.7 Incremental production is half the system

rustc queries / Adapton-style demand-driven IC: regenerating docs IR for the entire ecosystem on every commit is impossible. Model producers as pure queries over source fingerprints; persist results CAS-style; invalidate finely.

### 5.8 Human narrative is an overlay

DocC catalogs and docs.rs HTML show that **authored tutorials** and **extracted API docs** must merge. Prefer: extracted IR as base layer; narrative as patches referencing symbol hashes; renderers join them. Structured merge (3DM-like) or patch commute handles concurrent API regen vs tutorial edits.

---

## 6. Annotated bibliography (compact)

### Patch theory & formal methods
1. Roundy et al. — Darcs & Theory wiki — https://darcs.net/Theory  
2. Dagit — Darcs Patch Theory tutorial — https://www.cs.tufts.edu/~nr/cs257/archive/jason-dagit/tmr-darcs.pdf  
3. Lynagh — *An Algebra of Patches* (2006) — https://urchin.earth.li/~ian/conflictors/paper-2006-10-30.pdf  
4. Jacobson — *Formalization… Inverse Semigroups* (2009) — https://ww3.math.ucla.edu/camreport/cam09-83.pdf  
5. Mimram & Di Giusto — *A Categorical Theory of Patches* (2013) — arXiv:1311.3903 — DOI 10.1016/j.entcs.2013.09.018  
6. Angiuli, Morehouse, Licata, Harper — *Homotopical Patch Theory* (ICFP 2014; JFP 2016) — https://www.cs.cmu.edu/~rwh/papers/htpt/jfp.pdf  
7. Meunier — Pijul design posts & GOTO 2023 — https://pijul.org/ — https://www.youtube.com/watch?v=7MpdZkGj5AI  

### Merkle / CRDT / structured state
8. Auvolat & Taïani — *Merkle Search Trees* (SRDS 2019) — https://inria.hal.science/hal-02303490/document  
9. Sanjuán et al. — *Merkle-CRDTs* (2020) — https://research.protocol.ai/publications/merkle-crdts-merkle-dags-meet-crdts/psaras2020.pdf  
10. Noms / Prolly trees — https://github.com/attic-labs/noms/blob/master/doc/intro.md  
11. Dolt Prolly docs — https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree/  
12. van Otterdijk, Mendel-Gleason, Feeney — TerminusDB succinct+delta white paper — https://terminusdb.org/blog/2023-01-05-succinct-data-structures-for-modern-databases/  

### Structured document merge
13. Lindholm — 3DM / three-way XML merge (DocEng 2004) — https://dl.acm.org/doi/10.1145/1030397.1030399  
14. DeltaXignia — https://www.deltaxignia.com/  

### Code intelligence & IR
15. Microsoft — LSIF specification — https://microsoft.github.io/language-server-protocol/specifications/lsif/0.6.0/specification/  
16. Sourcegraph — SCIP announcement — https://sourcegraph.com/blog/announcing-scip  
17. Meta — Glean — https://glean.software/ — https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/  
18. Google — Kythe — https://kythe.io/  
19. Unison — content-addressed code — https://www.unison-lang.org/docs/the-big-idea/  
20. rustc-dev-guide — queries & incremental — https://rustc-dev-guide.rust-lang.org/query.html  
21. Hammer et al. — Adapton (PLDI 2014) — http://matthewhammer.org/adapton/adapton-pldi2014.pdf  

### Package documentation systems
22. docs.rs archive storage — https://github.com/rust-lang/docs.rs/blob/main/docs/archive-storage-and-indexes.md  
23. RFC 2963 rustdoc JSON — https://rust-lang.github.io/rfcs/2963-rustdoc-json.html  
24. Apple DocC — https://developer.apple.com/documentation/xcode/distributing-documentation-to-other-developers  
25. Nix binary cache / CAS — https://fzakaria.com/2021/08/12/a-nix-binary-cache-specification — https://tweag.io/blog/2020-09-10-nix-cas/  

---

## 7. Recommended architecture sketch (synthesis only)

```
                     Language producers
            (rustdoc JSON, DocC symbolgraph, SCIP, …)
                              │
                              ▼
                 Normalize → Canonical Docs IR
                 (Kythe/SCIP-inspired schema;
                  nodes hashed Unison-style)
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
     Change log (Pijul-like)          Snapshot store
     - add/delete/relabel IR edges    - Prolly map or
     - deps + known-context             Terminus-like layers
     - commute when independent       - root hash per package-version
              │                               │
              └───────────────┬───────────────┘
                              ▼
                    Query (Glean/Angle or SQL)
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
           Renderers      Sync/CDN        Agents
           (HTML/MD)   (narinfo-like     (fix links,
                        docinfo+CAS)      merge conflicts)
```

**Why this mix:**  
- **Pijul-like changes** give correct concurrent multi-pipeline updates (regen API docs ‖ edit tutorial ‖ fix cross-links).  
- **Prolly/Terminus snapshots** give O(delta) diff, structural sharing, and fast query.  
- **SCIP/Glean schemas** give multi-language symbol/doc facts.  
- **docs.rs/Nix CDN patterns** give package-scale distribution.  
- **Homotopy/category theory** optionally formalize the patch laws before implementation freezes.

---

## 8. Gaps / open research (useful for novelty claims)

1. **No production system** fully combines sound patch commute **with** compiler-grade multi-language docs IR.  
2. **Semantic commute** for docs (reorder independent API items) is richer than line commute—needs IR-specific patch laws (Homotopical PT style).  
3. **Cross-ecosystem identity** (Rust trait ↔ Swift protocol ↔ TypeScript interface) lacks a stable shared naming/hash theory.  
4. **Merge of narrative + extracted API** under continuous rebuild is unsolved beyond DocC’s rebuild-all archive model.  
5. **Partial sync of live IR** (Pijul’s skip-dead-contents) applied to documentation CDNs is under-explored.  
6. **Proof-carrying doc patches** (Camp/Jacobson/Mimram) have not been connected to SCIP-scale indexes.

---

## 9. Conclusion

Prior art clusters into: (A) **algebraic patch theories** (Darcs → Camp → Jacobson → Mimram → HoTT → Pijul), (B) **content-addressed structured state** (Git, Prolly/MST, Terminus layers, Unison, Nix), (C) **semantic indexes** (Kythe, Glean, SemanticDB/LSIF/SCIP), and (D) **package doc delivery** (docs.rs, rustdoc JSON, DocC).  

An **IR-native Pijul-like store for multi-language documentation** should treat (A) as the concurrency model, (B) as the storage engine, (C) as the schema, and (D) as the operational scale target. The highest-leverage citations for design justification are: **Mimram & Di Giusto (2013)**, **Angiuli et al. (2014/16)**, **Pijul/Meunier (2020–2023)**, **TerminusDB succinct layers (2020)**, **Prolly trees / Dolt**, **SCIP (2022)**, **Glean**, and **rustdoc JSON / docs.rs archives**.

---

*End of research brief 05-prior-art-papers.*
