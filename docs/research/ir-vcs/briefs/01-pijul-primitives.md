# Research Brief: Pijul / libpijul Primitives for an IR-Symbol VCS

> **Status (Rev 3.2):** Research context, non-normative — feeds `design/IR-NATIVE-VCS-DESIGN.md`. The design adopts this brief's libpijul-log plumbing and hybrid checkpoint+tail materialize, but **rejects** its conflict-as-state direction: the `SolveSymbolConflict`/`SolveOrderConflict` atom sketches (§4.3), `IrView::conflicts()` (§4.4), the conflict-as-graph stance (§5.3), and `SymbolLocalId` vertex keying (§4.3) are all superseded by K5 (IntroId-keyed atoms) and K14 (single-writer linear apply; races fail closed as `ApplyError::ConcurrentWrite`).

**Scope.** This brief surveys patch-theoretic foundations and the concrete architecture of Pijul 1.0 beta / `libpijul`, with the design goal of a custom VCS that versions **Intermediate Representation (IR) symbols** rather than files/lines. It is not a Nudox product design document; it is a primitives research note for mapping decisions.

**Primary sources.** Pijul manual (why_pijul, theory), blog posts (partials 2020; hybrid snapshot/patch 2022; 1.0 beta), `libpijul` 1.0.0-beta.10 docs.rs API and source tree, Sanakirja docs, Mimram & Di Giusto (arXiv:1311.3903), Angiuli et al. Homotopical Patch Theory (arXiv:1211.2851 / ICFP 2014 / JFP expanded).

---

## 1. Patch theory foundations

### 1.1 Darcs patch theory vs Pijul

**Darcs** is the classic patch-based DVCS. Its mental model is: a repository is an unordered *set* of patches; patches may be reordered when they *commute*; merge is “pull and commute into a linear sequence.” Two primitive operations structure the theory:

1. **Commutation.** Given consecutive patches \(A; B\), produce \(B'; A'\) (if they commute) such that both sequences have the same effect on content.
2. **Inversion.** Every patch \(A\) has \(A^{-1}\) with \(A; A^{-1} \equiv \mathrm{id}\) (up to identity patches / bookkeeping).

Darcs’ hard problems historically:

- **Conflictors / mergers.** When patches fail to commute during merge, Darcs invents special “conflictor” patches. The design has been iterated for years; conflicts and commutation interact poorly.
- **Exponential merge.** Conflict resolution can re-commute huge sequences; worst cases are infamous (hours for tiny edits). See Darcs FAQ “exponential merge problem.”
- **Repositories as sequences, not conflict-tolerant states.** The pristine is typically a materialized file tree without a first-class conflict graph.

**Pijul** (Pierre-Étienne Meunier; theory influenced by Mimram et al.) keeps the *user-facing* patch algebra (commutation, inverses, associative apply) but changes the *machine representation*:

| Dimension | Darcs | Pijul |
|---|---|---|
| Core object | Patch sequences + conflictors | Directed graph of content vertices + labeled edges |
| Pristine | Materialized files | Conflict-tolerant graph (“pristine”) cached in Sanakirja |
| Conflicts | Special patch types; commute fails | First-class graph states (zombies, cycles, non-orderable vertices) |
| Conflicting patches | **Never** commute (classically) | **Always** commute (both apply; conflict is in the graph) |
| Merge complexity | Can explode with history | Apply is local to patch size × conflict size × log history |
| Identity of patches | Stable hashes; reorder changes meaning of sequences | Stable hashes; order of independent patches is irrelevant to state |

From the Pijul manual (*Why Pijul?*):

> Pijul is a mostly formally-correct version of Darcs’ theory of changes and a new algorithm for merging changes. Its main innovation compared to Darcs is to use a better data structure for its pristine… Conflicting changes always commute in Pijul and never commute in Darcs.

**Key algebraic properties Pijul claims:**

1. **Associativity of apply.** Applying \(A\) then \((BC)\) equals applying \((AB)\) then \(C\). Git’s three-way merge does *not* guarantee this; the classic “bad merge” (Tahoe/Zooko line-shuffle example) shows snapshot systems can reorder non-conflicting edits.
2. **Inversion.** Every change \(A\) has \(A^{-1}\) such that the *content state* after \(A; A^{-1}\) matches neither applied (both still appear in the log).
3. **Commutation of independent changes.** If \(A\) and \(B\) could have been written without knowledge of each other, then \(AB\) and \(BA\) produce the **same repository state**; only the log order differs. Hashes of \(A\) and \(B\) **do not change** when reordered (unlike Git rebase).
4. **Dependency as the dual of commutation.** Either \(A\) and \(B\) commute, or \(A\) depends on \(B\), or \(B\) depends on \(A\).

### 1.2 Commutation, inverses, conflicts as first-class

#### Commutation (operational meaning)

Independence is *file/identity-based*, not line-offset-based:

- File and directory identities are **not** path strings; they are vertices introduced by the change that created them (inode + name vertex pair).
- Renaming a file **commutes** with editing its contents.
- Editing file \(x\) **commutes** with adding unrelated file \(y\).

This is what makes partial clones sound: you can pull only the changes that touch a subgraph of file identities and still produce new changes that push back into the full history without rewriting hashes (blog: *Commutation and scalability*, Dec 2020).

#### Inverses

`Change::inverse` exists in libpijul. At the graph level, inversion maps:

- `NewVertex` insertions → edge maps that delete (or vice versa),
- flag transitions reverse,
- file add ↔ file del, etc.

Unrecord (`unrecord::unrecord`) removes a change from a channel when it has no reverse-dependencies remaining on that channel—essentially “apply the inverse and drop the log entry,” with bookkeeping for pseudo-edges and contexts.

#### Conflicts as first-class (not working-tree markers)

Pijul does **not** block commits on conflicts. After applying changes, conflicts are *detected* by inspecting the pristine graph. Manual theory lists three structural conflict types for content:

1. **Order conflict / disconnected order:** two alive vertices with no directed path either way (incomparable in the partial order).
2. **Cycle conflict:** paths in both directions between alive vertices.
3. **Zombie vertices:** a vertex with both alive and dead incident edges (e.g., edit into a context deleted in parallel without knowledge).

File-level conflicts (after contracting name↔inode edges):

- disconnected file graph (add into deleted directory),
- parallel renames (DAG instead of tree),
- duplicate names,
- cyclic directory moves.

**Contrast with Git:** Git writes conflict markers into the working tree and refuses to commit until resolved. **Contrast with Darcs:** conflictors encode conflicts into patch algebra; commute is blocked.

**CRDT observation (manual Theory):** the pristine graph operations (add vertices/edges; map known edge labels) are conflict-free as a *datatype*. Pijul is a CRDT that *models textual conflicts accurately*—not a CRDT that pretends conflicts do not exist. Edge labels (which change introduced an edge) are essential; without them, parallel delete + inverse delete collapses incorrectly.

### 1.3 Categorical models (pushouts; Mimram / Meunier)

#### Mimram & Di Giusto — *A Categorical Theory of Patches*

- **arXiv:** https://arxiv.org/abs/1311.3903 (cs.LO, 13 Nov 2013)
- **Journal:** Electronic Notes in Theoretical Computer Science 298:283–307, MFPS 2013  
  DOI: https://doi.org/10.1016/j.entcs.2013.09.018
- **Authors:** Samuel Mimram, Cinzia Di Giusto

**Abstract (key claims):**

> We begin by defining a category of files and patches, where the operation of merging the effect of two coinitial patches is defined by pushout. Since two patches can be incompatible, such a pushout does not necessarily exist in the category, which raises the question of which is the correct category to represent and manipulate files in conflicting state. We provide an answer by investigating the free completion of the category of files under finite colimits, and give an explicit description of this category: its objects are finite sets labeled by lines equipped with a transitive relation and morphisms are partial functions respecting labeling and relations.

**Formal sketch:**

- Objects: file states (sequences / ordered labeled sets of lines).
- Morphisms: patches (edits) from one state to another.
- **Merge of coinitial patches** \(f: A \to B\), \(g: A \to C\) is the **pushout** \(B \leftarrow A \rightarrow C \Rightarrow B \rightarrow D \leftarrow C\), when it exists.
- Pushouts fail when edits are incompatible (true conflicts).
- **Free finite-colimit completion** enlarges the category so pushouts always exist; objects become:
  - finite sets of line-labeled elements,
  - equipped with a **transitive relation** (not necessarily a total order),
  - morphisms: partial functions preserving labels and relations.

This is essentially “files may become **posets / preorders of lines** during conflict,” which is the categorical precursor of Pijul’s “graph of lines with alive/dead edges.”

**Relation to Pijul implementation.** Pijul’s pristine is an *engineered realization* of living in (something like) that completed category:

- total order of lines ≜ path-connected alive subgraph without branches,
- conflict ≜ loss of unique total order (forks, cycles, zombies),
- merge ≜ apply both patches as graph updates (colimit-like), then optionally record a resolution patch that restores a total order.

Meunier’s engineering contribution is making this:

1. **append-only and CRDT-like** at the graph operation level,
2. **efficient** via Sanakirja + pseudo-edges,
3. **multi-file** with name/inode separation so renames commute with content edits.

There is no single published “Meunier theorem” paper equivalent to Mimram’s MFPS note that fully axiomatizes production Pijul; the normative theory text is https://pijul.org/manual/theory.html plus blog posts.

### 1.4 Homotopical patch theory (Angiuli et al.)

#### Primary references

1. **Homotopical Patch Theory** — Carlo Angiuli, Edward Morehouse, Daniel R. Licata, Robert Harper  
   - Conference: ICFP 2014, ACM SIGPLAN  
     https://dl.acm.org/doi/10.1145/2628136.2628158  
   - PDF (CMU): https://www.cs.cmu.edu/~rwh/papers/htpt/paper.pdf  
   - Expanded JFP version: https://www.cs.cmu.edu/~rwh/papers/htpt/jfp.pdf  
   - Angiuli expanded: https://carloangiuli.com/papers/hpt-expanded.pdf  
   - **arXiv:** https://arxiv.org/abs/1211.2851 (also related notes around 2012–2014; HoTT blog post cites ICFP paper)  
   - HoTT blog announcement: https://homotopytypetheory.org/2014/09/01/homotopical-patch-theory/

**Core idea (from abstract / blog):**

> We show how patch theory can be developed in homotopy type theory. Our formulation separates formal theories of patches from their interpretation as edits to repositories. A patch theory is presented as a higher inductive type. Models of a patch theory are given by maps out of that type, which, being functors, automatically preserve the structure of patches.

**Technical structure:**

- **Repository states** ~ points of a space (type).
- **Patches** ~ paths (identity/equality proofs) between states—**proof-relevant**.
- **Patch laws** (e.g. commute diagrams, inverses) ~ **higher paths** (2-paths / homotopies).
- Patch theories as **higher inductive types (HITs)** with constructors for:
  - points (contexts / repository shapes),
  - paths (atomic patches),
  - path-constructors encoding equations (commutation squares, etc.).
- An **interpretation / model** is a map out of the HIT into a concrete type of repositories; functoriality automatically preserves composition, inverses, and laws.

**Why this matters for an IR-symbol VCS:**

- Separates *syntax of patches* from *semantics on a domain*. For Nudox-like systems, the domain is not “lines of text” but “symbol tables / IR graphs.”
- Encourages stating **patch laws as higher equalities** (what must commute) independently of storage.
- Shows that some useful functions (`optimize : (p : Patch) → Σ(q : Patch). p = q`) map into **contractible** types—computational content still matters even when homotopy-trivial.

**Limitation relative to Pijul:** Homotopical patch theory is a *logical framework* and toy/realistic models (including text-file patch theory in later sections of the paper); it is **not** a description of libpijul’s graph engine. Use it for axiomatization and law design; use Mimram + Pijul theory for operational merge semantics.

### 1.5 Related formal work (pointers)

| Work | URL | Relevance |
|---|---|---|
| Mimram & Di Giusto, Categorical Theory of Patches | https://arxiv.org/abs/1311.3903 | Pushout merge; colimit completion = conflict states |
| Angiuli et al., Homotopical Patch Theory | https://arxiv.org/abs/1211.2851 ; https://www.cs.cmu.edu/~rwh/papers/htpt/jfp.pdf | HIT patch theories; models as functors |
| Darcs patch theory (various) | e.g. https://www.cs.tufts.edu/~nr/cs257/archive/jason-dagit/tmr-darcs.pdf | Classical commute/inverse; conflict problem statement |
| Pijul Theory manual | https://pijul.org/manual/theory.html | Production graph model, pseudo-edges, multi-file, Merkle |
| Pijul Why | https://pijul.org/manual/why_pijul.html | Associativity, Darcs comparison, conflict philosophy |
| jneem early explainer | https://jneem.github.io/pijul/ | “Graggles” (files as DAGs of lines) |

---

## 2. libpijul / Pijul 1.0 beta architecture

### 2.1 Source layout and packaging

- **CLI crate:** `pijul` (e.g. 1.0.0-beta.20 on docs.rs) — https://docs.rs/crate/pijul/1.0.0-beta.20/source/
- **Core library:** `libpijul` (e.g. 1.0.0-beta.10 documented) — https://docs.rs/libpijul/1.0.0-beta.10/libpijul/  
  Source browse: https://docs.rs/crate/libpijul/1.0.0-beta.10/source/  
  Nest: https://nest.pijul.com/pijul/libpijul
- **License (libpijul crates.io / docs.rs):** **GPL-2.0-or-later**
- **Key dependency:** `sanakirja` ^1.4 — **MIT OR Apache-2.0** (safe to reuse independently)
- **Hashing:** BLAKE3 for change identity; curve25519/ed25519 for keys; optional zstd-seekable for tags/snapshots

**libpijul module map (1.0.0-beta.10):**

```
alive/          # alive-subgraph retrieval / output helpers
apply/          # apply_change*, apply_local_change*, workspaces
change/         # Change, Hunk, Atom, NewVertex, EdgeMap, serialization
changestore/    # ChangeStore trait (filesystem / memory)
diff/           # diff algorithms used by record
output/         # materialize working copy / archive; Conflict enum
pristine/       # Vertex, EdgeFlags, Channel*, Txn*, Sanakirja backend
record/         # RecordBuilder: working copy → change hunks
unrecord/       # remove changes from a channel
working_copy/   # working copy backends
fs.rs           # inode/path tracking in tree tables
vertex_buffer.rs
missing_context.rs
tag.rs          # (feature zstd) tags / compressed snapshots
```

Public re-exports include: `Vertex`, `Inode`, `EdgeFlags`, `Hash`, `Merkle`, `ChangeId`, `ChannelRef`, `ArcTxn`, `apply_change_arc`, `RecordBuilder`, `Conflict`, etc.

### 2.2 Core types (real API names)

#### Graph identity

```rust
// libpijul::pristine::Vertex
#[repr(C)]
pub struct Vertex<H> {
    pub change: H,              // ChangeId (internal) or Hash (external)
    pub start: ChangePosition,  // byte offset in that change's contents
    pub end: ChangePosition,    // exclusive end
}
// Vertex::ROOT — all zeros; is_root()
// start_pos() / end_pos() → Position<H>
// len() = end - start in bytes
```

A vertex is **not** “line number N of file F.” It is a **byte interval introduced by a specific change**. Same text introduced twice is two vertices. Content identity survives reordering of contexts.

```rust
// Position = change + offset (point, not interval)
// ChangePosition — position of a byte within a change
// ChangeId — compact internal id mapped to external Hash in txn tables
// Hash — external identity of a change (BLAKE3-based enum Hash)
// Merkle — channel state identifier (multiplicative / discrete-log style; see §2.7)
// Inode — working-copy file/directory identity mapping graph ↔ FS
```

#### Edges

```rust
// EdgeFlags bitflags (libpijul::pristine::EdgeFlags)
BLOCK    // "internal"/status-bearing edge vs pure order after vertex splits
PSEUDO   // not part of any change; restores alive connectivity / marks conflicts
FOLDER   // filesystem hierarchy edge (non-transitive in the file sense)
PARENT   // reverse direction of an edge (graph stores both directions)
DELETED  // source or target (depending on PARENT) is deleted
```

Helpers: `is_deleted()`, `is_parent()`, `is_folder()`, `is_block()`, `is_alive_parent()`.

`SerializedEdge` is the on-disk target half of an edge. Every real edge has a reverse (`PARENT`) counterpart.

#### Changes and hunks

```rust
// Type alias
pub type Change = LocalChange<
    Hunk<Option<Hash>, Local>,
    Author
>;

// Conceptual layout of LocalChange / Change:
struct Change {
    offsets: Offsets,           // TOC for seeking inside change file
    hashed: Hashed {            // contributes to Hash
        version: u64,           // VERSION / VERSION_NOENC
        contents_hash,          // hash of contents section
        changes: Vec<Hunk<...>>,
        metadata: Vec<u8>,
        dependencies: Vec<...>,
        extra_known: Vec<...>,  // context knowledge for zombie detection
        header: ChangeHeader,   // authors, message, timestamp, ...
    },
    unhashed: Option<serde_json::Value>, // optional TOML/JSON extras
    contents: Vec<u8>,          // raw introduced bytes (may be large)
}
```

**Atoms** — the two primitive graph operations:

```rust
pub enum Atom<Change> {
    NewVertex(NewVertex<Change>), // introduce bytes + alive edges around them
    EdgeMap(EdgeMap<Change>),     // map existing edge labels (delete, folder ops, ...)
}
```

This matches the theory manual’s “only two kinds of actions.”

**BaseHunk** variants (user-level structure wrapping atoms):

| Variant | Role |
|---|---|
| `FileMove { del, add, path }` | rename / reparent |
| `FileDel { del, contents?, path, encoding? }` | delete file |
| `FileUndel { ... }` | inverse of delete |
| `FileAdd { add_name, add_inode, contents?, path, encoding? }` | create file (name + inode vertices) |
| `SolveNameConflict` / `UnsolveNameConflict` | name conflict resolution |
| `Edit { change, local, encoding? }` | content edit |
| `Replacement { change, replacement, local, encoding? }` | replace hunk |
| `SolveOrderConflict` / `UnsolveOrderConflict` | order conflict resolution |
| `ResurrectZombies { change, local, encoding? }` | zombie repair |
| `AddRoot` / `DelRoot` | repository root bookkeeping |

`Change::inverse`, `Change::serialize`, `Change::deserialize`, `Change::hash`, `Change::size_no_contents` (ops without full contents) are first-class.

#### Channels and transactions

- **Channel** ≈ named branch: an ordered log of applied changes + a graph state + a Merkle state.
- Default channel name: `DEFAULT_CHANNEL = "main"`.
- Traits: `ChannelTxnT`, `ChannelMutTxnT`, `GraphTxnT`, `GraphMutTxnT`, `DepsTxnT`, `TreeTxnT`, `TxnT`, `MutTxnT`.
- `ChannelRef<T>`, `ArcTxn<T>` for shared access.
- Sanakirja backend: `pristine::sanakirja::{Txn, MutTxn}`.

`MutTxnTExt` methods of interest:

- `apply_change`, `apply_change_rec`, `apply_deps_rec`, `apply_change_ws`
- `apply_local_change` / `apply_recorded`
- `unrecord`
- `add_file` / `add_dir` / `move_file` / `remove_file`

`TxnTExt`: `log`, `reverse_log`, `has_change`, `is_alive`, `current_state`, `touched_files`, `follow_oldest_path`, `iter_adjacent`, …

### 2.3 Files as graphs of line (byte-interval) vertices + edges

From https://pijul.org/manual/theory.html:

> …a repository [is] a single file represented by a directed graph \(G=(V,E)\) of lines of text, where each vertex \(v \in V\) represents a line of text, and an edge from \(u\) to \(v\), labelled with a change number \(c\), could be read as “according to change \(c\), line \(u\) comes before \(v\).”

**Refined model in production Pijul:**

1. Vertices are **byte intervals** `change:c0 start:i end:j`, not necessarily single lines. Diff algorithm chooses splits; theory notes you could split by AST if desired.
2. **Alive** edges encode current order/status; **deleted** edges retain history without removing vertices.
3. **Pseudo-edges** reconnect the alive subgraph after deletions so output complexity does not scan dead spans.
4. **BLOCK** flag separates “this edge carries deletion/alive status for its target block” from mere ordering after splits.
5. **Multi-file:** `FOLDER` edges; each file/dir has a **name vertex** and an **inode vertex** so:
   - directory rename ⟂ file rename,
   - file rename ⟂ edit at start of file.

**Output (materialization):** walk the alive subgraph in topological order, emit contents of alive vertices, detect conflicts (`output::Conflict`) when the graph is not a single total order per file / not a tree of files.

### 2.4 Sanakirja: channels as DB forks / B-trees

**Sanakirja** (https://docs.rs/sanakirja/, nest.pijul.com/pijul/sanakirja): transactional on-disk COW datastructures; concurrent readers + exclusive writer.

Properties relevant to Pijul:

- Memory-mapped file (or in-memory), page size 4096.
- Fixed number of **root versions** at env init: supports MVCC-like “readers keep old root; writer copies youngest root onto oldest free root.”
- B-trees via `sanakirja::btree` with `Storable` keys/values.
- Root page holds array of root pointers (`set_root` / `root_db`) to multiple databases.

**How Pijul uses it:**

- `.pijul/pristine` — Sanakirja DB holding:
  - per-channel graphs (adjacency),
  - change id ↔ external hash maps,
  - changeset logs (apply order → ChangeId + Merkle),
  - dependency / reverse-dependency tables,
  - tree/inode tables for working copy tracking,
  - remotes metadata,
  - touched-files indices, etc.
- **Channel fork** is essentially **forking B-tree roots** (COW): cheap branch creation without copying the whole graph.
- Changes themselves live in `.pijul/changes` (compressed change files), not necessarily fully in the pristine DB—pristine stores the *applied graph*, change files store *operations + contents*.

This is the engineering answer to Darcs’ “patch-only” performance: pristine is a **cache of the colimit** of applied patches.

### 2.5 Change application algorithm (apply / unrecord)

**Record path (working copy → change):**

1. `RecordBuilder` / `record::` diff pristine vs working copy (`diff` module, `Algorithm` enum).
2. Produce local hunks with `Local` / `LocalByte` positions.
3. `globalize` maps internal `ChangeId` positions to external `Option<Hash>`.
4. Compute dependencies (`change::dependencies` / `full_dependencies`) from contexts + deleted edges + `extra_known`.
5. Serialize; hash; store via `ChangeStore::save_change`.
6. `apply_local_change` updates channel graph + inode tables.

**Apply path (change → channel graph):**

Complexity cited by Meunier (partials post):

\[
O(|p| \cdot |c| \cdot \log |H|)
\]

where \(|p|\) = change size, \(|c|\) = size of largest conflict involving \(p\), \(|H|\) = history size (log factor from DB).

High-level steps (conceptual):

1. Ensure dependencies applied (`apply_change_rec`).
2. For each atom:
   - `NewVertex`: split existing vertices if inserting mid-block; insert new vertex; add alive edges (and reverses); possibly pseudo-edges.
   - `EdgeMap`: locate edges by (source, target, flags, introduced-by); map flags (e.g. mark DELETED); repair contexts (`missing_context`, `repair_context` timers).
3. Update alive connectivity; detect zombie/cycle issues.
4. Append to channel log; update **Merkle** state.
5. Optionally output conflicts when materializing.

**Unrecord:**

- Invert effects of a change on the channel graph.
- Fail or recurse if other applied changes depend on it.
- Recompute missing contexts (historical bugs fixed around re-detecting contexts after unapply).
- Used also for “archive at state”: unrecord forward until Merkle matches, then output (see `archive_prefix_with_state`—**destructive to channel** unless forked first).

### 2.6 Identity of changes (hashes)

- Change identity is a cryptographic **Hash** (BLAKE3 family in modern libpijul).
- **Hashed section** includes graph ops, dependency lists, contents_hash, header metadata—not necessarily full raw `contents` bytes in the same way for partial download (see partials).
- Design (2020 partials post): split change file into:
  1. **operations section** (graph edits + length metadata + hash of contents),
  2. **contents section** (actual bytes).
- Hash of change ≜ hash of operations section (which embeds contents hash)—enables downloading ops without contents while still authenticating later.
- **Internal ChangeId:** short id used in graph tables; mapped bijectively to external Hash in the pristine.

### 2.7 Version / channel state identifiers (Merkle)

Because independent patches commute, a linear chain hash (Git-style) would order-dependently identify the same set. Pijul uses a **commutative state identifier** `Merkle`:

- Empty state ≈ identity \(1\).
- Applying change with hash \(h\) to state \(V = e^v\) yields \(V_h = e^{v \cdot h}\) (discrete-log style multiplicative scheme; related to homomorphic hashing).
- Forging a state that claims to include certain patches without knowing the discrete log is hard.

API: `current_state(channel) -> Merkle`, returned alongside apply timestamps from `apply_change`.

### 2.8 Partial clones / partials

Blog: https://pijul.org/posts/2020-12-19-partials

**Mechanism:**

- Each change references the **file identities** (inode vertices) it touches.
- Because independent file subgraphs commute, you may clone/pull only changes related to paths of interest.
- New edits on the partial history still commute with omitted changes when pushed—**without rewriting hashes**.

**Large binary optimization:**

- Ops-only download for intermediate full-file replacements; fetch latest contents only.
- Security caveat: malicious server can defer content mismatch until unrecord forces content download.

### 2.9 Hybrid snapshot / patch (compressed Sanakirja, 2022)

Posts:

- https://pijul.org/posts/2022-01-07-compressed-sanakirja/ — *Towards a hybrid snapshot/patch version control system*
- https://pijul.org/posts/2022-01-08-beta/ — *Announcing Pijul 1.0 beta*

**Problem:** cold clone of huge history must apply every change; apply is fast-ish but not free at \(10^5+\) changes.

**Approach:**

- Treat a Sanakirja pristine at a point in time as a **snapshot**.
- Compress with **seekable Zstd** so random access to pages remains possible.
- Tags / `tag` feature (`zstd-seekable`) package compressed repository states.
- Hybrid model: patches for incremental collaboration; snapshots for fast bootstrap and navigability—**backward compatible** with pure patch apply.

This is the pragmatic convergence of patch theory with Git-like snapshot distribution, without abandoning commutative patch identity.

### 2.10 License implications

| Component | License | Implication for proprietary / permissively licensed IR VCS |
|---|---|---|
| **libpijul** | **GPL-2.0-or-later** | Linking/distributing a derivative typically requires GPL on the combined work. Embedding as the core of a non-GPL product is a legal non-starter without a commercial exception from copyright holders. |
| **pijul CLI** | GPL (same family) | Same. |
| **Sanakirja** | MIT OR Apache-2.0 | Reusable freely; excellent COW B-tree primitive. |
| **Ideas / algorithms / papers** | Not code copyright | Clean-room reimplementation of graph+patch algebra is fine; do not copy GPL source. |
| **Change format / wire** | De facto defined by libpijul | Interop with Pijul repos implies careful reverse-engineering or GPL tooling side-car. |

**Practical recommendation:** treat libpijul as a **reference implementation**, not a library dependency, unless the entire product is GPL-compatible.

---

## 3. What to REUSE vs REIMPLEMENT

### 3.1 Using libpijul as a library

**Pros:**

- Battle-tested apply/record/unrecord.
- Channel fork via Sanakirja is free.
- Partial pull, Merkle states, change serialization done.
- `ChangeStore` trait allows non-disk backends.

**Cons:**

- **GPL-2.0-or-later** viral to the product.
- File/line ontology is hard-wired: `BaseHunk` paths, encodings, inode/fs tree, diff-on-bytes.
- Documentation coverage ~11% on docs.rs; API is powerful but expert-level.
- Still beta; breaking changes across 1.0.0-beta.x.
- IR symbols are not files: forcing symbols into fake files loses semantic commute structure (or fakes it poorly).

**API surface you would actually call:**

```text
MutTxnTExt::apply_recorded / apply_change_rec
RecordBuilder + diff::Algorithm
Change::{serialize, deserialize, inverse, hash}
TxnTExt::{log, current_state, follow_oldest_path, is_alive}
output::archive / Conflict
changestore::ChangeStore
pristine::sanakirja::{Env, Txn, MutTxn}  // via libpijul wrappers
```

**Maturity:** usable for production-adjacent tools in the Pijul ecosystem; not a stable “libgit2 for patches” with long-term API guarantees.

### 3.2 Extracting only mathematical primitives into a custom engine

**Reuse as specification (not code):**

1. **Two atom types:** introduce node; map edge labels.
2. **Labeled edges with introducer identity** (critical for correct inverse/delete).
3. **Pseudo-edges** for alive connectivity.
4. **Conflicts as graph predicates**, resolved by later patches.
5. **Commutative state monoid** (Merkle / homomorphic hash of patch set).
6. **Explicit dependencies** from contexts + deleted edges + semantic extras.
7. **Ops/contents split** for large payloads.
8. **COW B-tree snapshot** of applied state (Sanakirja or reimplementation; LMDB/redb/sled are weaker fits for multi-version roots).

**Reimplement completely:**

- Vertex payload = IR symbol / IR node id, not byte interval of a file.
- Edge vocabulary = IR relations (e.g. `contains`, `after`, `types`, `calls`, `binds`), not only linear order + folder.
- Hunk ADT = symbol-level operations (see §4).
- Diff = structural / semantic IR diff, not line diff.
- Materialization = zero-copy IR view / query index, not filesystem checkout.
- Sync = CAS want/have of change objects + optional snapshots.

### 3.3 Mapping table: files/lines → IR symbols

| Pijul concept | File/line meaning | IR-symbol meaning (proposed) |
|---|---|---|
| Repository | Project tree | Package-lineage store (all generations of a package’s IR) |
| Channel | Branch (`main`) | Package mainline / feature branch / generation lane |
| Change | Patch (hash-stable) | `SymbolDelta` — set of symbol-graph atoms |
| Vertex | Byte interval in a change | IR entry identity: `(delta_id, symbol_local_id)` or content-addressed symbol id introduced by delta |
| Edge (order) | Line \(u\) before \(v\) | Symbol ordering *only if needed* (e.g. enum variants); else typed IR edges |
| Edge `FOLDER` | Directory tree | Module / namespace / package path tree (`NudoxPath`) |
| Name vertex | Path component | `NudoxPath` segment or export name |
| Inode vertex | File identity | **Symbol container** or **compilation unit** identity (stable across renames) |
| `Edit` hunk | Line insert/delete | Symbol body replace / span edit |
| `FileAdd` | New file | Introduce new symbol or new unit |
| `FileMove` | Rename path | Rename `NudoxPath` / re-export; identity preserved |
| Alive subgraph | Current file text | Current package IR snapshot (materialized view) |
| Conflict (order) | Parallel inserts | Parallel incompatible IR rewrites of same symbol region |
| Zombie | Edit in deleted context | Edit to symbol deleted in parallel |
| Pseudo-edge | Skip deleted lines | Skip tombstoned symbols in traversal indexes |
| Partial clone | Path filter | Filter by package path prefix / symbol namespace |
| Contents section | File bytes | Optional large payloads (string literals, debug info, codegen blobs) |
| Merkle state | Commutative set id | Package generation id = homomorphic hash of applied `SymbolDelta`s |
| Working copy | Files on disk | Zero-copy IR arena / mmap’d symbol table |

---

## 4. Concrete mapping proposal sketches

### 4.1 Repository → package-lineage store

```text
PackageLineageStore
├── deltas/                    # content-addressed SymbolDelta objects (CAS)
│   └── <blake3>.delta
├── pristine/                  # COW graph DB (Sanakirja-like)
│   ├── channels/
│   │   ├── main               # applied set + graph root + Merkle
│   │   ├── gen/1.4.2          # release generation channel
│   │   └── exp/rewrite-ir
│   ├── symbol_graph           # vertices + typed edges
│   ├── path_tree              # NudoxPath ↔ unit inode
│   └── deps_index             # delta dependencies / reverse deps
├── snapshots/                 # optional compressed pristine snapshots
└── config
```

A “checkout” is not required for core operations; the pristine *is* the IR.

### 4.2 Channels = mainline / branches / generations

```rust
struct Channel {
    name: String,              // "main", "gen/1.4.2", "pr/123"
    log: Vec<(ApplyTs, DeltaId, Merkle)>,
    graph_root: PagePtr,       // COW B-tree root
    state: Merkle,             // commutative applied-set id
}
```

- **mainline:** continuous integration of SymbolDeltas.
- **generation channel:** frozen set of deltas that define a published package version (tag = snapshot of channel state).
- **feature branch:** fork of graph root; cheap COW.

Unlike Git branches, two channels that applied the same unordered set of commuting deltas share the same `Merkle` state even if logs differ.

### 4.3 Changes = SymbolDelta between package versions

```rust
/// Content-addressed patch for IR symbols
struct SymbolDelta {
    header: DeltaHeader,           // authors, message, timestamp
    deps: Vec<DeltaId>,            // hard dependencies
    extra_known: Vec<DeltaId>,     // for zombie/conflict detection
    atoms: Vec<SymbolAtom>,
    /// Large optional payloads (string tables, blobs), CAS-separated
    contents: BlobRef,
}

enum SymbolAtom {
    /// Introduce a new symbol node (payload in contents or inline)
    NewSymbol {
        local: SymbolLocalId,
        kind: SymbolKind,          // Fn, Type, Effect, Module, ...
        edges: Vec<IntroEdge>,     // alive edges at introduction
    },
    /// Map edge labels (delete, retarget, change relation)
    EdgeMap {
        edges: Vec<EdgePatch>,     // (src, dst, old_flags, new_flags, introduced_by)
    },
    /// Path / namespace ops (FOLDER analogue)
    PathAdd { name: NudoxPathSeg, unit: UnitId },
    PathDel { ... },
    PathMove { ... },
    /// Explicit resolution patches
    SolveSymbolConflict { ... },
    SolveOrderConflict { ... },
    ResurrectZombies { ... },
}

struct SymbolVertex {
    delta: DeltaId,
    local: SymbolLocalId,          // analogue of ChangePosition range
    // OR: stable SymbolId = H(delta || local) 
}
```

**Semantic dependencies (beyond Pijul’s syntactic contexts):**

```rust
// hooks may add:
deps.extend(typechecker.symbols_referenced(edit));
deps.extend(parent_module_delta);
```

This is exactly what Pijul’s theory allows: “Hooks and scripts may add extra language-dependent dependencies based on semantics.”

### 4.4 Working copy = materialized zero-copy IR view

```rust
trait IrView {
    fn symbol(&self, id: SymbolId) -> Option<SymbolRef<'_>>; // zero-copy
    fn children(&self, id: SymbolId) -> impl Iterator<Item = SymbolId>;
    fn lookup_path(&self, path: &NudoxPath) -> Option<UnitId>;
    fn conflicts(&self) -> Vec<IrConflict>;
}

// Materialization strategies:
// 1. Fully in pristine graph (query in place) — preferred
// 2. Arena snapshot for compiler pipeline (rebuild from alive subgraph)
// 3. Export to files only for human debug (lossy)
```

**Record** becomes: compiler or editor produces a candidate IR arena → structural diff against channel’s alive IR → `SymbolDelta`.

### 4.5 Sync protocol: Pijul pull/push vs CAS want/have

**Pijul-style (set of changes):**

```text
Client: have(channel_state_merkle, set_of_delta_ids)
Server: missing = needed_deltas_not_in(have) respecting deps
Server → Client: delta ops [+ contents on demand]
Client: apply_change_rec in any valid order
```

Because of commutation, sync is **set reconciliation**, not “fast-forward my ref.”

**CAS want/have (recommended hybrid for IR store):**

```text
1. Exchange Merkle channel states (or list of delta ids).
2. Compute delta id set difference (want/have).
3. Fetch SymbolDelta objects from CAS by id (HTTP, ipfs, s3, ...).
4. Fetch contents blobs lazily (ops/contents split).
5. Optional: fetch compressed pristine snapshot near target Merkle to skip long apply.
6. Apply remaining deltas; verify Merkle.
```

Comparison:

| | Pijul pull/push | Pure CAS | Hybrid (recommended) |
|---|---|---|---|
| Unit | Change files + channel | Objects by hash | Deltas in CAS + channel Merkle |
| Partial | Path-filtered changes | Prefix-filtered symbol ids | Both |
| Bootstrap | Apply all / tags | N/A | Snapshot + tail deltas |
| Auth | Nested/SSH/HTTP | Capability URLs | Same + signed deltas |

---

## 5. Performance characteristics

### 5.1 Known scalability profile

| Operation | Behavior | Notes |
|---|---|---|
| Apply one change | \(O(|p|\,|c|\log|H|)\) | Conflict size dominates |
| Record/diff | Depends on WC size | File-oriented; IR structural diff can be better or worse |
| Channel fork | ~O(1) COW roots | Sanakirja strength |
| Cold clone history | Historically painful | Motivation for 2022 snapshots |
| Partial clone | Excellent when commute holds | File/symbol identity isolation |
| Output/materialize | Alive-subgraph walk | Pseudo-edges avoid dead scans |
| Disk | Pristine growth + change store | Early Pijul had disk bloat; improved pre-1.0 |

### 5.2 Binary / large-file handling

- Changes split **ops vs contents**; intermediate binary replacements need not all download contents.
- Still, binary-as-single-vertex or whole-file delete+add creates large contents blobs and coarse conflicts (any parallel edit conflicts).
- No Git-LFS equivalent built-in; partial contents fetch is the main tool.
- For IR: put huge payloads (object code, debug) in **side CAS**, keep symbol graph small.

### 5.3 When graph-of-lines is wrong for structured IR

Graph-of-lines (or byte intervals) fails when:

1. **Identity is semantic, not positional.** Renaming a function should be a path/name edge map, not delete+add of all lines (which breaks blame and commute with edits).
2. **Reorder is not a conflict.** Sorting imports or normalizing IR should not create order conflicts; line graphs treat reorder as massive delete+insert.
3. **Typed edges matter.** “Function A calls B” is not recoverable from line order; IR needs a multigraph with relation labels.
4. **Non-linear structure.** ASTs, CFGs, type graphs are not total orders; forcing total order invents false conflicts.
5. **Fine-grained commute.** Two edits to different fields of a struct should commute; line-based diff may overlap spans spuriously.
6. **Canonicalization.** IR often wants alpha-equivalence / hash-consing; byte identity fights that unless vertices are introduced at a canonical symbol id layer.

**When graph-of-lines is still right:**

- Pretty-printed IR dumps for debugging,
- documentation strings,
- unstructured notes,
- any payload that truly is text.

**Design stance for IR VCS:** keep Pijul’s **patch algebra and conflict-as-graph** core; replace the **vertex = text line** instantiation with **vertex = symbol / IR node**, and replace linear order edges with a **schema of IR relations** plus optional order edges where order is semantically meaningful (e.g. match arms if order matters).

### 5.4 Complexity hazards unique to IR

- High-churn symbol tables → many small deltas; need aggregation or snapshots.
- Global typecheck-driven dependency edges can over-serialize (destroy commutation); keep semantic deps **minimal**.
- Hash-consed symbols: if vertex id = content hash, “edit” is delete+add and loses continuous identity—prefer **introduction identity** (Pijul-style) plus optional content hash attributes.
- Conflict UX: must surface symbol-level conflicts to tooling, not conflict markers in files.

---

## Key excerpts

### From Pijul Theory (manual)

> As a first approximation, one can think of a repository as a single file represented by a directed graph \(G=(V,E)\) of lines of text… edges labelled with a change number \(c\)… “according to change \(c\), line \(u\) comes before \(v\).”

> We have just described the two basic kinds of actions in Pijul. There are no other. One kind adds vertices to the graph, along with “alive” edges around them, and the other kind maps an existing edge label onto a different one.

> …Pijul implements a conflict-free replicated datatype (CRDT): indeed, we’re just adding vertices and edges to a graph, or mapping edge labels which we know exist because of dependencies. However, Pijul’s datastructure models, in a conflict-free way, the conflicts that can happen over a text file.

### From Why Pijul (manual)

> Pijul is a mostly formally-correct version of Darcs’ theory of changes and a new algorithm for merging changes… Conflicting changes always commute in Pijul and never commute in Darcs.

> In Pijul, for any two changes \(A\) and \(B\), either \(A\) and \(B\) can be applied in any order, or \(A\) depends on \(B\), or \(B\) depends on \(A\).

### From Commutation and scalability (2020)

> …any two changes that could have been written independently always commute… both orders will yield the exact same result…

> Complexity of apply is in \(O(|p||c|\log|H|)\), where \(|p|\) is the size of the change, \(|c|\) is the size of the largest conflict in which \(p\) is involved, and \(|H|\) is the number of edits made since the start of the repository.

### From Mimram & Di Giusto (arXiv:1311.3903 abstract)

> …merging the effect of two coinitial patches is defined by pushout… free completion of the category of files under finite colimits… objects are finite sets labeled by lines equipped with a transitive relation and morphisms are partial functions respecting labeling and relations.

### From Angiuli et al. (Homotopical Patch Theory)

> A patch theory is presented as a higher inductive type. Models of a patch theory are given by maps out of that type, which, being functors, automatically preserve the structure of patches.

### From libpijul API (1.0.0-beta.10)

```rust
pub struct Vertex<H> { pub change: H, pub start: ChangePosition, pub end: ChangePosition }
pub enum Atom<Change> { NewVertex(NewVertex<Change>), EdgeMap(EdgeMap<Change>) }
pub type Change = LocalChange<Hunk<Option<Hash>, Local>, Author>;
// EdgeFlags: BLOCK | PSEUDO | FOLDER | PARENT | DELETED
// License: GPL-2.0-or-later
```

---

## Decision matrix: adopt libpijul | fork primitives | pure inspiration

| Criterion | **A. Adopt libpijul** | **B. Fork primitives** (clean-room engine; reuse Sanakirja ideas/code) | **C. Pure inspiration** (design from papers + custom store) |
|---|---|---|---|
| **License fit for closed/permissive product** | Poor (GPL-2.0-or-later) | Good if no GPL code copied; Sanakirja is MIT/Apache | Good |
| **Time-to-correct merge algebra** | Best (already implemented) | Medium–High | Highest risk of subtle inverse/zombie bugs |
| **IR symbol impedance** | Poor (files/bytes) | Good (you control vertex schema) | Best flexibility |
| **Partial sync / commute** | Built-in for paths | Reimplement using same theory | Reimplement |
| **Snapshots / hybrid bootstrap** | Tags + compressed pristine directionally available | Build on COW DB + zstd-seekable | Design own |
| **Interop with Pijul repos** | Native | Only if clone format | None |
| **Performance control for IR** | Limited (line diff assumptions) | Full | Full |
| **Maintenance burden** | Track betas / nest | Own apply correctness forever | Own everything |
| **Recommended when** | GPL OK + text-ish IR dump versioning | **Default for IR-symbol VCS** | Research prototype / radical algebra changes |

### Recommended posture for an IR-symbol VCS

**Primary: B — fork the primitives (clean-room), not the GPL codebase.**

1. **Steal the math, not the bytes:** Mimram pushout/colimit intuition; Pijul two-atom graph; labeled edges with introducer; pseudo-edges; conflicts first-class; commutative Merkle; ops/contents split; dependency = context + semantic hooks.
2. **Optionally reuse Sanakirja** (permissively licensed) or a similar COW B-tree for channel forks and pristine graphs.
3. **Do not encode symbols as fake text files** inside stock libpijul; you will inherit wrong conflict/diff granularity and GPL.
4. **Use Homotopical Patch Theory** to write down patch laws (what commutes, what is an inverse, what resolutions mean) as an executable/spec layer later.
5. **Use pure inspiration (C)** only for experimental algebras (e.g. fully CRDT OT hybrids, bidirectional transformations) after B’s core is solid.

### Minimal “correctness checklist” if reimplementing

- [ ] Independent patches commute and preserve Merkle.
- [ ] Inverses restore content state; parallel delete + one inverse ≠ wipe both.
- [ ] Zombie detection uses `extra_known` / edge introducer ids.
- [ ] Pseudo-edges keep materialize sublinear in dead history.
- [ ] Name/identity vertices separate from payload so renames ⟂ edits.
- [ ] Resolution is itself a patch (no silent state).
- [ ] Apply is associative on non-conflicting and conflicting inputs.
- [ ] Snapshot bootstrap yields same Merkle as replaying deltas.

### One-page mental model

```text
                    ┌─────────────────────────────┐
   SymbolDelta      │  atoms: NewSymbol | EdgeMap │
   (CAS object)     │  deps + extra_known         │
                    └─────────────┬───────────────┘
                                  │ apply
                                  v
                    ┌─────────────────────────────┐
   Channel          │  COW graph pristine         │
   pristine         │  alive IR multigraph        │
                    │  Merkle = Π hashes          │
                    └─────────────┬───────────────┘
                                  │ materialize
                                  v
                    ┌─────────────────────────────┐
   IrView           │  zero-copy symbol arena     │
                    │  conflicts as first-class   │
                    └─────────────────────────────┘
```

**Bottom line:** Pijul is the best *production* embodiment of patch theory with first-class conflicts; libpijul is the reference—but **GPL and file/line ontology** push an IR-symbol VCS toward a **clean-room primitive engine** guided by Mimram/Pijul theory and Homotopical Patch Theory laws, optionally standing on Sanakirja-style COW storage, with deltas as CAS objects and channels as package lineage generations.

---

## Bibliography (selected)

1. Pierre-Étienne Meunier et al., *Pijul Manual: Why Pijul?* https://pijul.org/manual/why_pijul.html  
2. Pierre-Étienne Meunier, *Pijul Manual: Theory* https://pijul.org/manual/theory.html  
3. Pierre-Étienne Meunier, *Commutation and scalability* (2020-12-19) https://pijul.org/posts/2020-12-19-partials  
4. Pierre-Étienne Meunier, *Towards a hybrid snapshot/patch VCS* (2022-01-07) https://pijul.org/posts/2022-01-07-compressed-sanakirja/  
5. Pierre-Étienne Meunier, *Announcing Pijul 1.0 beta* (2022) https://pijul.org/posts/2022-01-08-beta/  
6. `libpijul` 1.0.0-beta.10 docs https://docs.rs/libpijul/1.0.0-beta.10/libpijul/  
7. `sanakirja` docs https://docs.rs/sanakirja/  
8. Samuel Mimram, Cinzia Di Giusto, *A Categorical Theory of Patches*, arXiv:1311.3903 https://arxiv.org/abs/1311.3903  
9. Carlo Angiuli, Edward Morehouse, Daniel R. Licata, Robert Harper, *Homotopical Patch Theory*, arXiv:1211.2851; JFP/ICFP https://www.cs.cmu.edu/~rwh/papers/htpt/jfp.pdf  
10. HoTT blog: Homotopical Patch Theory https://homotopytypetheory.org/2014/09/01/homotopical-patch-theory/  
11. Pijul FAQ https://pijul.org/faq  

---

*End of research brief.*
