
---

# Part II — Identity & IR

## §4 IR contract & occurrence model

**Source:** `.research/librarification/04-ir-audit.md` · **Plane:** SHARED

### 4.1 Current shape (load-bearing facts)

- `ir::Index` is a **flat table**: `root_ids: Vec<NudoxPath>` + `entries_by_path: HashMap<NudoxPath, Entry>` (`workspace/ir/entry.rs:26–35`); nesting is reconstructed from `members: Option<Vec<NudoxPath>>` and inverted to `member_of` in the graph. The "stable integer IDs" doc comment is stale.
- `NudoxPath` (`entry.rs:15–24`) is the IR's only inter-entry pointer: `External { path, dependency }` | `Local(PathBuf)`. Producers commonly store a single component embedding `::`; the graph linker and `SymbolTable::path_segments` re-split on `::`/`.` (`workspace/compiler/graph/symtab.rs:21–51`). This re-splitting is fragile and becomes the moniker extractor's problem (§6).
- Convention: fields typed `Option<Vec<T>>` distinguish absent from empty (`ir/lib.rs:1–16`). serde is the active wire path; facet is reserved/inactive.
- Pipeline stages `Collected → Indexed → Roots` (`ir/pipeline/pipeline.rs:5–68`); the root heuristic (single-component Local without `::`) is crude and misses crate-level modules.
- **Three identity systems co-exist** and must not be conflated (→ §0.3 identity stack): NudoxPath (version-free within a package), graph IRI `Symbol/{lang}%2F{pkg}%2F{fq}` (version-agnostic), heart `SymbolId` = UUIDv5(instance ‖ versioned PackageId ‖ segments) (version-coupled).

### 4.2 Target changes

| Change | Why |
|---|---|
| Add `EntryContentHash` per entry (computed at seal; = §6 `content_hash`) | symbol-level fan-out (GD-28) |
| Producers stamp `SymbolMoniker` materials (descriptor chain, kind) per entry | §6 emission requirements |
| `OccurrenceSet` stays in ir as the persisted reference corpus; CstSet demoted (GD-26) | trees are transient (GD-11) |
| Keep ir heart-free (GD-23); ids are stamped by compiler-core at seal | crate purity |
| Fix root heuristic to be producer-declared rather than inferred | correctness for §9 projection |
| `BlobManifest` (in blob crate) points at `ir_ref`, `references_ref`, NEW `occurrences_ref`, `files[]` | GD-10 |

Implementation steps: **S4.1** producer-declared roots + stale-comment cleanup; **S4.2** `EntryContentHash` field + seal-time computation (needs §6 normalizers); **S4.3** moniker material emission from all six producers (checklist per producer, table in §6.5); **S4.4** occurrences_ref plumbing (= S3.2). *Acceptance:* snapshot tests (`tests/snap_*.rs`) extended to assert monikers + hashes are populated and deterministic across double-runs.

---

## §5 Symbol identity foundations

**Sources:** `.research/librarification/05-symbol-identity-academic.md`, `06-symbol-identity-industrial.md` · **Plane:** SHARED (informs §6)

The question "when are two symbols across generations the same?" has 20+ years of literature (origin analysis, refactoring detection, clone genealogy) and a decade of industrial systems (SCIP/LSIF, Kythe, Glean/Glass, Unison, rust-analyzer, git rename detection). Both bodies of evidence converge on the same conclusions, which §6 freezes into a design:

1. **Identity is layered, not binary.** The tier lattice T0 (content-identical) → T1 (path+name exact) → T2 (rename) → T3 (move) → T4 (signature evolution) → T5 (split/merge) → T6 (semantic near-duplicate) → T7 (truly new/deleted) comes straight from the literature (Kim/Pan/Whitehead WCRE'05 origin relationships; Godfrey & Zou TSE'05 merge/split origin analysis; Kim & Notkin MSR'06 matching). Precision degrades sharply as the identity claim gets more ambitious — thresholds must be tier-specific.
2. **Feature sets that dominate empirically:** name similarity, body/text similarity, signature, call/reference structure, container/path. The classic matcher quartet (name / declaration / metrics / call-relation) survives in every modern tool.
3. **Cost asymmetry is universal:** when history is written once and read many times, **false merges are worse than false splits**. A false merge pollutes embeddings reuse, API-evolution edges, and lineage stores; a false split is recoverable by later re-matching. Operational translation (frozen into §6): raise auto-link thresholds, allow soft multi-candidate edges, never force 1:1 assignment on marginal scores.
4. **Industrial verdict on monikers:** moniker equality is **not** continuity. SCIP symbol strings (`<scheme> <manager> <package-name> <version> <descriptors>`) are the best-designed cross-language coordinate — nudox adopts the descriptor alphabet wholesale — but every industrial system either keeps monikers version-qualified (index-scoped) or leaves cross-version continuity to the consumer. Continuity requires (a) moniker stability by design, (b) content-address sameness (Unison's AST hashes, git blob OIDs, Cursor chunk hashes), or (c) an explicit successor edge (Unison patches, Glean stacked DBs, git rename edges). nudox implements **all three**, in that order of preference.
5. **Graceful degradation** (Sourcegraph search-based nav fallback) justifies §6's Soft/Provisional edge decisions: a stale or missing precise edge falls back to suggestion-grade evidence, never silently upgraded.

Key references carried into the design: scip.proto grammar + `scip` crate 0.9.x (interop target), Kythe path+signature VNames, Unison content-addressed definitions, RefactoringMiner's rename/move detection accuracy culture (human-validated oracles), git's similarity-scored rename detection (the model for `git_rename_score` as *evidence*, not verdict).

---

## §6 Moniker & lineage (normative)

**Source:** `.research/librarification/19-symbol-moniker-rfc.md` (design freeze) · **Plane:** SHARED · **Crate:** `moniker`

This section is normative. Constants and grammar are frozen at `moniker_grammar_version = 1`, `normalizer_version = 1`, `matcher_version = 1.0.0`.

### 6.1 Identity lattice

| ID | Versioned? | Role |
|---|---|---|
| `LineageMoniker` | **No** | Cross-generation continuity join key (tier-0 coordinate) |
| `GenerationSymbol` (SCIP-shaped) | Yes | Within-generation index row, SCIP interop, debug |
| Graph `Symbol` IRI | No | Versionless graph node key (existing `link::symbol_iri`) |
| `PackageId` (heart) | Yes | Package *instance* id — keep as-is |
| `PackageStemId` (NEW, heart) | No | origin + name; package-level join across versions |
| `SymbolInstanceId` (= today's SymbolId) | Yes | Per-generation, per-instance UUID |
| `LineageId` (NEW, optional) | No | Derived stable UUID for UI/history; never primary |

**Frozen rule:** never silently rewrite monikers to preserve history. Continuity is **edges + moniker equality**, never mutation of IDs.

### 6.2 Moniker grammar (frozen, SCIP descriptor alphabet)

```
<LineageMoniker>   ::= <scheme> " " <ecosystem> " " <package-name> " " <descriptor>+
<GenerationSymbol> ::= <scheme> " " <ecosystem> " " <package-name> " " <version> " " <descriptor>+

<scheme>     ::= "nudox-rust" | "nudox-ts" | "nudox-go" | "nudox-java"
               | "nudox-py" | "nudox-cs" | "nudox-nix"
<ecosystem>  ::= "cargo" | "npm" | "gomod" | "maven" | "pypi" | "nuget" | "nix" | "."
<descriptor> ::= <name>"/"   (namespace/Module)   | <name>"#"  (type)
               | <name>"."   (term)               | <name>"("<disambig>?")." (method)
               | "["<name>"]" (type-param)        | "("<name>")" (parameter)
               | <name>":"   (meta)               | <name>"!"  (macro)
```

Spaces escape as double-space per SCIP. Package-name canonicalization per ecosystem: cargo crate name as published; npm full `@scope/pkg`; gomod module path; maven `groupId/artifactId` (slash inside the package field only); pypi PEP 503 normalized; nuget id as published; nix flake/attr path. Package rename is a **package-level** lineage event (new lineage roots unless an explicit rename patch maps them). **Kind hard filter:** matching never links across incompatible descriptor kinds except explicit T5 rules. Schemes never collide; cross-language identity is out of scope for auto lineage.

### 6.3 Content hashes (frozen)

All hashes BLAKE3-256 via `heart::ContentHash`, domain-separated with length-prefixed tags (JobKey style): `H(tag, parts…) = BLAKE3(le_u64(len)‖part …)`.

| Field | Tag | Canonical input |
|---|---|---|
| `sig_hash` | `sig-v1` | normalizer_version, kind tag, visibility, unqualified name, normalized signature (params sorted by position w/ canonical types, returns, generics+bounds, sorted attrs, receiver kind). **Excludes** docs, body, spans. Types canonicalize via resolved moniker when linkable, else stable pretty-print (sorted unions, primitive enum names, floats at 15 sig digits) |
| `body_hash` | `body-v1` | normalized token stream from tree-sitter/LanguageSpec: drop whitespace/pure comments (doc comments → doc_hash), keep identifiers as-is (Type-1/2 identity; no de Bruijn erasure in v1), canonical string-literal escapes. Empty body well-defined |
| `doc_hash` | `doc-v1` | doc text: NFC, per-line trailing trim, LF, ≥3 blank lines → 2 |
| `ref_hash` | `ref-v1` | sorted unique outbound (and inbound when available, else `in_absent` flag) intra-package lineage-moniker strings; unresolved paths tagged `u:` |
| `embed_key` | — | embedding cache key (drives GD-28 re-embed skip) |
| `content_hash` | `sym-v1` | full early-cutoff key: normalizer_version ‖ lineage_moniker ‖ part hashes |
| `simhash64` | — | winnowing/SimHash of body tokens — LSH blocking ONLY, never identity |

### 6.4 Matching cascade T0–T7 (frozen thresholds)

Pipeline order is mandatory; each tier removes matched endpoints from the pools (T4 labels, T5 multi-maps exempt):

```
T0 content short-circuit → T1 moniker exact → T2 same-parent rename
→ T3 move(+rename), LSH-blocked → T4 signature-evolution labels
→ T5 split/merge/extract/inline → T6 residual bipartite + embed assist → T7 birth/death
```

| Tier | Match rule | Confidence / auto-link |
|---|---|---|
| **T0** | kind = ∧ body_hash = ∧ sig_hash = (hash join); boilerplate guard: duplicate body_hash requires parent or name equal for auto | 1.00 unique · 0.99 with parent · 0.85 soft among duplicates; auto ≥ 0.99. Edge: `Identical`, or `Moved`/`Renamed` when hash-equal but moniker differs |
| **T1** | lineage_moniker = ∧ kind = | 0.99 sig= · 0.97 sig-compatible · 0.95 body-changed; always auto for unique keys. Edges: `SameMoniker[SigChanged|BodyChanged]` |
| **T2** | same parent_moniker, same kind, unmatched | unique sig_hash → 0.98 · body_sim ≥ 0.85 → 0.96 · body_sim ≥ 0.70 ∧ (name_JW ≥ 0.80 ∨ ref_jaccard ≥ 0.50) → 0.93 · body_sim ≥ 0.70 alone → 0.90 (auto only if unique best ∧ margin ≥ 0.05). Body metric: token Jaccard (Jaro-Winkler secondary < 30 tokens). Greedy assignment; Hungarian on dense collision |
| **T3** | cross-parent move: as T2 with `body_cross_parent_auto = 0.75`, simhash blocking (hamming ≤ 3), top-k = 20, margin 0.05 | analogous; edges `Moved`/`MovedRenamed` |
| **T4** | annotates T1/T2 edges with signature-evolution labels — not a bipartite pass | `SignatureEvolved` |
| **T5** | split/merge via coverage: `split_cover = 0.80` | `Split`/`Merged`/`ExtractedFrom`/`InlinedInto`; may leave partial coverage |
| **T6** | residual bipartite composite score (weights: body .35 name .20 sig .15 ref .15 path .10 doc .05); embed assist ≥ 0.80 | auto ≥ 0.92 (margin 0.08) → `Related`; 0.75–0.92 soft → `MaybeSame` |
| **T7** | residual unmatched | `Added`/`Removed` markers |

**Global gates:** `auto_lineage_min_conf = 0.93`, `embed_skip_min_conf = 0.95` (an embed is only reused across a lineage edge at ≥ 0.95), mass-delete guard: if > 40% of a package's symbols disappear, demote all auto edges to Provisional pending review.

### 6.5 Edge model and storage

```rust
pub enum LineageKind { Identical, SameMoniker, SameMonikerSigChanged, SameMonikerBodyChanged,
    Renamed, Moved, MovedRenamed, SignatureEvolved, BodyEdited, Split, Merged,
    ExtractedFrom, InlinedInto, Related, MaybeSame, Reexport, Copy, Manual, Added, Removed }

pub struct LineageEdge {
    pub edge_id: ContentHash,          // H(canonical edge bytes) — CAS/dedup
    pub from: SymbolEndpoint, pub to: SymbolEndpoint,   // generation-qualified
    pub kind: LineageKind, pub tier: u8, pub confidence: f32,
    pub decision: Decision,            // Auto | Soft | Manual | Provisional
    pub features: FeatureVector,       // body/name/sig/ref/path/doc/embed sims + git_rename_score
    pub evidence: Vec<Evidence>,       // HashEquality, MonikerEqual, GitFileRename, AuthorPatch, …
    pub matcher_version: semver::Version,
    pub moniker_grammar_version: u32, pub normalizer_version: u32,
    pub created_at: Timestamp,
}
```

`SymbolEndpoint` carries stem + version + PackageId + instance id + moniker + SCIP string + graph IRI, so every consumer joins on its own layer. **Storage:** SQLite (INDEX and REGISTRY) `lineage_edges` tables + CAS blobs for evidence payloads; Terminus receives Auto edges for hot packages only (GD-13). Producers' emission checklist: every entry gets moniker materials + part-hash inputs at produce time; `generate` seals fingerprints into the IR blob (S4.2/S4.3).

### 6.6 Rename/move/split/merge policies

Auto edges write lineage and permit embed reuse (at ≥ 0.95); Soft edges surface as "maybe related" in UI and never skip embeds or merge Terminus identities; Manual edges (author patches) override everything and are never GC'd; Provisional edges await the next generation's corroboration. Reexports are alias edges, not origin. Cross-package `Copy` detection is default-off for auto.

### 6.7 Implementation steps

- **S6.1** `moniker` crate: grammar types, parser/printer, FQN→descriptor extractors per language (from IR kind + NudoxPath). *Acceptance:* SCIP round-trip tests against `scip` 0.9 corpus; golden moniker snapshots per fixture package (reuse `tests/snap_*` corpus).
- **S6.2** heart: `PackageStemId`, part-hash structs, `normalizer_version` plumbing.
- **S6.3** Normalizers (sig/body/doc/ref canonical bytes) in compiler-core seal path; determinism tests (double-run equality) per language.
- **S6.4** Matcher: T0+T1 only (hash joins) + `LineageEdge` writes; ship behind a flag. **S6.5** T2/T3 with LSH blocking. **S6.6** T4 labels + T5 split/merge. **S6.7** T6 embed-assist (needs §12 local embeddings). **S6.8** Manual patch API + mass-delete guard + UI soft-edge surfacing (→ §19 lineage timeline).
- Ordering: S6.1→S6.3 before any §7 symbol-level fan-out; T0–T2 (S6.4/S6.5) before §10 lineage-in-Terminus; T5/T6 are post-MVP.
