# Academic Literature Survey: Symbol Identity Across Package Generations

**Research task:** 05-symbol-identity-academic  
**Date:** 2026-07-16  
**Context:** nudox multi-language code-intelligence platform — IR generations, incremental re-embed / re-index / TerminusDB lineage  
**Central question:** *Under what reasonable interpretation can we say that two symbols, across generations of a package, are THE SAME symbol?*  
**Asymmetry constraint:** False merges corrupt lineage history; false splits only lose continuity. Prefer false-split over false-merge.

---

## 0. Executive framing

nudox lowers package source into a shared IR of typed symbols (functions, types, modules) with signatures, docs, and reference edges. Each published package version is a new **generation**. Incremental pipeline work requires deciding, for each symbol in generation \(G_{n}\), whether it is a continuation of some symbol in \(G_{n-1}\).

This is the classic **program-element matching / origin analysis** problem, studied since the early 2000s in software evolution, mining software repositories (MSR), refactoring detection, clone genealogy, and AST differencing. The literature does **not** deliver a single universal identity predicate. It delivers:

1. A **layered** notion of identity with graceful degradation (exact name → rename → move → signature evolve → split/merge → birth/death).
2. Feature sets that repeatedly dominate empirical studies: **name**, **body/text**, **signature**, **call/reference structure**, **container/path**.
3. Precision/recall numbers that degrade sharply as the identity claim becomes more ambitious.
4. A clear evaluation culture: human-validated oracles, multi-tool union+review, and asymmetric error costs.

This report surveys that literature exhaustively, cites primary sources with URLs, extracts algorithms/features/thresholds/accuracy, and synthesizes a concrete **nudox matching pipeline** with confidence-tagged lineage edges.

---

## 1. Problem formalization

### 1.1 Identity as origin relationship

Kim, Pan & Whitehead (WCRE 2005) formalize **origin relationship** between consecutive revisions \(r_1, r_2\):

- Let \(D = \{d_1,\ldots,d_m\}\) be functions deleted (present in \(r_1\), absent in \(r_2\) under name-based identity).
- Let \(A = \{a_1,\ldots,a_n\}\) be functions added.
- Candidate origin pairs form the bipartite product \(D \times A\).
- Maximum number of origin relationships is \(\min(|D|,|A|)\).
- Pair \((d_x, a_y)\) has an origin relationship iff \(a_y\) is renamed and/or moved from \(d_x\).

Source: https://users.soe.ucsc.edu/~ejw/papers/kim-wcre2005.pdf

Godfrey & Zou (IEEE TSE 2005) extend origin analysis to **merging and splitting** of files and functions — not only 1:1 rename/move, but 1:N and N:1 entity evolution.  
Source: https://plg.uwaterloo.ca/~migod/papers/2003/wcre03-tseVersion.pdf  
Also: https://swag.uwaterloo.ca/publications/using-origin-analysis-to-detect-merging-and-splitting-of-source-code-entities.html

Kim & Notkin (MSR 2006) frame the general problem: *multi-version program analyses require that elements of one version be mapped to elements of other versions*.  
Source: https://web.cs.ucla.edu/~miryung/Publications/msr06-matching.pdf  
ACM: https://dl.acm.org/doi/10.1145/1137983.1137999

### 1.2 What "same symbol" can mean (tier lattice)

| Tier | Interpretation of identity | Typical evidence | Literature home |
|------|---------------------------|------------------|-----------------|
| T0 | **Content-identical** | Cryptographic hash of normalized body | SWHID / content addressing |
| T1 | **Path+name exact** | Fully-qualified name equal, same kind | Symbol tables, API tools |
| T2 | **Rename (same container)** | Body/signature/refs match; name differs | Origin analysis, RefactoringMiner Rename* |
| T3 | **Move (possibly rename)** | Body match across path change | CodeShovel, CodeTracker, Move Method/Class |
| T4 | **Signature evolution** | Same entity, param/return type changes | Change*Type, Add/Remove Parameter |
| T5 | **Structural split/merge** | 1→N extract, N→1 inline/merge | Godfrey/Zou, Extract/Inline/Merge Method |
| T6 | **Semantic near-duplicate** | High embedding/clone similarity without structural proof | Clone detection, CodeBERT |
| T7 | **Truly new / truly deleted** | No admissible match above threshold | Residual unmatched sets |

**Principle for nudox:** Emit a lineage edge only when evidence supports a tier; attach **confidence** and **tier label**. Downstream consumers (vector re-embed skip, TerminusDB queries, UI history) filter by confidence.

### 1.3 Cost asymmetry

Entity-resolution and code-matching communities agree: when history is written once and read many times, **false merges are worse than false splits**.

- A false merge links unrelated symbols → polluted embeddings reuse, wrong API evolution edges, corrupted TerminusDB lineage.
- A false split creates a "new" symbol and orphans history → recoverable via later re-matching or UI "maybe related" suggestions; does not poison the graph.

Operational translation: raise auto-link thresholds; allow multi-candidate soft edges with lower confidence; never force 1:1 assignment when best score is marginal.

---

## 2. Origin analysis (foundational)

### 2.1 Godfrey & Zou — Using origin analysis to detect merging and splitting (IEEE TSE 2005)

**Core idea:** Track entities across revisions when names fail by combining multiple *matchers*, then reason about call-graph deltas to detect merge/split.

**Matchers used in the Beagle/origin-analysis line (as summarized by successors):**
1. **Name matcher** — lexical name similarity
2. **Declaration matcher** — signature / header similarity
3. **Metrics matcher** — size/complexity fingerprints
4. **Call relation matcher** — callers/callees set similarity

**Human-in-the-loop:** Classic origin analysis surfaces top-\(k\) candidates; a human picks the best. Full automation was not the primary claim.

**Merge/split detection:** Extended origin analysis reasons about how call relationships change when entities disappear/appear — if callees of deleted \(f\) appear partitioned among new \(g_1,g_2\), that supports split; reverse supports merge.

**URLs:**
- PDF: https://plg.uwaterloo.ca/~migod/papers/2003/wcre03-tseVersion.pdf
- SWAG page: https://swag.uwaterloo.ca/publications/using-origin-analysis-to-detect-merging-and-splitting-of-source-code-entities.html
- Precursor (Tu & Godfrey IWPC 2002 "integrated approach"): often cited as origin of the term *origin analysis*

**Nudox takeaway:** Call/reference edges in the IR are first-class matching features, not optional extras. Merge/split is a first-class outcome type, not an error case.

### 2.2 Kim, Pan & Whitehead — When functions change their names (WCRE 2005)

**Most quantitative early automation paper for function origin detection.**

#### Algorithm
1. Extract consecutive revisions \(r_1, r_2\).
2. Build candidate set \(D \times A\) (deleted × added under name identity).
3. For each pair, compute **eight similarity factors**.
4. Weighted sum → total similarity.
5. Threshold → origin relationship yes/no.

#### Eight similarity factors (Table 1 in paper)

| Factor | Description | Similarity metric |
|--------|-------------|-------------------|
| Function name | Lexical name | ISC / LCSC |
| Incoming call set | Callers | ISC / LCSC |
| Outgoing call set | Callees | ISC / LCSC |
| Signature | Return type + arguments | ISC / LCSC |
| Function body diff | Line-based text diff (ignore whitespace) | Diff ratio |
| Complexity metrics | LOC, cyclomatic, in/out calls, nesting | Vector distance |
| MOSS | Fingerprint similarity | MOSS score |
| CCFinder | Token clone similarity | CCFinder score |

ISC (Intersection Set Count) similarity for sets \(A,B\):
\[
S_{AB} = \frac{1}{2}\left(\frac{|A \cap B|}{|A|} + \frac{|A \cap B|}{|B|}\right)
\]

LCSC (Longest Common Subsequence Count) preserves order (important for names).

Total similarity:
\[
ts = \frac{\sum_{i \in F} w_i S_i}{\sum_{i \in F} w_i}
\]

#### Oracle construction
- 10 human judges, ≥70% agreement → oracle pair.
- Projects: Subversion revs 1–3500; Apache2 revs 1–2000.
- Inter-rater agreement: **84.8%** (Apache2), **89.1%** (Subversion).
- Oracle sizes: 91 origins (Apache2), 626 (Subversion).

#### Accuracy results
| Setting | Accuracy |
|---------|----------|
| Apache2 (all factors, tuned) | **87.8%** |
| Subversion | **91.1%** |
| Avg human inter-rater | 84.8% / 89.1% |
| Trained weights, held-out periods | 83.7% train / **96.8%** test A / 86.5% test B |

**Factor significance (accuracy of each factor alone, Apache2 / SVN):**
- Body diff: 86.2% / **95.1%** (strongest)
- Outgoing calls: 86.5% / 89.5%
- Name: 78.1% / 90.6%
- Signature: 76.4% / 87.4%
- MOSS: 79.5% / 83.3%
- Incoming calls: 71.4% / 81.9%
- Complexity metrics: 46.3% / 85.7% (unstable)
- CCFinder: 61.3% / 75.1% (weakest)

**Key finding:** *Adding more factors does not necessarily improve accuracy.* Best Apache2 combination (body + name + sig + complexity) hit **91.0%**; full total-similarity ranked only #50 at 87.8%.

**Thresholds:** Best global threshold ≈ **0.52–0.60** depending on project (0.6 Apache2, 0.52 SVN; train-period best 0.47).

Source: https://users.soe.ucsc.edu/~ejw/papers/kim-wcre2005.pdf

**Nudox takeaway:**
- Body + outgoing refs + name dominate.
- Complexity metrics alone are weak/unstable — do not use as primary.
- Clone-detector scores (CCFinder/MOSS) are useful but secondary.
- Weights trained on one project transferred reasonably to other periods of the same systems.

### 2.3 Incremental origin analysis of source code files (MSR 2014)

Later work optimizes origin analysis for file-level incremental computation across large histories rather than pairwise full recompute. Relevant for nudox generation-to-generation batch matching at scale.  
ACM: https://dl.acm.org/doi/10.1145/2597073.2597111

---

## 3. Entity mapping / design differencing

### 3.1 UMLDiff (Xing & Stroulia, ASE/TSE era ~2005)

**Input:** Two reverse-engineered class models (packages, classes, interfaces, fields, methods).  
**Strategy:** Top-down structural matching using:
- **Name similarity** (lexical)
- **Structure similarity** (shared relationships: containment, inheritance, usage)

Renames inferred when entities share structural neighborhoods despite name change. After matching, refactorings are inferred (move class, rename method, etc.).

**Properties:**
- Hierarchical: match packages → classes → members.
- Lexical + structural hybrid (same spirit as origin analysis).
- Evaluated on real systems including JFreeChart in related work.

Semantic Scholar: https://www.semanticscholar.org/paper/UMLDiff:-an-algorithm-for-object-oriented-design-Xing-Stroulia/cdd9f62998fd6d3b0feecc34d2738e0d77c6006e

VTracker (Tsantalis line) later compared domain-independent tree differencing against UMLDiff on JFreeChart successive versions: https://users.encs.concordia.ca/~nikolaos/projects.html

**Nudox takeaway:** Match **containers first** (modules/packages/types), then members. Shared reference neighborhoods disambiguate renames. Hierarchical matching reduces combinatorial candidate sets.

### 3.2 AURA — Hybrid approach to identify framework evolution (ICSE 2010)

Wu, Guéhéneuc, Antoniol, Kim: maps old→new API methods for framework evolution (replacement recommendations when APIs disappear).

**Hybrid signals:** call dependency analysis + text similarity; handles many-to-one replacements better than pure call-based predecessors (e.g., earlier Kim et al. work).

**Reported relative gains:** average **recall +53.07%** vs prior approaches while precision stays approximately equal (~0.10% lower).  
ACM: https://dl.acm.org/doi/10.1145/1806799.1806848  
PDF: https://web.cs.ucla.edu/~miryung/Publications/icse10-aura.pdf

**Nudox takeaway:** When symbols vanish, search for **replacement** mappings (possibly N:1), not only rename. Hybrid call+text beats either alone for API evolution.

### 3.3 Other entity-mapping systems (brief)

| System | Approach | Notes |
|--------|----------|-------|
| RefactoringCrawler | Shingles fingerprint + semantic refs + iterative fixpoint | Top-down refactoring discovery |
| ChangeDistiller | AST edit ops → change types | Fine-grained, not high-level refactorings alone |
| LSDiff / rule-based (M. Kim) | Systematic change rules | Pattern-level, not single-entity identity |
| HiMa / later API mappers | Multi-feature | Higher precision variants post-AURA |

REF-FINDER (ICSM 2010) surveys and extends this space: https://web.cs.ucla.edu/~miryung/Publications/icsm10-reffinder.pdf

---

## 4. Tree differencing (AST / CST element mapping)

### 4.1 Tree edit distance foundations

**Zhang–Shasha (1989):** Classical tree edit distance, roughly \(O(n^4)\) worst-case classic bound; practical improvements exist. Too expensive for large ASTs without heuristics.

**GumTree insight:** Avoid full TED; use **greedy top-down isomorphic subtree matching** + bottom-up recovery with a max-size hyperparameter.

### 4.2 GumTree (Falleri et al., 2014) and ecosystem

**GumTree** is the de-facto language-independent AST differencing engine used by research tools (including RefactoringMiner internals historically, Spoon, etc.).

GitHub: https://github.com/GumTreeDiff/gumtree

**Algorithm sketch:**
1. **Top-down phase:** Greedy largest isomorphic subtree mappings (anchors).
2. **Bottom-up phase:** Extend mappings using structural context; limited by *maximum size threshold* hyperparameter for recovery of unmatched subtrees.
3. Edit script: insert, delete, update, move.

**Known accuracy issues (hyperparameter / mapping quality studies):**
- GumTree, MTDiff, IJM produce **inaccurate mappings for 20–29%, 25–36%, 21–30%** of file revisions respectively (reported across DAT / fine-grained differencing papers).  
  Sources: https://arxiv.org/pdf/2011.10268 · https://hal.science/hal-04423080/document · https://hal.science/hal-04855170v1/document

### 4.3 Successors and variants

| Tool | Contribution | Reference |
|------|--------------|-----------|
| **MTDiff** (Dotzler & Philippsen, 2016) | Move-optimized heuristics refining GumTree | Move-focused accuracy improvements |
| **IJM** (Frick et al.) | More accurate/compact edit scripts; partial Java pruning | Often second-best after domain-specific tools |
| **DAT** (Martinez, Falleri, Monperrus) | Hyperparameter auto-tuning for GumTree | Shorter edit scripts in ~18–22% of cases; https://arxiv.org/pdf/2011.10268 |
| **GumTree simple / ICSE 2024** (Falleri & Martinez) | Less expensive heuristic; scales to large ASTs; better quality than original on 4 datasets / 2 languages | https://hal.science/hal-04855170v1/document · ICSE 2024 |
| **SAT-DIFF** (2024) | SAT-solving for tree diffs | More concise edit scripts than GumTree (~1.72× fewer edits avg vs GumTree in reported comparison); https://arxiv.org/pdf/2404.04731 |
| **RefactoringMiner AST Diff** (Alikhanifard & Tsantalis TOSEM/related) | Refactoring- and semantic-aware mappings; multi-mapping support | +6.4% precision / +24% recall vs second-best (MTDiff) on dedicated AST mapping benchmark; multi-mapping P≈99.7% R≈98.4% vs competitors R<11% |
| **DiffSitter-adjacent** | tree-sitter CST differencing | Language-agnostic CST ops over tree-sitter — natural fit for multi-language platforms |
| **HyperDiff** (2025) | Stability of GumTree variants under HyperDiff | https://pure.tudelft.nl/admin/files/247281336/Evaluating_Stable_Tree_Differencing_with_Gumtree_and_HyperDiff.pdf |
| **SoliDiffy** (2024) | Solidity-specific AST diff | Domain adaptation of GumTree ideas |

### 4.4 How AST diff yields element-level mappings

Given trees \(T_{old}, T_{new}\):
- Mapped declaration nodes → candidate same-entity pairs at method/type/field granularity.
- Unmapped old → delete candidates; unmapped new → insert candidates.
- Move actions on declaration subtrees → move detection.
- Multi-mapping (one node ↔ many) needed for extract/inline — classical GumTree is weak here; RefactoringMiner-style semantic matching is strong.

**Complexity note:** Practical GumTree is near-linear to low-quadratic on typical files due to greedy hashing of subtrees; full TED is not used.

**Nudox takeaway:**
- For **body-level** identity evidence, prefer **normalized token / statement sequences** from tree-sitter over raw text.
- Full GumTree-over-whole-package is expensive and 20%+ mappings can be wrong — use AST/CST diff **inside candidate pairs** or **same-file**, not as global bipartite matcher over entire packages.
- If multi-mapping (extract/inline) matters, use refactoring-aware matchers, not vanilla GumTree.

---

## 5. Refactoring detection as identity machinery

Refactoring detectors are **specialized origin analyzers** with labeled edge types (Rename Method, Move Class, Extract Method, …). For lineage, the *mapping* they produce is more valuable than the refactoring label — but labels are excellent confidence boosters.

### 5.1 RefactoringMiner (Tsantalis et al.) — state of the art (verified 2026-06-29)

**Repository:** https://github.com/tsantalis/RefactoringMiner  
**Accuracy doc:** https://github.com/tsantalis/RefactoringMiner/blob/master/documentation/accuracy.md

**Capabilities (2026):**
- 100+ refactoring types (rename/move/extract/inline/split/merge/change-type/annotations/…).
- Statement-level matching with **multi-mapping**.
- Language expansion: Java (native), Python, Kotlin (validated oracles); C++ via RefactoringMiner++ in related work.
- Since v3.0: improved statement-level mappings.

#### Java Benchmark 1 (Tsantalis/Ketkar/Dig TSE 2022 oracle, extended)

- 547 commits, 188 projects  
- **As of 2026-06-29: Total Precision = 0.999, Recall = 0.984** (TP=12706, FP=13, FN=211)

Selected types critical for symbol identity:

| Type | Precision | Recall |
|------|-----------|--------|
| Rename Method | 0.995 | 0.955 |
| Move Method | 0.992 | 0.987 |
| Move And Rename Method | 1.000 | 0.970 |
| Rename Class | 1.000 | 0.966 |
| Move Class | 1.000 | 0.996 |
| Extract Method | 0.999 | 0.981 |
| Inline Method | 1.000 | 0.992 |
| Extract And Move Method | 1.000 | **0.696** (hard) |
| Split Method | 1.000 | 1.000 |
| Merge Method | 1.000 | 1.000 |
| Change Return Type | 1.000 | 0.974 |
| Add Parameter | 0.999 | 0.999 |
| Remove Parameter | 1.000 | 1.000 |

#### Java Benchmark 2 (Liu et al. TSE 2025 oracle, re-validated)

- 400 commits, 20 projects, commits ≥ 2024-03-28  
- **Total P=0.989, R=0.979**  
- Move And Rename Method: P=0.897 R=1.000 (precision dip on hard combined moves)

#### Python Benchmark (PyRef-origin, re-validated)

- **Total P=0.996, R=0.997**  
- Rename Method: P=1.000 R=0.993

#### Kotlin Benchmark

- **Total P=0.998, R=1.000**

**TSE 2022 paper headline (RefactoringMiner 2.0):** average precision **99.6%**, recall **94%** on then-current oracle (7,226 true instances, 40 types) — https://www.computer.org/csdl/journal/ts/2022/03/09136878/1liqhds60s8

**Why it works (transferable ideas):**
1. Statement-level abstraction (not raw tokens only).
2. **Replacement** matching for bodies under rename.
3. Multi-mapping for extract/inline (one method's statements map to many locations).
4. Refactoring rules as constraints on the match (semantic compatibility of AST node kinds).
5. Iterative detection: find easy renames first, use them to unlock harder moves.

**Language-agnostic transfer:** The *statement matching* and *signature abstraction* ideas transfer to any IR with:
- normalized statement/token sequences
- typed signatures
- reference graphs

The Java-specific AST visitors do **not** transfer; nudox should reimplement matching over its shared IR, not shell out to Java-only RMiner for non-Java packages.

### 5.2 RefDiff 2.0 (Silva & Valente, TSE 2020)

**Multi-language by design** via language plugins over a common code structure model.

- Java: **precision ~96%, recall ~80%** (competitive with specialized Java tools of that era)
- JavaScript / C plugins: precision and recall **88–91%**

PDF: https://homepages.dcc.ufmg.br/~mtov/pub/2020-tse-refdiff.pdf

**Architecture lesson for nudox:** Separate **language frontend → normalized code structure** from **refactoring/matching core**. RefDiff's model (types, functions, relationships) is close to a shared IR.

### 5.3 RefDetect (string alignment, multi-language)

- 27 refactoring types; Java + C++.
- Java: overall **P≈91%, R≈85%**; f-score **87.3%** vs RMiner 86% on their 514-commit / 5,058-instance study (method-level sometimes better than RMiner in that paper's setting).
- C++: **P=96.1%, R=94.1%**

ResearchGate: https://www.researchgate.net/publication/352142435_RefDetect_A_Multi-Language_Refactoring_Detection_Tool_Based_on_String_Alignment

**Technique:** String alignment on normalized code representations — aligns well with tree-sitter token streams.

### 5.4 RefactoringMiner++ (C++, ~2025)

Extends RMiner ideas to C++; reported high precision/recall in evaluation literature (see RMiner ecosystem references). Validates that statement-matching approach is not Java-intrinsic.

### 5.5 LLM-based refactoring detection (2024–2025)

Emerging work (e.g., RefModel, MANTRA-adjacent) uses foundation models for refactoring classification. Useful as **secondary validation**, not primary identity at scale: cost, non-determinism, and weaker precision guarantees than RMiner-class tools. Prefer classical high-precision matchers for lineage writes; LLMs for hard residual cases offline.

---

## 6. Method history trackers (code element histories)

These tools answer: *given a method at commit C, what is its full evolution history?* They are the closest systems to "symbol lineage across versions."

### 6.1 CodeShovel (Grund et al., ICSE 2021)

- On-demand method history without full-repo preprocessing.
- **String similarity** (Jaro-Winkler) for body/signature matching.
- Reported: complete and accurate histories for **~90% of methods** (including **97% of all method changes**); beat FinerGit, IntelliJ, git log.  
  Paper: https://www.cs.ubc.ca/~rtholmes/papers/icse_2021_grund.pdf

**Known weaknesses (later literature):**
- Struggles when methods undergo large body changes concurrent with moves.
- Case-insensitive matching can mis-handle string literals.
- Ignores merge commits, JavaDoc, annotations, formatting (by design in original).
- Oracle later found to have significant errors (see HistoryFinder).

### 6.2 CodeTracker (Jodavi & Tsantalis, ESEC/FSE 2022)

- Uses **RefactoringMiner entity matching** for refactoring-aware tracking.
- **Method tracking: 99.9% precision and 99.9% recall** on their corrected oracle.  
  PDF: https://users.encs.concordia.ca/~nikolaos/publications/FSE_2022.pdf  
  ACM: https://dl.acm.org/doi/10.1145/3540250.3549079
- Later: block tracking (for/if/while/try/…) with **~99.5%** P/R average (refactoring-aware block tracking TSE line).
- Tradeoff: slower than CodeShovel due to RMiner dependency.

GitHub: https://github.com/jodavimehran/code-tracker

### 6.3 HistoryFinder (Islam et al., 2025 — arXiv 2507.14716)

**Motivation:** Prior oracles (CodeShovel, CodeTracker) contain substantial inaccuracies when re-validated by multi-tool union + dual expert review (~300 hours × 2 authors).

**Oracle construction:**
- Union of commits from CodeShovel, CodeTracker, IntelliJ, GitFuncName, GitLineRange.
- Two experts remove FPs and add missed commits.
- Corrected CodeShovel oracle: 200 methods; HistoryFinder oracle: 200 methods from 20 popular Java projects.

**Oracle drift vs original CodeShovel:**
- 40 methods modified (excl. annotations/docs); 59 commits added, 186 removed.
- Including annotations/JavaDoc/formatting: 142 methods changed, 429 commits added.

**Algorithm (highly relevant to nudox):**
1. Build file-level DAG via `git log <commit> <file>`.
2. BFS ancestors.
3. Match in parent by **exact signature** first.
4. Else **body similarity ≥ 0.70** (Jaro-Winkler) same file.
5. Else search other changed files with body similarity **≥ 0.75**.
6. Recursive history on move/rename of file.
7. Include merge commits; track annotations, JavaDoc, formatting.

**Results:** HistoryFinder achieves highest overall F1 vs CodeShovel, CodeTracker, IntelliJ, Git baselines across new oracles; competitive runtime (often best mean/median among research tools).  
Source: https://arxiv.org/html/2507.14716

**Nudox takeaway (critical):**
- Generation-to-generation matching can use the same cascade: **signature exact → body ≥0.70 → cross-path body ≥0.75**.
- Thresholds 0.70/0.75 are literature-validated starting points for Jaro-Winkler-style string similarity on method text.
- **Do not trust git log -L alone** — research tools dominate Git/IDE baselines substantially.

### 6.4 FinerGit / Historage

- Preprocess repo into method-per-file fine-grained git history.
- High storage/time overhead; FinerGit improves small-method rename/move detection vs Historage.
- Less suitable for on-demand package-generation matching; interesting for offline full-history builds.

### 6.5 CodeMapper / region mapping (2024–2025)

Code region ↔ commit mapping approaches report exact-match rates **71–94.5%**, precision **76–97%**, recall **78–98%** depending on dataset (CodeTracker methods subset often easiest). Context windows (~15 lines) help. Useful for statement/region lineage if nudox ever tracks below symbol granularity.

---

## 7. Clone genealogy and provenance as identity proxies

### 7.1 Clone genealogies (Kim, Notkin, et al.)

**An Empirical Study of Code Clone Genealogies** (ESEC/FSE 2005) and **Using a clone genealogy extractor** (MSR 2005):

- Define clone evolution model: clone groups linked across versions by text/token similarity.
- Extract genealogies: how clone lineages form, persist, diverge, die.
- PDF: https://web.cs.ucla.edu/~miryung/Publications/esecfse05-clonegenealogy.pdf

**Identity proxy:** If code fragment \(c\) in \(G_n\) is a Type-1/2 clone of fragment \(c'\) in \(G_{n-1}\) *and* structural context aligns, treat as same lineage with clone-confidence.

**Caution:** Clones can be **coincidental** (similar boilerplate) or **intentionally duplicated then diverged**. Clone similarity alone must not auto-merge distinct symbols in the same generation; across generations within one package's evolution, clone signals are stronger when combined with path/call context.

### 7.2 Software Heritage and SWHID (content identity)

Software Heritage defines **SWHID** (SoftWare Hash IDentifier) — intrinsic, content-addressed identifiers. Became **ISO/IEC 18670** (2025-04-23).

- Levels: content, directory, revision, release, snapshot; fragment qualifiers.
- https://docs.softwareheritage.org/devel/swh-model/persistent-identifiers.html
- https://www.softwareheritage.org/2025/06/13/software-hash-identifier-swhid-tutorial/
- https://swhid.org/

**What SWHID gives nudox:**
- **T0 identity:** same normalized content → same hash → definitely same artifact.
- Provenance across archives/packages.

**What SWHID does NOT give:**
- Rename continuity (content hash changes when body changes even one character).
- Symbol-level identity inside a file (unless fragment SWHIDs are maintained carefully).

**Recommendation:** Store content hashes of normalized symbol bodies as **immutable fingerprints** for T0 short-circuit and dedup; never confuse content hash equality with the only form of identity.

### 7.3 Git rename detection

Git's rename/copy detection uses similarity index (default **50%** of file). Configurable via `-M`/`-C` and `diff.renames`. This is a **file-level** heuristic, not symbol-level, but provides weak prior: if git believes file A renamed to B, boost path-mapping priors for symbols inside.

---

## 8. Fingerprinting and near-duplicate detection

### 8.1 Winnowing / MOSS

**Winnowing** (Schleimer, Wilkerson, Aiken, SIGMOD 2003): local algorithms for document fingerprinting — select k-gram hashes robustly. MOSS uses fingerprint overlap for plagiarism/similarity.

Used as a factor in Kim et al. 2005 origin detection (MOSS alone: ~80% accuracy as origin predictor — good but not best).

### 8.2 SimHash / MinHash / LSH

- **SimHash** (Charikar): Hamming distance approximates cosine similarity; excellent for near-duplicate retrieval.
- **MinHash + LSH:** Sublinear candidate retrieval for large symbol sets.

HistoryFinder authors experimented with SimHash for method matching but found **Jaro-Winkler gave better recall** on their oracles. Still, SimHash is ideal for **blocking** (candidate generation) before expensive pairwise scoring.

### 8.3 SourcererCC (Sajnani et al., ICSE 2016)

Token-based Type-1/2/3 clone detector scalable to big code via inverted index + filtering.  
arXiv: https://arxiv.org/abs/1512.06448 · https://cs.uwaterloo.ca/~m2nagapp/courses/CS846/1171/papers/sajnani_icse16.pdf

Typical operating point: **~80% token overlap** threshold (configurable). Strong for near-duplicate body matching at scale.

### 8.4 Normalized AST / Merkle hashing

Structural fingerprints:
1. Parse to AST/CST.
2. Normalize (strip comments, normalize identifiers optionally for Type-2, sort unordered children).
3. Merkle-hash subtrees bottom-up.

**Exact structural identity** if roots match. **Partial** if large subtrees share hashes (similar to GumTree anchors). Useful T0/T1 structural short-circuit independent of source formatting.

### 8.5 API / binary compatibility tools (signature identity)

- **japicmp**, **Revapi**: Java binary API diffs.
- **Roseau** (ICSME 2025 area): source-based API break detection; reported **F1≈0.99** vs JApiCmp ~0.86, Revapi ~0.91.

These tools answer "is the API surface compatible?" not "is this the same symbol after rename." Useful for **signature-evolution tier** validation when FQN matches.

---

## 9. Embedding-based matching (neural code similarity)

### 9.1 Model lineage

| Era | Models | Use |
|-----|--------|-----|
| 2018–2020 | code2vec, code2seq | Path-based method embeddings |
| 2020–2022 | CodeBERT, GraphCodeBERT | NL-PL pretraining; clone detection / search |
| 2022–2024 | UniXcoder, CodeT5, CodeT5+ | Unified encoder; strong clone & search |
| 2024–2026 | Larger code LLMs; LoRA adapters on UniXcoder/GraphCodeBERT | Fine-tuned similarity |

### 9.2 Reliability for *identity*

Neural models shine at **semantic similarity** (Type-3/4 clones, cross-language analogs). That is a **different problem** from cross-version identity:

- Two different utility functions can embed closely (`isEmpty` vs `isBlank`).
- A heavily refactored function may embed far from its origin if control flow changed.
- Temperature/finetune variance makes hard thresholds brittle.

**Literature consensus for operational systems:** use embeddings as **tie-breakers** and **candidate generators**, not as sole auto-merge signals.

**Suggested use in nudox:**
1. Precompute embedding for each symbol body (already needed for vector search).
2. For unmatched residuals after structural tiers, retrieve top-k by embedding cosine.
3. Accept auto-link only if cosine ≥ high threshold **AND** at least one structural feature agrees (kind match, partial name, shared callees, doc similarity).
4. Otherwise emit **soft** edge `maybe_same` with low confidence for UI/research, not for automatic embed-skip.

Empirical clone-detection F1 for GraphCodeBERT/UniXcoder is high on BigCloneBench-style tasks, but those tasks are not version-identity oracles. Do not transfer clone F1 numbers as lineage precision.

---

## 10. Bipartite matching formulation

### 10.1 Problem as assignment

Given unmatched deleted set \(D\) and added set \(A\), build similarity matrix \(S_{ij} = \mathrm{sim}(d_i, a_j)\).

**1:1 assignment:** Hungarian algorithm / Jonker-Volgenant / min-cost max-flow maximize \(\sum S_{ij} x_{ij}\) subject to row/column constraints.

**Thresholded assignment:** Only allow edges with \(S_{ij} \ge \tau\); unmatched remain new/deleted.

**Stable matching (Gale-Shapley):** Prefer when similarity is asymmetric or when agents have preference lists; less common in code literature than thresholded Hungarian.

### 10.2 Preferring false-split over false-merge

Strategies from entity resolution + code papers:

1. **High \(\tau\)** for auto-merge (e.g., 0.85–0.95 composite score).
2. **Margin requirement:** best match must beat second-best by \(\delta\) (e.g., 0.05–0.10) to avoid ambiguous pairs.
3. **Kind hard filter:** never match function ↔ type, method ↔ field.
4. **One-sided greedy:** sort edges by score descending; accept if both endpoints free and \(S \ge \tau\); never reassign.
5. **Partial matches:** allow 1:N for extract (split) and N:1 for inline/merge only when refactoring-style evidence exists (shared statement multi-map, coverage thresholds).

### 10.3 Split/merge (1:N, N:1)

Godfrey & Zou treat split/merge via call-structure reasoning.  
RefactoringMiner detects Extract Method (1:N statements), Inline Method (N:1), Split/Merge Method/Class explicitly with high precision.

**Coverage-based multi-match (recommended):**
- For candidate split: body of deleted \(d\) approximately equals union of bodies of \(\{a_1..a_k\}\) (token multiset coverage ≥ 0.8) and each \(a_i\) has high overlap with some partition of \(d\).
- For merge: reverse.

Do **not** use pure bipartite 1:1 solvers for split/merge; use dedicated multi-map or refactoring rules.

### 10.4 Composite similarity function (recommended shape)

\[
\begin{align}
\mathrm{sim}(x,y) =\;
& w_n\,\mathrm{name}(x,y) + w_s\,\mathrm{sig}(x,y) + w_b\,\mathrm{body}(x,y) \\
& + w_r\,\mathrm{refs}(x,y) + w_d\,\mathrm{doc}(x,y) + w_p\,\mathrm{path}(x,y)
\end{align}
\]

Starting weights (inspired by Kim 2005 significance + modern trackers):

| Feature | Weight | Metric |
|---------|--------|--------|
| Body tokens | 0.35 | Jaccard / cosine TF-IDF / Jaro-Winkler on normalized text |
| Name (unqualified) | 0.20 | Jaro-Winkler or LCSC |
| Signature | 0.15 | Param type multiset + return type |
| Reference edges | 0.15 | Jaccard on callee/caller symbol IDs (resolved within package) |
| Path/container | 0.10 | Shared path prefix length / module distance |
| Doc text | 0.05 | TF-IDF cosine |

Calibrate on a held-out set of packages with known renames.

---

## 11. Evaluation landscape and achievable precision by tier

### 11.1 Ground-truth datasets

| Dataset / Oracle | Granularity | Notes |
|------------------|-------------|-------|
| Kim et al. 2005 human oracle | Function origins | SVN, Apache2; 10 judges |
| RefactoringMiner Java Benchmark 1 | Refactoring instances | 547 commits, 188 projects; continuously extended |
| RefactoringMiner Java Benchmark 2 | Refactoring instances | 400 commits from 2024+ |
| RefactoringMiner Python / Kotlin | Refactoring instances | Cross-language validation |
| CodeShovel oracle (2021) | Method histories | Later found flawed |
| CodeTracker oracle (2022) | Method histories | Corrected CodeShovel; still imperfect |
| HistoryFinder oracles (2025) | Method histories | 400 methods; multi-tool union + dual experts |
| AST mapping benchmark (Alikhanifard/Tsantalis) | AST node maps | 800 bug-fix + 188 refactoring commits |
| BigCloneBench / BigCloneBench variants | Clones (not identity) | Do not misuse as lineage GT |

### 11.2 Literature-grounded precision by identity tier

These are **order-of-magnitude operational expectations** synthesized from the papers above, not a single unified benchmark:

| Tier | Expected auto-link precision | Expected recall | Evidence basis |
|------|------------------------------|-----------------|----------------|
| T0 Content hash equal | **~100%** | low (only identical bodies) | Definitional |
| T1 FQN + kind equal | **~99–100%** | high for stable APIs | Symbol table; fails on rename |
| T2 Rename (RMiner-class) | **~99.5%** | **~95–99%** | RMiner Rename Method 0.995/0.955; higher with body confirm |
| T3 Move (+ optional rename) | **~99%** | **~97–99%** | RMiner Move*; CodeTracker 99.9% method track |
| T4 Signature evolution (same FQN) | **~99%** | **~97–99%** | RMiner Change*Type / Add Parameter |
| T5 Extract/Inline/Split/Merge | **~99% P** when detected | **~70–99% R** by type | Extract And Move R can drop to ~0.70 |
| Body-only fuzzy (no name) @0.70 | **~90–97%** | moderate | HistoryFinder thresholds; Kim body factor 86–95% |
| Full origin composite (Kim 2005) | **~88–91%** | — | Automated vs human oracle |
| Human inter-rater ceiling | **~85–90%** | — | Kim 2005 judges |
| Embedding-only auto-merge | **unknown / unsafe** | — | No solid version-identity oracle; clone F1 ≠ identity P |
| git log -L / IntelliJ | substantially worse | substantially worse | HistoryFinder RQ2 |

**Interpretation:** With IR-quality features and a cascade like HistoryFinder + RMiner-style rules, **≥99% precision is achievable** for T1–T4 auto-links. T5 needs specialized multi-map. Fuzzy residual matching should be confidence-gated below auto-link threshold.

### 11.3 Why oracle quality matters

HistoryFinder shows that even "gold" method-history oracles can be wrong by hundreds of commits. For nudox internal evaluation:

1. Build oracles via **multi-signal union** (name, body, refs, optional RMiner-on-Java).
2. Dual review on sample.
3. Measure precision of auto-links with **asymmetric cost** (weight FP merges higher).
4. Track calibration: predicted confidence vs empirical precision.

---



---

## 11A. Worked example: applying the tier cascade

Consider two consecutive generations of a hypothetical Rust package `acme-http`.

### Generation \(G_{n-1}\) (selected symbols)

| ID | FQN | Kind | Notes |
|----|-----|------|-------|
| A | `acme_http::client::Client::send` | method | body 40 tokens |
| B | `acme_http::client::Client::send_with_timeout` | method | body 55 tokens |
| C | `acme_http::util::parse_headers` | function | body 80 tokens |
| D | `acme_http::util::normalize_url` | function | body 30 tokens |

### Generation \(G_n\) (selected symbols)

| ID | FQN | Kind | Notes |
|----|-----|------|-------|
| A′ | `acme_http::client::Client::send` | method | same body as A (whitespace only) |
| B′ | `acme_http::client::Client::request` | method | body ≈ B with rename + minor edit (JW 0.88) |
| C1 | `acme_http::util::parse_header_name` | function | extracted from C |
| C2 | `acme_http::util::parse_header_value` | function | extracted from C |
| E | `acme_http::util::join_url` | function | was D, moved+renamed, body JW 0.91 |
| F | `acme_http::middleware::Logger::log` | method | brand new |

### Expected decisions

| Pair | Tier | Edge | Confidence | Rationale |
|------|------|------|------------|-----------|
| A→A′ | T0/T1 | `identical` / `same_fqn` | 1.00 / 0.99 | content_hash equal + FQN equal |
| B→B′ | T2 | `renamed` | ~0.94 | same parent, body ≥0.70, name changed |
| C→{C1,C2} | T5 | `split` / `extracted_from` | ~0.88 | multiset coverage of C by C1∪C2 high |
| D→E | T3 | `moved_renamed` | ~0.93 | cross-parent body ≥0.75, unique best |
| F | T7 | birth | — | no match |
| (none← deleted leftover) | T7 | death | — | if any |

**False-merge trap:** If `join_url` had body similarity 0.72 to *both* D and some other deleted helper, margin rule rejects auto-link → soft edge only.

This example shows why **tier labels** matter for TerminusDB: UI can show "renamed to request", "split into parse_header_*", "moved from util::normalize_url", rather than a flat same-id.

---

## 11B. Feature availability matrix vs literature signals

| Literature signal | nudox IR field | Notes |
|-------------------|----------------|-------|
| Function name | `name_u`, FQN | Always available |
| Signature | normalized typed signature | Better than Kim 2005 raw C signatures if types resolve |
| Body text / tokens | tree-sitter normalized tokens | Prefer tokens over raw text for language noise |
| Incoming calls | `ref_in` | Requires intra-package resolution quality |
| Outgoing calls | `ref_out` | Same |
| Complexity metrics | derivable (LOC, depth) | Low priority per Kim 2005 |
| MOSS / winnowing | optional fingerprint service | Good for blocking, not primary |
| CCFinder / SourcererCC | optional | Overkill if token Jaccard available |
| AST node maps | tree-sitter CST ops | Local pairwise, not global GumTree |
| Docs | doc text | Weak alone; good boost |
| Embeddings | vector index | T6 tie-break only |
| Content hash | SHA of normalized body | T0 |
| Container/path | parent FQN | Hierarchy / move priors |

**Gap risk:** If reference resolution is incomplete for a language frontend, ref Jaccard becomes noisy — down-weight \(w_r\) per-language when resolver coverage is low.

---

## 11C. Comparison of body similarity metrics

| Metric | Strengths | Weaknesses | Literature use |
|--------|-----------|------------|----------------|
| Line diff ratio | Simple | Brittle to formatting/reorder | Kim 2005 body factor |
| Jaro-Winkler on full text | Good for renames of moderate size; HistoryFinder preferred over SimHash for recall | Expensive on huge methods; less token-aware | HistoryFinder, CodeShovel |
| Token Jaccard | Language-robust with tree-sitter; set semantics | Ignores order | Clone detectors, recommended primary |
| Token TF-IDF cosine | Down-weights common tokens | Needs corpus stats | IR/search systems |
| SimHash Hamming | Fast blocking | Weaker final ranking (HistoryFinder) | Candidate generation |
| Statement abstraction match | High precision for refactorings | Needs statement IR | RefactoringMiner |
| Embedding cosine | Semantic near-duplicates | False friends; model drift | T6 assist only |

**Recommendation:** Primary = **token Jaccard** on normalized tree-sitter tokens (identifiers kept for Type-1/2 identity; optional identifier-normalized variant for aggressive Type-3 residual only). Secondary = Jaro-Winkler for short methods where token sets are unstable. Blocking = SimHash 64-bit.

---

## 11D. Mapping literature algorithms → nudox components

| Component | Role | Inspired by |
|-----------|------|-------------|
| `hash_join_content` | T0 | SWHID / content addressing |
| `hash_join_fqn` | T1 | Symbol tables, API tools |
| `match_renames` | T2 cascade | HistoryFinder FindBySignature/Body; Kim factors |
| `match_moves_lsh` | T3 | HistoryFinder AltFileMatch; git rename prior |
| `annotate_sig_evolution` | T4 | RMiner Change*/Add Parameter taxonomy |
| `match_splits_merges` | T5 | Godfrey/Zou; RMiner Extract/Inline multi-map |
| `match_residual_bipartite` | T6 | Hungarian + entity-resolution thresholds |
| `confidence` payload | edge metadata | Operational requirement (not pure academic) |
| Hierarchical container match | optimization | UMLDiff top-down |
| Language plugins → IR | architecture | RefDiff multi-language model |

---

## 12. Cross-cutting design principles from the literature

1. **Cascade, don't blend blindly.** Signature match → body → cross-file → multi-map. HistoryFinder and origin analysis both use staged matching; Kim 2005 shows naive all-factor blends can lose to curated subsets.

2. **Hierarchy first.** UMLDiff-style: packages/modules → types → members. Reduces candidates and encodes move-class context.

3. **References are gold.** Incoming/outgoing call sets repeatedly rank among top features (Godfrey, Kim 2005, AURA, RMiner).

4. **Body similarity is the workhorse** for rename/move when names fail — but choose metric carefully (Jaro-Winkler / token Jaccard on normalized bodies beat raw metrics and sometimes SimHash for recall).

5. **Multi-mapping is mandatory** for extract/inline; classical 1:1 bipartite matching is insufficient.

6. **Language-agnostic core + language frontends** (RefDiff model) matches nudox IR strategy.

7. **Conservative thresholds for writes; soft edges for hints.** Entity-resolution practice + lineage corruption risk.

8. **Do not use clone/embedding F1 as lineage precision.** Different task.

9. **Content addressing complements, not replaces, origin analysis.**

10. **Re-validate on your IR.** Java-centric numbers (RMiner, CodeTracker) are upper bounds if IR loses statement fidelity.

---

## 13. Recommended nudox matching pipeline

### 13.1 Inputs available per symbol (assumed)

From the mission brief:
- Fully-qualified path (module/type/name)
- Kind (function, method, class, trait, module, …)
- Normalized signature from typed IR
- Doc text
- Normalized body tokens from tree-sitter
- Intra-package reference edges (calls, type uses, inherits)

**Additionally compute once per symbol:**
- `content_hash` = SHA256(normalized body tokens)
- `sig_hash` = hash(normalized signature)
- `name_u` = unqualified name
- `parent_fqn` = container FQN
- `ref_out` / `ref_in` = sets of resolved intra-package symbol IDs (or stable path keys)
- `embed` = vector (existing pipeline)
- `size` = token count (for thresholds that depend on size)

### 13.2 Pipeline overview

```
Generation G_{n-1} symbols  ×  Generation G_n symbols
                    │
                    ▼
         ┌─────────────────────┐
         │  Tier 0: Hash equal │  content_hash match, same kind
         └──────────┬──────────┘
                    ▼
         ┌─────────────────────┐
         │  Tier 1: FQN exact  │  fqn+kind match
         └──────────┬──────────┘
                    ▼
         ┌─────────────────────┐
         │  Tier 2: Same parent│  rename within container
         │  body/sig cascade   │
         └──────────┬──────────┘
         ┌─────────────────────┐
         │  Tier 3: Move       │  cross-parent body match
         └──────────┬──────────┘
         ┌─────────────────────┐
         │  Tier 4: Signature  │  same fqn, sig changed (already T1)
         │  evolution labels   │  or same body, sig evolved
         └──────────┬──────────┘
         ┌─────────────────────┐
         │  Tier 5: Split/Merge│  multi-map coverage
         └──────────┬──────────┘
         ┌─────────────────────┐
         │  Tier 6: Residual   │  bipartite + embed tie-break
         │  (soft edges)       │
         └──────────┬──────────┘
                    ▼
         Unmatched: birth (new) / death (deleted)
```

### 13.3 Tier specifications

#### Tier 0 — Content identity
- **Rule:** `kind` equal ∧ `content_hash` equal  
- **Algorithm:** Hash join  
- **Confidence:** `1.00`  
- **Edge type:** `identical`  
- **Expected precision:** ~100%  
- **Note:** May match unrelated clones if bodies are identical boilerplate — optional guard: require same `parent_fqn` OR unique body in both generations. Prefer requiring parent match for auto-link when duplicates exist.

#### Tier 1 — Fully-qualified identity
- **Rule:** `fqn` equal ∧ `kind` equal  
- **Algorithm:** Hash join on FQN  
- **Confidence:**  
  - `0.99` if `sig_hash` equal  
  - `0.97` if signature compatible (see T4)  
  - `0.95` if only FQN matches (body changed)  
- **Edge type:** `same_fqn` / `same_fqn_sig_changed`  
- **Expected precision:** ~99–100% for real packages (collisions only if publish process is broken)

#### Tier 2 — Rename within same container
- **Candidates:** Unmatched in \(G_{n-1}\) and \(G_n\) with same `parent_fqn` and same `kind`.  
- **Cascade:**  
  1. Exact `sig_hash` match among candidates (unique) → link conf `0.98`  
  2. Else body similarity \(B \ge 0.85\) → conf `0.96`  
  3. Else body \(B \ge 0.70\) ∧ (name JW ≥ 0.80 ∨ ref Jaccard ≥ 0.50) → conf `0.93`  
  4. Else body \(B \ge 0.70\) alone → conf `0.90` (auto-link only if unique best and margin ≥ 0.05)  
- **Body metric:** Prefer token-Jaccard on normalized tree-sitter tokens; Jaro-Winkler on full normalized text as alternative (HistoryFinder).  
- **Matching:** Greedy by score with uniqueness; Hungarian if dense collision.  
- **Expected precision:** ~95–99% at conf≥0.93 (aligned with RMiner Rename + HistoryFinder).

#### Tier 3 — Move (and move+rename)
- **Candidates:** Cross-parent unmatched pairs, same `kind`, blocked by SimHash/LSH or inverted token index to top-k=20.  
- **Rule:** Body similarity \(B \ge 0.75\) (HistoryFinder cross-file threshold)  
  - Boost +0.05 conf if `name_u` equal  
  - Boost if ref_out Jaccard ≥ 0.4  
  - Boost if git/file-level rename prior exists between parents  
- **Auto-link threshold:** composite ≥ `0.88`, margin ≥ `0.05`  
- **Confidence:** `min(0.97, 0.80 + 0.2*B + boosts)`  
- **Edge type:** `moved` / `moved_renamed`  
- **Expected precision:** ~97–99% at high threshold; lower threshold → soft edges only.

#### Tier 4 — Signature evolution (annotation on T1/T2 edges)
- When T1 matched but signature differs, classify:
  - add/remove/reorder parameter
  - change return/param types
  - async/throws/etc. modifiers
- **Confidence:** inherits T1; add payload `sig_delta`  
- **Edge type:** `signature_evolved`  
- Not a separate bipartite pass — a **labeler** on already matched pairs.

#### Tier 5 — Split / merge / extract / inline
- **Trigger:** Unmatched symbols where multi-coverage is high.  
- **Split (1→N):**  
  - Deleted \(d\), added set \(A^*\).  
  - Token multiset coverage: \(\mathrm{cover}(d, A^*) \ge 0.80\)  
  - Each \(a \in A^*\) has pairwise overlap with \(d \ge 0.25\) and better with \(d\) than with other deleted.  
  - Prefer when \(|A^*|\) small (2–4) and sizes sum ≈ size(d).  
- **Merge (N→1):** reverse.  
- **Confidence:** `0.85–0.95` depending on coverage; label `extracted_from` / `inlined_into` / `split` / `merged`.  
- **Expected precision:** high when coverage strict; recall incomplete (RMiner Extract And Move R~0.70 on hard cases) — **accept incomplete recall**.

#### Tier 6 — Residual bipartite (soft)
- Build similarity matrix for remaining unmatched with composite \(\mathrm{sim}\) (§10.4).  
- Retrieve embed neighbors cosine ≥ 0.85 as candidates.  
- Hungarian/greedy with \(\tau_{\mathrm{auto}} = 0.92\), \(\tau_{\mathrm{soft}} = 0.75\).  
- **Auto-link only if** \(\mathrm{sim} \ge 0.92\) ∧ kind match ∧ margin ≥ 0.08 ∧ embed cosine ≥ 0.80.  
- Else if \(\mathrm{sim} \ge 0.75\): emit `maybe_same` conf = sim, **do not** skip re-embed.  
- **Expected precision auto:** aim ≥95% via strict \(\tau\); soft edges untrusted for automation.

#### Tier 7 — Birth / death
- Remaining unmatched in \(G_n\): `added`  
- Remaining unmatched in \(G_{n-1}\): `removed`  
- No lineage edge (or explicit `terminus` marker in TerminusDB).

### 13.4 Confidence model on lineage edges

Store on every edge:

```json
{
  "from_id": "...",
  "to_id": "...",
  "tier": "rename|move|identical|...",
  "confidence": 0.0-1.0,
  "features": {
    "body_sim": 0.91,
    "name_sim": 0.45,
    "sig_sim": 1.0,
    "ref_jaccard": 0.72,
    "embed_cos": 0.88
  },
  "decision": "auto|soft|human",
  "matcher_version": "nudox-identity-1.0"
}
```

**Consumer policy examples:**
- **Skip re-embed** only if `confidence ≥ 0.95` ∧ body_sim ≥ 0.99 (essentially unchanged).  
- **Propagate TerminusDB stable identity** if `confidence ≥ 0.93` ∧ decision=auto.  
- **UI "renamed from"** if tier ∈ {rename, move} ∧ conf ≥ 0.90.  
- **Never** treat soft edges as hard identity for incremental skip.

### 13.5 Threshold starter table

| Parameter | Starter | Rationale |
|-----------|---------|-----------|
| Body auto same-parent | 0.70 | HistoryFinder |
| Body auto cross-parent | 0.75 | HistoryFinder |
| Body high-precision | 0.85 | Extra guard for rename-only |
| Name JW weak accept | 0.80 | With body support |
| Composite auto (residual) | 0.92 | Prefer false-split |
| Composite soft | 0.75 | UI hints |
| Margin (best−second) | 0.05–0.08 | Ambiguity rejection |
| Embed assist min | 0.80 | Tie-break floor |
| Split coverage | 0.80 | Multi-map |
| Ref Jaccard boost | 0.40–0.50 | Kim outgoing-call strength |

### 13.6 Algorithmic complexity controls

1. **Blocking:** Only compare symbols with same `kind`; optional same top-level module for T2.  
2. **LSH/SimHash** on body for T3/T6 candidate generation — avoid \(O(|D|·|A|)\) full Cartesian on large packages.  
3. **Hierarchical:** Match modules/types first; restrict member matching within mapped containers + small cross-container band for moves.  
4. **Incremental:** Match only symbols in files/modules that changed between package versions when file-level diff is available; still run global unmatched pass for moves across modules.

### 13.7 What NOT to do

- Do not shell out to Java-only RefactoringMiner for Rust/Go/Python as primary (reimplement IR-level matching; optionally use RMiner for Java packages as oracle/calibration).  
- Do not auto-merge on embedding cosine alone.  
- Do not use git similarity 50% as symbol threshold.  
- Do not require all similarity factors (Kim 2005: more is not always better).  
- Do not treat Type-3 clone hits across the package as identity without uniqueness checks.

---

## 14. Implementation sketch (nudox services)

```
fn match_generations(old: &[Symbol], new: &[Symbol]) -> Vec<LineageEdge> {
    let mut edges = vec![];
    let mut old_u = IndexSet::from(old);
    let mut new_u = IndexSet::from(new);

    // T0 + T1 hash joins
    edges.extend(hash_join_content(&mut old_u, &mut new_u));
    edges.extend(hash_join_fqn(&mut old_u, &mut new_u));

    // T2 per parent_fqn group
    for parent in all_parents(&old_u, &new_u) {
        edges.extend(match_renames(parent, &mut old_u, &mut new_u));
    }

    // T3 blocked move matching
    edges.extend(match_moves_lsh(&mut old_u, &mut new_u));

    // T5 multi-map split/merge
    edges.extend(match_splits_merges(&mut old_u, &mut new_u));

    // T6 residual soft/hard
    edges.extend(match_residual_bipartite(&mut old_u, &mut new_u));

    // T4 label signatures on existing edges
    annotate_sig_evolution(&mut edges, old, new);

    // births/deaths implicit in residuals
    edges
}
```

**Testing plan:**
1. Synthetic renames/moves/extracts on internal fixtures.  
2. Java subset: compare against RefactoringMiner mappings as pseudo-oracle.  
3. Manual audit of 100 random soft edges and 100 auto edges per language.  
4. Measure FP merge rate as primary KPI (target <1% on auto edges).

---

## 15. Open questions and risks

1. **IR fidelity:** If tree-sitter bodies lose macros/generics/comments unevenly across languages, body_sim calibration drifts. Mitigate with per-language golden tests and frozen normalizer versions on edges (`normalizer_version` field).  
2. **Overloaded names / multiple same-name methods:** Signature disambiguation mandatory; name-only tiers fail. Rank candidates by signature distance before body.  
3. **Generated code:** Identical generated bodies → T0 false merges across unrelated symbols; need parent+name guards and optionally a `generated: bool` IR flag to disable T0 cross-parent.  
4. **Partial publishes / yanked files:** Missing modules look like mass deletes; avoid aggressive T5 when >X% of package symbols vanish.  
5. **Cross-package identity:** Out of scope for generation matching but SWHID/content hash helps provenance products later.  
6. **Confidence calibration:** Starter thresholds need empirical Bayes calibration on real nudox packages; maintain a living confusion matrix dashboard.  
7. **Multi-generation transitive identity:** Matching only adjacent generations is correct; transitive closure should use conf product or min-conf along path. Never invent edges between non-adjacent generations without path evidence.  
8. **Parallel evolution (branches):** Package registry versions are usually linear; if not, model as DAG like HistoryFinder.  
9. **Embedding model drift:** When embed model versions change, T6 thresholds break — version the matcher and freeze embed model ID on edges.  
10. **Legal/license clone false friends:** Cross-repo identical functions should not share lineage IDs within a package graph.  
11. **Macro-heavy / template-heavy languages:** C++ templates, Rust macros, Zig comptime may expand differently than surface IR suggests — body identity may under-match; prefer signature+refs.  
12. **Anonymous / lambda / closure symbols:** Unstable names; either exclude from lineage or key by parent+ordinal+body hash.  
13. **Re-exports and type aliases:** "Same" API surface may be a re-export of a different underlying symbol — decide whether lineage follows **API identity** or **implementation identity** (recommend: implementation for embed/index; API alias edges separate).  
14. **Evaluation cost:** Building HistoryFinder-quality oracles is hundreds of human hours; budget continuous sampling audit (1% of auto edges/week) instead of one-shot perfect GT.  
15. **Performance SLOs:** Large monorepo-style packages with 10⁵ symbols need LSH blocking; full Hungarian on dense residuals may need approximation (greedy).  
16. **Adversarial / minified code:** Token streams may be meaningless; fall back to FQN-only tiers.  
17. **False precision from RMiner numbers:** RMiner is evaluated on commits that *contain* refactorings with expert oracles; package-registry version deltas may include large feature additions where residual matching is harder — expect lower recall, protect precision.

---

## 16. Related work quick reference (URLs)

| Topic | URL |
|-------|-----|
| Godfrey & Zou origin analysis PDF | https://plg.uwaterloo.ca/~migod/papers/2003/wcre03-tseVersion.pdf |
| Kim/Pan/Whitehead WCRE 2005 | https://users.soe.ucsc.edu/~ejw/papers/kim-wcre2005.pdf |
| Kim & Notkin MSR 2006 matching | https://web.cs.ucla.edu/~miryung/Publications/msr06-matching.pdf |
| AURA ICSE 2010 | https://web.cs.ucla.edu/~miryung/Publications/icse10-aura.pdf |
| Clone genealogies | https://web.cs.ucla.edu/~miryung/Publications/esecfse05-clonegenealogy.pdf |
| GumTree | https://github.com/GumTreeDiff/gumtree |
| DAT hyperparams | https://arxiv.org/pdf/2011.10268 |
| GumTree ICSE 2024 | https://hal.science/hal-04855170v1/document |
| SAT-DIFF | https://arxiv.org/pdf/2404.04731 |
| RefactoringMiner | https://github.com/tsantalis/RefactoringMiner |
| RMiner accuracy (2026-06-29) | https://github.com/tsantalis/RefactoringMiner/blob/master/documentation/accuracy.md |
| RefDiff 2.0 PDF | https://homepages.dcc.ufmg.br/~mtov/pub/2020-tse-refdiff.pdf |
| CodeTracker FSE 2022 | https://users.encs.concordia.ca/~nikolaos/publications/FSE_2022.pdf |
| CodeShovel ICSE 2021 | https://www.cs.ubc.ca/~rtholmes/papers/icse_2021_grund.pdf |
| HistoryFinder 2025 | https://arxiv.org/html/2507.14716 |
| SourcererCC | https://arxiv.org/abs/1512.06448 |
| SWHID spec | https://docs.softwareheritage.org/devel/swh-model/persistent-identifiers.html |
| SWHID tutorial 2025 | https://www.softwareheritage.org/2025/06/13/software-hash-identifier-swhid-tutorial/ |
| Matching ICSE 2007 (Dagenais et al. related) | https://homes.cs.washington.edu/~djg/papers/matching_icse07.pdf |

---

## 17. Executive summary (in-document)

**Question:** When are two symbols across package generations "the same"?

**Answer:** Identity is a **tiered, evidence-graded relation**, not a boolean name equality. The academic literature—from Godfrey & Zou origin analysis through Kim et al. function mapping, UMLDiff, AURA, GumTree-family AST differencing, RefactoringMiner/RefDiff/RefDetect, CodeShovel/CodeTracker/HistoryFinder, clone genealogies, fingerprinting, and Software Heritage content addressing—converges on a cascade:

1. Content/FQN exact (near-perfect precision)  
2. Same-container rename via body+signature+refs (~95–99% P)  
3. Cross-container move with stricter body thresholds (~97%+ P at high conf)  
4. Signature-evolution labels on stable identities  
5. Explicit split/merge multi-maps (high P, incomplete R)  
6. Residual bipartite + embedding **soft** edges only  

Modern tools show that **≥99% precision** is achievable for rename/move tracking when statement/body matching is strong (RefactoringMiner totals P≥0.989 across Java/Python/Kotlin benchmarks as of 2026-06-29; CodeTracker 99.9% method history). Fuzzy origin detection without structure sits nearer **88–91%** (Kim 2005), at or slightly above human inter-rater agreement—hence the need for confidence and conservative auto-link thresholds.

**For nudox:** Implement the §13 cascade over the shared IR; record tier+confidence+feature vector on every TerminusDB lineage edge; skip re-embed/re-index only on high-confidence near-identical pairs; prefer false splits. Reimplement matching language-agnostically on IR features rather than depending on Java-only detectors, using RMiner numbers as calibration targets for languages where oracles exist.

---

## 18. Appendix: precision/recall cheat sheet (for implementers)

| System | Year | Metric | Value | URL |
|--------|------|--------|-------|-----|
| Kim et al. origin | 2005 | Accuracy Apache2 / SVN | 87.8% / 91.1% | https://users.soe.ucsc.edu/~ejw/papers/kim-wcre2005.pdf |
| Kim et al. human agreement | 2005 | Inter-rater | 84.8% / 89.1% | same |
| Kim body-only factor | 2005 | Accuracy | 86.2% / 95.1% | same |
| RefactoringMiner Java B1 | 2026-06-29 | P / R | 0.999 / 0.984 | https://github.com/tsantalis/RefactoringMiner/blob/master/documentation/accuracy.md |
| RefactoringMiner Java B2 | 2026-06-29 | P / R | 0.989 / 0.979 | same |
| RefactoringMiner Python | 2026-06-29 | P / R | 0.996 / 0.997 | same |
| RefactoringMiner Kotlin | 2026-06-29 | P / R | 0.998 / 1.000 | same |
| RMiner Rename Method (B1) | 2026-06-29 | P / R | 0.995 / 0.955 | same |
| RMiner Move Method (B1) | 2026-06-29 | P / R | 0.992 / 0.987 | same |
| RMiner Extract And Move (B1) | 2026-06-29 | P / R | 1.000 / 0.696 | same |
| RefactoringMiner 2.0 paper | 2022 | avg P / R | 99.6% / 94% | https://www.computer.org/csdl/journal/ts/2022/03/09136878/1liqhds60s8 |
| RefDiff 2.0 Java | 2020 | P / R | ~96% / ~80% | https://homepages.dcc.ufmg.br/~mtov/pub/2020-tse-refdiff.pdf |
| RefDiff 2.0 JS/C | 2020 | P / R | 88–91% | same |
| RefDetect Java | 2021 | P / R | ~91% / ~85% | ResearchGate RefDetect |
| RefDetect C++ | 2021 | P / R | 96.1% / 94.1% | same |
| CodeTracker methods | 2022 | P / R | 99.9% / 99.9% | https://users.encs.concordia.ca/~nikolaos/publications/FSE_2022.pdf |
| CodeTracker blocks | ~2024 | P / R | ~99.5% | TSE block tracking line |
| CodeShovel | 2021 | complete history | ~90% methods / 97% changes | https://www.cs.ubc.ca/~rtholmes/papers/icse_2021_grund.pdf |
| HistoryFinder | 2025 | F1 | best among compared tools on new oracles | https://arxiv.org/html/2507.14716 |
| HistoryFinder thresholds | 2025 | body same/cross file | 0.70 / 0.75 | same |
| GumTree inaccurate maps | 2023–24 | % file revs | 20–29% | https://arxiv.org/pdf/2011.10268 |
| MTDiff inaccurate maps | 2023–24 | % file revs | 25–36% | same |
| IJM inaccurate maps | 2023–24 | % file revs | 21–30% | same |
| RMiner AST multi-map | ~2024 | P / R | ~99.7% / ~98.4% | TOSEM AST diff line |
| AURA vs prior | 2010 | Δ recall | +53.07% | https://dl.acm.org/doi/10.1145/1806799.1806848 |
| Roseau API breaks | 2025 | F1 | ~0.99 | ICSME 2025 area reports |
| SWHID | 2025 | ISO | IEC 18670 | https://www.softwareheritage.org/software-hash-identifier-swhid/ |
| git rename default | — | similarity | 50% file-level | git documentation |

---

## 19. Changelog


- 2026-07-16: Full academic survey completed with fresh WebSearch/WebFetch verification; RefactoringMiner accuracy as of 2026-06-29; HistoryFinder 2025 oracles; SWHID ISO status; concrete nudox pipeline.
