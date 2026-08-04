# Structural/Typed Diffing and Type-Directed Symbol Search for an IR-Native VCS and Lookup Layer

> **Status (Rev 3.2):** Research context, non-normative — feeds `design/IR-NATIVE-VCS-DESIGN.md`. The design's record path is "materialize prev pristine vs new arena → IrAtoms" keyed by `IntroId` (not `NudoxPath`), and the archive type fingerprint is the design's `nudox.tyskel.v1` opcode walk — this brief's `TypeSkeleton` feature vector remains the search-side concern. `CommuteResult::Conflict` / `merge` sketches (§3.6) are superseded by K14 (single-writer linear).

**Research date:** 2026-07-16  
**Audience:** IR platform / registry / edge serving design  
**Scope:** (1) Rust structural-diff crates with focus on `diff-struct`; (2) academic and industrial tree/AST/semantic diff; (3) symbol-level deltas over package IR keyed by `NudoxPath`; (4) Hoogle- and rustdoc-style type-directed search mapped onto multi-language IR; (5) cold-path serving without Terminus.  
**Local priors skimmed:** `.research/librarification/15-gui-references` (Hoogle §1.8, rustdoc search index), `05-symbol-identity-academic` (GumTree / TED), `10-tantivy` (symbol search stack), live IR (`workspace/ir/{entry,ty,function}.rs`).

---

## 0. Executive framing

Nudox stores packages as **typed IR**, not as blobs of source. That changes both versioning and lookup:

| Concern | Text VCS / search | IR-native layer |
|--------|-------------------|-----------------|
| Diff unit | Line / hunk | Symbol entry / field of `Entry` / nested `Type` |
| Identity key | Path + line range (unstable) | `NudoxPath` (package-local) + moniker/lineage (cross-version) |
| Patch semantics | Apply to text | Apply to map of entries; commute when paths disjoint |
| Search | BM25 over tokens | Name + **type skeleton** + inverted fingerprint index |
| Serve hot path | Graph DB or full deserialize | mmap package archive → path/type index → zero-copy view |

Two orthogonal mechanisms must share vocabulary:

1. **Structural typed diff** — produce and apply compact deltas between IR snapshots (VCS, incremental reindex, client sync).  
2. **Type-directed symbol search** — rank symbols by approximate type shape (product wedge; rare outside Haskell/rustdoc).

Neither is solved by line-diff crates alone, nor by pure full-text search. The rest of this brief grounds each layer in real APIs and papers, then sketches Rust traits that fit the existing IR.

---

## 1. `diff-struct` and the Rust structural-diff ecosystem

### 1.1 What `diff-struct` is

Crate name on crates.io: **`diff-struct`** (library crate name: **`diff`**).  
Sources: [lib.rs/crates/diff-struct](https://lib.rs/crates/diff-struct), [docs.rs/diff-struct](https://docs.rs/diff-struct), [crates.io/crates/diff-struct](https://crates.io/crates/diff-struct), GitHub `benhall-7/diff-struct`.

- Latest documented release in this survey: **0.5.3** (docs built with `diff_derive ^0.2.3`, `serde ^1`, optional `num`).  
- ~1.5K SLoC, MIT, modest adoption (~13k downloads/month, tens of reverse deps).  
- Design goal: abstract `A → B = D` and `A ← D = B` for **in-memory Rust values**, not for text files.

Abstract model from the crate README:

```
A -> B = D   and   A <- D = B
```

An object implementing `Diff` “translates well into human-readable diffs of arbitrary data types, avoiding the problems and false positives of diffing or patching plain-text versions of the serialized structs.”

### 1.2 Core trait: `Diff`

From [docs.rs `diff::Diff`](https://docs.rs/diff-struct/0.5.3/diff/trait.Diff.html):

```rust
pub trait Diff: Sized {
    type Repr;

    fn diff(&self, other: &Self) -> Self::Repr;
    fn apply(&mut self, diff: &Self::Repr);
    fn identity() -> Self;

    // Provided:
    fn apply_new(&self, diff: &Self::Repr) -> Self { /* clone self, apply */ }
    fn diff_custom<D: Differ<Self>>(&self, other: &Self, visitor: &D) -> D::Repr;
    fn apply_custom<D: Differ<Self>>(&mut self, diff: &D::Repr, visitor: &D);
    fn apply_new_custom<D: Differ<Self>>(&self, diff: &D::Repr, visitor: &D) -> Self;
}
```

Key semantics:

- **`Repr`** is *not* the same type as `Self` in general. Booleans, maps, and lists cannot represent their own difference as “just another value of the same type.”  
- **`identity()`** is the monoid-like zero: for integers, `0`; for `String`, empty; for maps, empty map. The docs assert  
  `i.apply_new(&i.diff(&s)) == s` i.e. `i + (s − i) = s`.  
- **`Differ`** is an escape hatch: custom visitors for types that need non-default strategies (useful if IR needs Myers on a field while field-wise equality elsewhere).

### 1.3 Derive macro

```rust
use diff::Diff;

#[derive(Debug, Default, PartialEq, Diff)]
#[diff(attr(
    #[derive(Debug, PartialEq)]
))]
pub struct ProjectMeta {
    contributors: Vec<String>,
    combined_work_hours: usize,
}
```

Behavior:

- Generates `ProjectMetaDiff` (name = base + `"Diff"` by convention).  
- Only helper attribute today: `#[diff(attr(...))]` to forward derives/docs onto the generated Repr.  
- Works on tuple and named field structs **when every field implements `Diff`**.  
- Enums are **not** first-class derive targets in the same automatic way as structs (see limitations).

Derived and built-in base types (crate docs / README):

| Category | Types | Typical `Repr` |
|----------|-------|----------------|
| Scalars | integers, floats, `bool`, `char` | delta or `Option<T>` replace |
| Text | `String`, `&str`, `PathBuf` | `Option<T>` full replace if changed |
| Smart pointers | `Box`, `Rc`, `Arc` | recurse into inner |
| Options | `Option<T>` | `OptionDiff<T>` |
| Maps | `HashMap`, `BTreeMap` | `*MapDiff` (altered + removed) |
| Sets | `HashSet`, `BTreeSet` | set diffs |
| Sequences | `Vec`, arrays | `VecDiff` / `ArrayDiff` |
| Tuples | up to length 18 | product of field Reprs |
| NonZero* | via `impl_num` feature | numeric Repr |

### 1.4 `HashMapDiff`

```rust
pub struct HashMapDiff<K: Hash + Eq, V: Diff> {
    /// Values that are changed or added
    pub altered: HashMap<K, <V as Diff>::Repr>,
    /// Values that are removed
    pub removed: HashSet<K>,
}
```

This is the **right shape for IR entry tables**. Nudox `Index` is:

```rust
// workspace/ir/entry.rs
pub struct Index {
    pub root_ids: Vec<NudoxPath>,
    pub entries_by_path: HashMap<NudoxPath, Entry>,
}
```

A natural package-level structural delta is essentially `HashMapDiff<NudoxPath, Entry>` once `Entry` implements `Diff` (or a hand-rolled `EntryDiff` that mirrors the same altered/removed split). Note: `altered` folds **insert and update** into one map of value-level Reprs; pure inserts are “diff from identity.”

### 1.5 `VecDiff` / `VecDiffType`

```rust
pub struct VecDiff<T: Diff>(pub Vec<VecDiffType<T>>);

pub enum VecDiffType<T: Diff> {
    Removed  { index: usize, len: usize },
    Altered  { index: usize, changes: Vec<T::Repr> },
    Inserted { index: usize, changes: Vec<T::Repr> },
}
```

Important caveat from the crate README:

> The implementation of diffing Vec’s is **non-standard**. It is faster and simpler than Myers’s algorithm, but **more error-prone on lists with many nearby duplicate elements**.

For IR this matters for:

- `Function.input_parameters: Option<Vec<Parameter>>` — positional; index-based edit is OK if identity is position.  
- `members: Option<Vec<NudoxPath>>` — better treated as a **set** or keyed map, not a positional vec, or moves will look like delete+insert.  
- Large statement lists in bodies — prefer content-addressed statements or Myers/`imara-diff` on a normalized token stream, not default `VecDiff`.

### 1.6 `OptionDiff`

```rust
pub enum OptionDiff<T: Diff> {
    Some(T::Repr),
    None,
    NoChange,
}
```

This is exactly the right encoding for optional IR fields (`generics`, `receiver`, `attributes`, docs, etc.): distinguish “became None,” “became Some(…),” and “unchanged” without serializing `Some(identity_diff)`.

### 1.7 Apply path and serde

- **In-place:** `value.apply(&d)`.  
- **Functional:** `value.apply_new(&d)`.  
- **Serde:** `HashMapDiff`, `VecDiff`, `OptionDiff`, and generated `*Diff` structs implement `Serialize`/`Deserialize` when `Repr` does (crate always depends on `serde`).

Practical pipeline for package deltas:

```text
old_index.diff(&new_index)  →  IndexDiff  →  postcard/bincode/yaml  →  store / ship
old_index.apply(&index_diff) →  new_index
```

Used in the wild for game-mod file patching (`motion_lib` SSBU example in README): produce YAML field diffs across game updates and re-apply onto modded files — same pattern as “rebase symbol patches onto new package IR.”

### 1.8 Limitations for large IR / recursive enums

These are the deal-breakers if one naïvely `#[derive(Diff)]` on the whole IR:

1. **Recursive enums (`Type`, `Entry` kinds)**  
   `Type` in `workspace/ir/ty.rs` is a large recursive enum (`TypeReference`, `FunctionPointer`, `Tuple(Vec<Type>)`, `Union`, `BorrowedRef`, …). Derive expects struct fields; recursive enum Reprs need **hand-written** `Diff` with care for stack depth and box indirection. Naïve full-tree Repr can be as large as a full clone when types change deeply.

2. **Cost of `diff` is O(size of both trees)**  
   No structural hashing short-circuit. For multi-MB package indexes, always prefilter with content digests / per-entry blake3 before field-level `Diff`.

3. **Vec semantics ≠ list identity**  
   Parameter reorder (e.g. optional args shuffled) becomes remove+insert sequences, not “moved.” Semantic merge tools (GumTree moves) are smarter; `diff-struct` is not.

4. **Maps require `Hash + Eq` keys**  
   `NudoxPath` already is `Hash + Eq`. Good.  
   Non-deterministic `HashMap` iteration order affects **serialized** patch bytes even when patches are semantically equal — always canonicalize (BTreeMap or sort keys before seal) for content-addressed CAS.

5. **Identity element for complex types**  
   `Diff::identity()` for a full `Entry` may be meaningless; package patches should not reconstruct from identity, only from a known parent snapshot hash.

6. **No first-class “move” op**  
   Path rename is delete+insert unless a higher layer emits `Rename { from, to }` (see §3).

7. **Maintenance / scale**  
   Small crate, last major activity ~2022; fine as a dependency or as a **pattern to reimplement**, not as the sole foundation for a product VCS.

### 1.9 Alternatives (when to use what)

| Crate | Model | Best for | Weak for |
|-------|--------|----------|----------|
| **`diff-struct`** | Associated `Repr`, apply monoid | Nested maps/structs with serde patches | Large recursive enums, list moves |
| **`structdiff`** | `#[derive(Difference)]` → `Vec` of field-level ops; `diff` / `diff_ref` / `apply*` | Sparse field patches; optional serde/nanoserde; large fields via `diff_ref` | Whole-map keyed IR without custom collection strategy |
| **`daft`** | `Diffable` + derive; semantic diffs of Rust data | Human-readable structured diffs | Heavy VCS apply pipelines (verify maturity) |
| **`diffogus`** | Diff presentation between instances | Debugging / API response “what changed” | Patch apply / storage format |
| **`serde_diff` / `serde-diff`** | Walk two values via serde model; serialize only differing fields | Any serde-serializable type without hand impls | Types with poor serde shape; not IR-idiomatic |
| **`similar`** | Text diff (Myers etc.) | Docs strings, source snippets in UI | Typed IR |
| **`imara-diff`** | Myers + **Histogram** (patience variant); very fast | Normalized token/line sequences, statement lists | Structured maps of symbols |

**`structdiff` sketch (real API shape):**

```rust
pub trait StructDiff {
    type Diff: StructDiffOwnedBound;
    type DiffRef<'target>: StructDiffRefBound + Into<Self::Diff>
    where Self: 'target;

    fn diff(&self, updated: &Self) -> Vec<Self::Diff>;
    fn diff_ref<'target>(&'target self, updated: &'target Self)
        -> Vec<Self::DiffRef<'target>>;
    fn apply_single(&mut self, diff: Self::Diff);
    fn apply(self, diffs: Vec<Self::Diff>) -> Self where Self: Sized;
    fn apply_mut(&mut self, diffs: Vec<Self::Diff>);
}
```

**Recommendation for Nudox:**

- **Do not** depend on `diff-struct` for the public IR delta schema.  
- **Do** steal its `HashMapDiff` / `OptionDiff` shapes.  
- Implement a **domain-specific** `SymbolPatch` / `EntryDiff` (section 3) with postcard serialization and explicit rename ops.  
- Use **`imara-diff`** (Histogram default) only for body/doc text and for ordered parameter *display* diffs.  
- Consider `structdiff`-style sparse field ops *inside* an entry once path identity is fixed.

---

## 2. Academic and industrial structural diff

### 2.1 Classical string algorithms (still relevant)

Even in an IR-native VCS, some payloads remain sequences (docs, statement tokens, serialized parameter lists for UI).

| Algorithm | Origin / use | Notes |
|-----------|--------------|-------|
| **Myers** | Eugene W. Myers, “An O(ND) Difference Algorithm and Its Variations,” *Algorithmica* 1(1–4):251–266, 1986 | Git default historically; linear-space variants in `imara-diff` |
| **Patience** | Bram Cohen (blog ~2005); used in `git diff --patience` | Anchors on unique lines; better for refactors with churned noise |
| **Histogram** | Git `histogram`; patience variant | `imara-diff` reports **10–100% faster than Myers** on many workloads; often more human-readable than pure minimal Myers |

`imara-diff` documents both Myers and Histogram and claims large wins vs `similar` on kernel-scale inputs. For IR body deltas, prefer Histogram over inventing TED.

### 2.2 Tree edit distance (TED)

**Definition:** minimum-cost sequence of node insert / delete / relabel (sometimes move) transforming tree \(T_1\) into \(T_2\).

Key papers / algorithms:

1. **Tai (1979)** — early TED formulation (high polynomial complexity).  
2. **Zhang & Shasha (1989)** — “Simple Fast Algorithms for the Editing Distance Between Trees and Related Problems,” *SIAM J. Comput.* 18(6):1245–1262.  
   - Dynamic programming over key roots; classic bound often cited as \(O(n^4)\) worst case; \(O(n^2)\) space variants discussed in tutorials.  
   - Still the pedagogical baseline ([arxiv:1805.06869](https://arxiv.org/abs/1805.06869) tutorial on backtracing).  
3. **Klein (1998)** — improved decomposition strategies for unrooted trees.  
4. **RTED** — Pawlik & Augsten, “RTED: A Robust Algorithm for the Tree Edit Distance,” *PVLDB* 5(4):334–345, 2012 ([arxiv:1201.0230](https://arxiv.org/abs/1201.0230)).  
   - Robust choice of left/right/heavy paths; complexity ≤ best previous LRH algorithms.  
5. **APTED** — Pawlik & Augsten; state-of-the-art practical TED ([github.com/DatabaseGroup/apted](https://github.com/DatabaseGroup/apted)); All Path Tree Edit Distance.  

**Nudox takeaway:** Full TED on package-scale ASTs is the wrong default. Use TED or APTED only on **small candidate subtrees** (e.g. two function bodies already paired by moniker).

### 2.3 GumTree and AST differencing ecosystem

**GumTree** (Falleri et al., 2014) — de-facto language-independent AST differencing engine  
(GitHub: [GumTreeDiff/gumtree](https://github.com/GumTreeDiff/gumtree)).

Algorithm sketch (aligned with internal research note `05-symbol-identity-academic`):

1. **Top-down:** greedy largest isomorphic subtree mappings (hash anchors).  
2. **Bottom-up:** extend mappings using structural context; recovery limited by max-size hyperparameter.  
3. Emit edit script: insert, delete, update, **move**.

Known quality issues:

- Studies report **inaccurate mappings on 20–29%** of file revisions for GumTree (and higher for some successors under certain oracles). Sources commonly cited: DAT paper [arxiv:2011.10268](https://arxiv.org/pdf/2011.10268), ICSE 2024 GumTree simple, etc.  
- Weak at **multi-mapping** (extract/inline method) vs RefactoringMiner-class tools.

Successors / relatives:

| Tool | Contribution |
|------|----------------|
| MTDiff (Dotzler & Philippsen, 2016) | Move-optimized heuristics |
| IJM (Frick et al.) | Compact edit scripts |
| DAT (Martinez, Falleri, Monperrus) | Hyperparameter auto-tuning (~18–22% shorter scripts) |
| GumTree simple (Falleri & Martinez, ICSE 2024) | Cheaper heuristic, better quality on several datasets |
| SAT-DIFF (2024) | SAT for concise edits (~1.72× fewer edits vs GumTree in reported numbers) |
| RefactoringMiner AST Diff | Semantic multi-mapping; high precision on Java oracles |
| DiffSitter-adjacent | tree-sitter CST ops — natural multi-language fit |
| HyperDiff (2025) | Stability of tree differencing variants |

**Nudox policy (from academic brief, still correct):**

- Prefer **normalized token / statement sequences** for body identity evidence.  
- Run AST/CST diff **inside candidate pairs** or same-file, never as global bipartite match over entire packages.  
- For extract/inline lineage, use moniker + refactoring-aware matchers, not vanilla GumTree alone.

### 2.4 Semantic / structural diff products

| System | Approach |
|--------|----------|
| **Difftastic** (Wilfred Hughes; Rust + tree-sitter) | Parse → s-expression trees → graph search marking novel vs shared nodes; human review focused; **not** a merge/patch engine |
| **SemanticMerge / Plastic SCM Xdiff** | Language-aware merge; statement-level; commercial |
| **SemanticDiff** | “Semantic” (deeper than pure structure) vs Difftastic “structural” |
| **rustdoc / docs tooling** | Not a VCS; structural signatures for search ranking |

Difftastic deliberately avoids patch generation; Nudox VCS needs **applyable** patches → domain IR ops beat AST graph search for storage.

### 2.5 OT vs CRDT vs patch commutation

| Model | Core idea | Fit for IR symbol VCS |
|-------|-----------|------------------------|
| **Operational Transformation (OT)** | Transform concurrent ops against each other so order converges (Google Docs classic) | Hard: needs transform functions for every op pair on nested IR |
| **CRDT** | Commutative mergeable state (or op-based with causal prep) | Good for **presence / annotations / comments**; heavy for whole `Entry` trees |
| **Patch commutation (Pijul / Darcs theory)** | Patches are first-class; commute when independent; conflict when not | **Best conceptual fit** for symbol-level deltas |

Pijul-like systems store a set of **changes** with explicit dependencies, not a linear commit chain of snapshots. For Nudox:

- Two patches that touch **disjoint `NudoxPath` sets** commute.  
- Two patches that update the same path’s nested fields may commute if field sets are disjoint (`docs` vs `signature`), else conflict.  
- Renames introduce edges: `Rename(A→B)` does not commute with `Update(A)` without transform.

This is **not** full OT: you do not transform arbitrary concurrent character inserts; you define a small algebra over `EntryOp` (section 3).

### 2.6 Minimal citation set (keep in design docs)

1. Myers, E. W. (1986). An O(ND) Difference Algorithm… *Algorithmica*.  
2. Zhang, K. & Shasha, D. (1989). Simple Fast Algorithms for the Editing Distance Between Trees… *SIAM J. Comput.*  
3. Pawlik, M. & Augsten, N. (2012). RTED… *PVLDB*.  
4. Falleri, J.-R. et al. (2014). Fine-grained and accurate source code differencing (GumTree). *ASE*.  
5. Pawlik & Augsten — APTED follow-ons (Efficient Computation of the Tree Edit Distance series).  
6. Martinez, Falleri, Monperrus — DAT / hyperparameter tuning for GumTree.  
7. Tsantalis et al. — RefactoringMiner accuracy oracles (identity / multi-mapping).  
8. Mitchell, N. — Hoogle design (TFP slides; 2020 blog “Hoogle Searching Overview”).  

---

## 3. Symbol-level deltas for package IR

### 3.1 Identity stack (do not conflate)

From live IR + librarification plan:

| Layer | Type | Version coupling |
|-------|------|------------------|
| Package-local path | `NudoxPath` (`Local(PathBuf)` / `External { path, dependency }`) | Within one package snapshot |
| Graph IRI | `Symbol/{lang}/{pkg}/{fq}` style | Version-agnostic edge target |
| Heart instance id | UUIDv5(instance ‖ versioned PackageId ‖ segments) | Version-coupled |
| Moniker / lineage | SCIP-like descriptors + optional `LineageId` | Cross-version identity |

**`SymbolPatch` is keyed by moniker or stable path stem, not by heart UUID alone**, so patches remain meaningful across package versions when lineage resolves.

### 3.2 Design: `SymbolPatch` / `EntryDiff`

```rust
use rustc_hash::FxHashMap as HashMap;
use serde::{Serialize, Deserialize};

/// Package-local address (existing IR).
// pub enum NudoxPath { External { .. }, Local(PathBuf) }

/// Cross-version identity (from moniker RFC / lineage).
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolKey {
    pub moniker: String,           // e.g. "rust;std/vec/Vec#push()."
    pub path: Option<NudoxPath>,   // snapshot-local, optional after rename
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageDelta {
    pub parent: ContentHash,       // blake3 of prior package IR seal
    pub child: ContentHash,        // seal after apply
    pub package_stem: PackageStemId,
    pub ops: Vec<EntryOp>,
    /// Optional: allows pijul-like partial order
    pub depends_on: Vec<ChangeId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EntryOp {
    Insert {
        key: SymbolKey,
        entry: Box<Entry>,         // full value, or CAS ref
    },
    Delete {
        key: SymbolKey,
        prev_hash: ContentHash,    // for conflict detection
    },
    Update {
        key: SymbolKey,
        prev_hash: ContentHash,
        diff: EntryDiff,
    },
    Rename {
        from: SymbolKey,
        to: SymbolKey,
        /// nested field changes simultaneous with rename
        diff: Option<EntryDiff>,
    },
}

/// Field-level sparse diff inside one entry (structdiff-inspired).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryDiff {
    pub kind: Option<KindChange>,
    pub function: Option<FunctionDiff>,
    pub docs: Option<TextPatch>,       // imara-diff hunks or replace
    pub members: Option<PathSetDiff>,  // set semantics, not VecDiff
    pub attributes: Option<AttrDiff>,
    // … other Entry arms as needed
    pub unknown_forward_compat: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionDiff {
    pub input_parameters: Option<ParamListDiff>,
    pub output_parameters: Option<ParamListDiff>,
    pub generics: Option<GenericsDiff>,
    pub receiver: Option<OptionDiffLite<ReceiverKind>>,
    pub attributes: Option<VecDiffLite<Attribute>>,
    pub implemented: Option<bool>,
}

/// Nested type field diffs — recursive, but only along changed spines.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TypeDiff {
    Replace(Type),                 // large or enum-tag change
    Same,
    Tuple(Vec<TypeDiff>),
    FunctionPointer {
        inputs: Option<Vec<TypeDiff>>,
        output: Option<Box<TypeDiff>>,
        // is_async / unsafety flags as Option<bool> replaces
    },
    TypeReference {
        path: Option<NudoxPath>,
        args: Option<Vec<GenericArgDiff>>,
    },
    BorrowedRef {
        lifetime: Option<OptionDiffLite<String>>,
        is_mutable: Option<bool>,
        inner: Option<Box<TypeDiff>>,
    },
    // Primitive/Never/Any/Infer: only Replace or Same
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ParamListDiff {
    /// Prefer keyed by name when present; else positional
    ByName(HashMapDiffLite<String, ParameterDiff>),
    Positional(Vec<ParamPosOp>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ParamPosOp {
    Keep,
    Delete,
    Insert(Parameter),
    Update(ParameterDiff),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParameterDiff {
    pub name: Option<String>,
    pub ty: Option<TypeDiff>,
    pub default: Option<OptionDiffLite</* expr id */ ContentHash>>,
}
```

### 3.3 Mapping structural `Diff` → insert/update/delete

Algorithm for two sealed package indexes \(I_{old}, I_{new}\):

```text
1. Build maps M_old, M_new : SymbolKey → (NudoxPath, Entry, content_hash)
   (resolve monikers; fall back to NudoxPath if moniker missing)

2. For k in M_old \ M_new:
     if ∃ rename evidence (identical body hash / high GumTree score / explicit producer event):
        emit Rename
     else:
        emit Delete

3. For k in M_new \ M_old:
     if not matched as rename target: emit Insert

4. For k in intersection:
     if hash equal: skip
     else: emit Update { diff: structural_entry_diff(old, new) }
```

`structural_entry_diff` can call a hand-rolled recursive differ inspired by `diff-struct` / `structdiff`, with:

- early-exit on equal subtree hashes;  
- `TypeDiff::Replace` when enum tags differ;  
- set-diff for `members: Vec<NudoxPath>`.

### 3.4 Nested type field diffs for Function signatures

`Function` (`workspace/ir/function.rs`) carries:

- `input_parameters` / `output_parameters`  
- `generics`, `receiver`, `attributes`, `overloads`  
- `members`, `implemented_protocols`

For API churn UX and type-search index maintenance:

| Change | Patch shape | Search index effect |
|--------|-------------|---------------------|
| Add optional param | `ParamPosOp::Insert` | new fingerprint |
| Change return type | `TypeDiff` on output | re-key inverted skeleton |
| Generic param rename only | `GenericsDiff` with alpha-equivalence note | **stable** fingerprint if normalized |
| `async` flag | `attributes` | fingerprint arity/effect bits |
| Docs only | `TextPatch` | no type index change |

**Alpha-normalization** of generics (`T0`, `T1`, … by first appearance) is mandatory before type fingerprinting so `fn foo<T>(T) -> T` and `fn foo<U>(U) -> U` share a skeleton.

### 3.5 Identity stability across package versions

Rules:

1. **Never** key long-lived patches solely by `NudoxPath` string if renames are common — attach moniker.  
2. Store `prev_hash` on Update/Delete so apply can soft-fail (“already applied” / “conflict”).  
3. When producers bump normalizer version, dual-write old+new monikers for one release (see moniker RFC research).  
4. Content hash of **normalized** entry body is T0 short-circuit for “unchanged symbol” across versions even if path formatting drifts.

### 3.6 Becoming a “change” in a pijul-like system

```rust
pub trait ChangeAlgebra {
    /// True if applying a then b equals b then a on all states.
    fn commute(a: &PackageDelta, b: &PackageDelta) -> CommuteResult;

    /// Merge non-conflicting concurrent deltas sharing same parent.
    fn merge(a: PackageDelta, b: PackageDelta) -> Result<PackageDelta, Conflict>;
}

pub enum CommuteResult {
    Commute,           // independent path sets / field sets
    Inverse,           // a then b = id (rare; delete+insert same)
    Conflict(ConflictReason),
}
```

Storage sketch (pijul-like):

```text
Change = {
  id: ChangeId,
  author, timestamp,
  delta: PackageDelta,   // ops with SymbolKeys
  deps: [ChangeId],      // explicit causal edges
}
Repository state = apply(deps-satisfying partial order) → sealed Index
```

Snapshot VCS (git-like) remains available: seal full `Index` to CAS; deltas are an **optimization and UX layer**, not the only truth. For client sync (research `14-client-sync`), shipping `PackageDelta` vs last known seal is bandwidth-cheap.

---

## 4. Hoogle and type-directed search

### 4.1 Hoogle 4 → 5 evolution (Neil Mitchell)

Primary source: [Hoogle Searching Overview (2020-06-09)](http://neilmitchell.blogspot.com/2020/06/hoogle-searching-overview.html); earlier TFP slides ([Hoogle finding functions from types](https://ndmitchell.com/downloads/slides-hoogle_finding_functions_from_types-16_may_2011.pdf), [Fast Type Searching](https://ndmitchell.com/downloads/slides-hoogle_fast_type_searching-09_aug_2008.pdf)).

| Generation | Strategy | Failure mode |
|------------|----------|--------------|
| v1–3 | Simple data files + expensive search | Did not scale |
| v4 | Elaborate precomputed indexes | Babysitting; rare updates; pathological queries |
| **v5** | **Simple mmap data + O(n) small-constant scan** | Scales to Stackage; nightly rebuild |

Hoogle 5 design principles:

1. **mmap** vectors/bytestrings via `ForeignPtr` — avoid deserializing the world.  
2. **O(n) is fine** if the constant is tiny and n is hundreds of thousands of *deduplicated* signatures.  
3. Separate **name search** (C substring over `\0`-separated blob) from **type search**.  
4. ~183MB Stackage DB; ~78% is render/docs payload (gzipped item info).

### 4.2 Type fingerprint (18 bytes)

From Mitchell’s post — each **deduplicated** type signature → **18-byte fingerprint**:

| Field | Size | Meaning |
|-------|------|---------|
| Arity | 1 byte | `a -> b -> c` ⇒ 3 |
| Constructor/variable count | 1 byte | `Maybe a -> a` ⇒ 3 |
| Three rarest names | 3 × 4 bytes | 32-bit rarity ranks of type constructors/vars |

Rarity intuition: searching `ShakeOptions -> [a] -> [a]` should lean on **ShakeOptions**, not on lists.

**Search procedure:**

1. Linear scan all fingerprints (~150K distinct ⇒ ~2.5MB) → top 100.  
2. Expensive precise match on those 100 (classes, alias expansion, shape) — originally missing; later contributed (Matt Noonan).  
3. For years fingerprint-only ranking was “good enough.”

This is the **template for Nudox** at package and corpus scale until inverted indexes become necessary (millions of signatures).

### 4.3 rustdoc search index (weak Hoogle)

From rustc-dev-guide and local `15-gui-references`:

- Pipeline: `search_index.rs` → JSON → `search.js` linear scan.  
- Compact parallel arrays: names, types, parent modules, **function signature encodings**, aliases, deprecation.  
- Supports type-ish queries (`vec -> usize`, `-> vec`, `String, &str -> Result`).  
- **Not** full unification; ranking heuristics on encoded signatures.  
- Per-crate browser delivery — wrong for multi-package INDEX (use Tantivy/SQLite + type sidecars instead of shipping giant JS indexes).

### 4.4 Mapping onto multi-language IR `Type`

Existing enum (abridged from `workspace/ir/ty.rs`):

```rust
pub enum Type {
    TypeReference(TypeReference),
    SelfType,
    DynTrait(DynTrait),
    GenericParam(GenericParam),
    Primitive(Primitive),
    FunctionPointer(FunctionPointer),
    Tuple(Vec<Type>),
    RecordLiteral(Box<Record>),
    Slice(Box<Type>),
    Array { r#type: Box<Type>, length: usize },
    ImplTrait(Vec<GenericBound>),
    Infer,
    Never,
    Any,
    RawPointer { is_mutable: bool, r#type: Box<Type> },
    BorrowedRef { lifetime: Option<String>, is_mutable: bool, r#type: Box<Type> },
    Union(Vec<Type>),
    // Intersection, etc.
}
```

**Normalization pipeline before fingerprint:**

```rust
pub trait TypeNormalize {
    /// Erase lifetimes; map language self → SelfType; expand trivial aliases.
    fn erase_regions(&self) -> Type;
    /// Rename generics to T0..Tn by first occurrence (alpha).
    fn alpha_rename(&self) -> Type;
    /// Collapse language-specific sugar (Option vs Nullability) into IR canon.
    fn canon_sugar(&self) -> Type;
}

pub fn type_skeleton(t: &Type) -> TypeSkeleton {
    let t = t.erase_regions().alpha_rename().canon_sugar();
    TypeSkeleton {
        arity: count_arrow_arity(&t),
        node_count: count_constructors_and_vars(&t),
        rarest: top3_rarest_names(&t, &GLOBAL_TYPE_FREQ),
        // Nudox extensions:
        effects: effect_bits(&t),          // async / const / unsafe
        lang: ecosystem_tag(),
        shape_hash: blake3_16(&structural_shape(&t)),
    }
}
```

Multi-language issues Hoogle never faced:

| Issue | Strategy |
|-------|----------|
| Method receivers | Encode as first arg *or* separate `receiver` bit in fingerprint |
| Overloads | Index each overload as distinct signature row |
| Union / intersection types | Treat as sorted multiset of constructors in rarity list |
| Nominal vs structural records | Prefer nominal `TypeReference` when available |
| `Any` / `Infer` | Low weight; penalty in precise phase |

### 4.5 Index layout: inverted index from type skeleton → symbol ids

For corpus ≫ 150K, replace pure linear scan with hybrid:

```text
L0: Fingerprint table (flat, mmap, Hoogle-style) for small packages / offline
L1: Inverted postings:
      type_name_id → [SymbolId]          # rare names first
      shape_bucket (arity, node_count) → [SymbolId]
L2: Precise Type store (CAS or archive offset) for top-K refinement
L3: Name Tantivy index (existing 10-tantivy plan) for hybrid queries
```

Concrete Rust sketch:

```rust
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TypeFingerprint {
    pub arity: u8,
    pub nodes: u8,
    pub rare: [u32; 3],
    pub effects: u8,
    pub shape: u64, // truncated
} // pad to 24–32 bytes if helpful for SIMD

pub struct TypeSearchIndex {
    /// Sorted or hashed map: rarest type atom → postings
    pub by_atom: HashMap<TypeAtomId, PostingList>,
    pub fingerprints: MmapSlice<TypeFingerprint>, // optional global scan
    pub atom_freq: Vec<u32>,
    pub symbol_meta: MmapSlice<SymbolMeta>,       // package, path off, kind
}

pub trait TypeSearch {
    fn search(&self, query: &Type, opts: SearchOpts) -> Vec<ScoredSymbol>;
}

pub struct SearchOpts {
    pub package_filter: Option<PackageStemId>,
    pub lang: Option<Ecosystem>,
    pub allow_arg_reorder: bool,
    pub top_k_fingerprint: usize, // default 100–500
}
```

**Ranking (precise phase):**

1. Unification / matching with rename of generics.  
2. Penalty for arg reorder, extra args, missing args (Hoogle v2+ ideas).  
3. Bonus for exact shape_hash.  
4. Kind prior (function > type alias) from Tantivy fusion research.  
5. Popularity / quality fast fields when available (registry), **not** as primary signal for type match.

### 4.6 Integration: resolve against condensed IR archive, not Terminus

Terminus (graph) is for **lineage / Auto edges / analytics**, not for interactive type search QPS.

Correct path (product + research alignment with `15-gui-references` and terminus-tiering):

```text
type query
  → TypeSearchIndex (sidecar next to package archive or global INDEX)
  → top-K SymbolIds
  → open package IR archive (mmap)
  → path index → Entry bytes
  → render signature + docs
```

Never: type query → SPARQL/Terminus hop → entry.  
Graph expansion (callers/callees) may use Terminus or prebuilt edge files as a **second** cold path (section 5).

### 4.7 Query UX (from local Hoogle research)

| UX | Note |
|----|------|
| Detect type vs name query | Heuristic: `->`, `=>`, generics, structured tokens |
| Package filters | `+serde` chips = Hoogle `+pkg` |
| Type builder UI | For non-Haskell languages; lower barrier than free-form |
| Explain ranking | Optional debug for power users |
| Avoid pure O(n) forever | Fine to 10⁵–10⁶; inverted index beyond that |

---

## 5. Serving path without Terminus

### 5.1 Happy path: symbol fetch / type search hit

```text
Request (symbol path | type query | name query)
    │
    ▼
L1 memory (moka Cache<Key, Arc<View>>)
    │ miss
    ▼
Resolve package seal → filesystem / CAS path
    │
    ▼
open PackageIrArchive (memmap2)
    │
    ▼
path index | type index | name postings  (header offsets in archive footer)
    │
    ▼
zero-copy EntryView<'mmap>  (rkyv / custom POD / postcard-from-slice)
    │
    ▼
Respond (JSON/protobuf/postcard) + populate L1
```

```rust
pub struct PackageIrArchive {
    mmap: memmap2::Mmap,
    header: ArchiveHeader,
}

pub struct ArchiveHeader {
    pub magic: [u8; 8],
    pub format_version: u32,
    pub path_index_off: u64,
    pub type_index_off: u64,
    pub entries_off: u64,
    pub seal: ContentHash,
}

/// Zero-copy view — no Terminus, no full Index deserialize.
pub trait EntryView<'a> {
    fn path(&self) -> NudoxPath;
    fn kind(&self) -> Kind;
    fn function_sig(&self) -> Option<FunctionSigRef<'a>>;
    fn docs(&self) -> Option<&'a str>;
    fn members(&self) -> impl Iterator<Item = NudoxPath> + 'a;
}

pub struct LookupService {
    l1: moka::sync::Cache<LookupKey, Arc<CachedEntry>>,
    archives: moka::sync::Cache<PackageSeal, Arc<PackageIrArchive>>,
    type_idx: Arc<TypeSearchIndex>, // may be global or per-archive
}

impl LookupService {
    pub fn get_entry(&self, key: LookupKey) -> Result<Arc<CachedEntry>, LookupError> {
        if let Some(v) = self.l1.get(&key) {
            return Ok(v);
        }
        // stampede control: see §5.3
        self.l1.try_get_with(key.clone(), || self.load_cold(&key))
            .map_err(|e| /* … */ e)
    }
}
```

### 5.2 Symbol search vs graph expansion (cold paths)

| Path | Data | Latency budget | Store |
|------|------|----------------|-------|
| Name search | Tantivy / postings | <10–30 ms | INDEX + local |
| Type search | Fingerprint + inverted + precise | <30–80 ms | type sidecar |
| Symbol hydrate | Archive mmap entry | <5 ms warm, <20 ms cold open | CAS archive |
| Graph expand (refs, impls) | Edge lists / Terminus / SCIP-like | higher; async OK | graph tier |
| Lineage timeline | moniker ↔ generations table | interactive | SQLite INDEX |

**Rule:** interactive omni-search never blocks on graph. Graph is a **panel hydration** after symbol identity is known.

### 5.3 Caching strategies (moka + stampede)

```rust
use moka::sync::Cache;
use std::time::Duration;

fn build_l1() -> Cache<LookupKey, Arc<CachedEntry>> {
    Cache::builder()
        .max_capacity(100_000)
        .time_to_idle(Duration::from_secs(600))
        .build()
}

fn build_archive_cache() -> Cache<PackageSeal, Arc<PackageIrArchive>> {
    Cache::builder()
        .max_capacity(256) // archives are large; few open mmaps
        .time_to_idle(Duration::from_secs(1800))
        .build()
}
```

**Stampede protection:**

- Prefer `Cache::try_get_with` / `get_with` so concurrent misses for the same key coalesce on one loader.  
- For type-search top-K result sets, cache `(TypeFingerprint query bits, filters) → Vec<SymbolId>` with short TTL.  
- Negative caching for missing packages (short TTL) to protect CAS.

**Invalidation:**

- Key includes `PackageSeal` / content hash — immutable seals never need invalidation, only eviction.  
- Mutable “latest” pointers use generation numbers; never cache “latest” without seal.

### 5.4 Condensed archive layout (concrete)

```text
[header]
[sorted path table: NudoxPath → (entry_off, entry_len, moniker_id)]
[type atom dictionary]
[type postings / fingerprint slab]
[entry blob region: length-prefixed rkyv/postcard entries]
[footer checksum]
```

Name search may live in a **global Tantivy** index pointing at `(seal, path)` rather than inside every archive; type postings can be **per-archive** (package-scoped type search) plus optional global merge for “all of crates.io.”

### 5.5 End-to-end type query example

```text
User:  (Iterator<Item=u8>) -> Option<u8>
  1. parse → IR Type (FunctionPointer)
  2. normalize + fingerprint
  3. inverted: atoms {Iterator, Item, u8, Option} → candidate set
  4. fingerprint score → top 200
  5. precise unify → ranked SymbolIds
  6. for i in top 20: L1 or archive hydrate EntryView
  7. respond name + normalized type + package + score
```

No Terminus round-trip. Graph “implementors of Iterator” is a separate button.

---

## 6. Recommended architecture (synthesis)

```text
┌─────────────────────────────────────────────────────────────┐
│ Producers (rustc/ts/go/…) → IR Index (entries_by_path)      │
└────────────────────────────┬────────────────────────────────┘
                             │ seal + monikers + type fingerprints
                             ▼
┌─────────────────────────────────────────────────────────────┐
│ CAS: PackageIrArchive (mmap) + TypeSearch sidecars          │
│ Deltas: PackageDelta / EntryOp (pijul-like change graph)    │
└───────────────┬─────────────────────────────┬───────────────┘
                │                             │
                ▼                             ▼
        Lookup / Search                  Lineage / Graph
        (no Terminus)                    (Terminus optional tier)
                │
                ▼
        moka L1 → archive → EntryView
```

**Diff stack:**

1. Content hashes for short-circuit.  
2. `EntryOp` map-diff keyed by `SymbolKey`.  
3. Nested `TypeDiff` / `FunctionDiff` for signature UX and incremental type-index update.  
4. `imara-diff` Histogram only for docs/body tokens.  
5. Optional GumTree/tree-sitter **offline** for lineage evidence, not for online apply.

**Search stack:**

1. Hoogle-style fingerprints + rarity atoms on IR `Type`.  
2. Inverted index when corpus grows.  
3. Tantivy for names (existing plan).  
4. Always hydrate from condensed IR archive.

---

## 7. Concrete trait sketches (copy-ready)

```rust
/// Structural applyable diff — domain version of diff-struct::Diff
pub trait StructuralDiff: Sized {
    type Repr: Clone + serde::Serialize + serde::de::DeserializeOwned;

    fn diff(&self, other: &Self) -> Self::Repr;
    fn apply(&mut self, diff: &Self::Repr) -> Result<(), ApplyError>;
    fn is_empty_diff(diff: &Self::Repr) -> bool;
}

/// Package-level symbol VCS surface
pub trait PackageVcs {
    fn delta(&self, old: &Index, new: &Index) -> PackageDelta;
    fn apply(&self, base: &Index, delta: &PackageDelta) -> Result<Index, Conflict>;
    fn commute(&self, a: &PackageDelta, b: &PackageDelta) -> CommuteResult;
}

/// Type-directed search
pub trait TypeDirectedSearch: Send + Sync {
    fn index_package(&mut self, seal: PackageSeal, index: &Index) -> Result<(), IndexError>;
    fn search(&self, q: TypeQuery) -> Result<Vec<ScoredHit>, SearchError>;
}

pub struct TypeQuery {
    pub ty: Type,
    pub filters: SearchOpts,
}

pub struct ScoredHit {
    pub symbol: SymbolKey,
    pub seal: PackageSeal,
    pub score: f32,
    pub why: MatchExplain, // fingerprint / unify / reorder penalty
}

/// Serving without Terminus
pub trait IrLookup: Send + Sync {
    fn entry(&self, seal: PackageSeal, path: &NudoxPath)
        -> Result<Arc<CachedEntry>, LookupError>;
    fn entry_by_moniker(&self, seal: PackageSeal, moniker: &str)
        -> Result<Arc<CachedEntry>, LookupError>;
}
```

---

## 8. Open decisions / risks

1. **Canonical serialization of patches** — sort all map keys; version the `EntryOp` schema.  
2. **Moniker quality** — patches are only as stable as monikers (see `19-symbol-moniker-rfc`).  
3. **When to inverted-index types** — measure at 500K vs 5M signatures.  
4. **Overload / impl method explosion** — may dominate type index; consider package-scoped default.  
5. **Conflict UX** — field-level merge for docs+signature concurrent edits vs whole-entry locks.  
6. **Do not** put full GumTree in the apply path; accuracy and cost are wrong.  
7. **Do not** serve interactive search from Terminus.

---

## 9. References (URLs)

### Crates / docs
- https://lib.rs/crates/diff-struct  
- https://docs.rs/diff-struct  
- https://crates.io/crates/diff-struct  
- https://docs.rs/structdiff  
- https://lib.rs/crates/daft  
- https://docs.rs/serde-diff  
- https://docs.rs/imara-diff  
- https://crates.io/crates/similar  

### Academic / tools
- Myers O(ND) PDF (classic): http://www.xmailserver.org/diff2.pdf  
- Zhang–Shasha tutorial: https://arxiv.org/abs/1805.06869  
- RTED: https://arxiv.org/abs/1201.0230  
- TED portal: https://tree-edit-distance.dbresearch.uni-salzburg.at/  
- APTED: https://github.com/DatabaseGroup/apted  
- GumTree: https://github.com/GumTreeDiff/gumtree  
- DAT: https://arxiv.org/pdf/2011.10268  
- SAT-DIFF: https://arxiv.org/pdf/2404.04731  
- Difftastic: https://difftastic.wilfred.me.uk/  

### Hoogle / rustdoc
- https://hoogle.haskell.org/  
- http://neilmitchell.blogspot.com/2020/06/hoogle-searching-overview.html  
- https://ndmitchell.com/downloads/slides-hoogle_finding_functions_from_types-16_may_2011.pdf  
- https://ndmitchell.com/downloads/slides-hoogle_fast_type_searching-09_aug_2008.pdf  
- https://rustc-dev-guide.rust-lang.org/rustdoc-internals/search.html  

### Local research
- `.research/librarification/15-gui-references` — Hoogle UX, rustdoc index  
- `.research/librarification/05-symbol-identity-academic` — GumTree / TED  
- `.research/librarification/10-tantivy` — name search stack  
- `workspace/ir/{entry,ty,function}.rs` — live IR shapes  

---

## 10. Bottom line

| Need | Use |
|------|-----|
| Typed field patches for IR | Hand-rolled `EntryOp` / `TypeDiff` (shapes from `diff-struct` / `structdiff`) |
| Text / body sequences | `imara-diff` Histogram |
| Cross-file symbol identity | Monikers + hashes; GumTree only as offline evidence |
| Concurrent edits | Pijul-like patch commutation on path/field disjointness |
| Type search | Hoogle 5 fingerprints → inverted atoms → precise unify on IR `Type` |
| Serve | moka → mmap archive → zero-copy entry; **never** Terminus on hot path |

This stack turns package IR into both a **versioned, mergeable artifact** and a **queryable API surface**, which is the core of an IR-native VCS and lookup layer.

---

*Word count: ~4,300+ (body). End of brief.*
