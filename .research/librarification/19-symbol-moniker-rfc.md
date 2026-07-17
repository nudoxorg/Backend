# RFC-19: Symbol Moniker + Lineage (Implementable Freeze)

**Status:** FROZEN design (not implemented)  
**Date:** 2026-07-16  
**Research inputs:** `05-symbol-identity-academic`, `06-symbol-identity-industrial`, `04-ir-audit` (identity), `07-terminus-tiering` (lineage), `08-incremental` (symbol content hash)  
**Normative for:** producers, generation differ, embed/index invalidation, Terminus + SQLite INDEX lineage  
**Non-goals:** build code, master plan rewrite, multi-package provenance product

---

## 0. Problem statement (frozen)

**Question:** When are two symbols, across package generations, *the same symbol*?

**Answer (normative):** Sameness is a **tiered, evidence-graded origin relation** between generation-scoped **instances**, not a single ID equality. Continuity is represented by:

1. **Layer A — `LineageMoniker`:** SCIP-inspired, version-stripped string coordinate of the *named API entity* within a package.
2. **Layer B — content hashes:** `sig_hash` / `body_hash` / `doc_hash` / `embed_key` over versioned normalizers.
3. **Layer C — `LineageEdge`:** explicit successor edges when moniker and/or hashes disagree (rename, move, split, merge, …).

**Asymmetry (non-negotiable):** False merges corrupt lineage, embeddings, and Terminus history. False splits only lose continuity. Prefer false-split over false-merge. Auto-link thresholds are high; soft edges (`maybe_same`) never drive embed-skip or hard identity.

**Adjacent-generation only:** Matchers run only on \(G_{n-1} \rightarrow G_n\). Transitive identity is path closure with confidence aggregation (min or product); never invent non-adjacent edges without a path.

---

## 1. Identity lattice (normative)

| ID / key | Scope | Versioned? | Role |
|----------|-------|------------|------|
| `LineageMoniker` | package + language + descriptors | **No** | Cross-generation continuity join key (tier-0 coordinate) |
| `GenerationSymbol` / SCIP form | moniker + package version | **Yes** | Within-generation index row, SCIP interop, debug |
| Graph `Symbol` IRI | `Symbol/{lang}%2F{pkg}%2F{fq}` | **No** | Versionless graph node key (existing `link::symbol_iri`) |
| `PackageId` (heart) | origin + name + **version** | **Yes** | Package *instance* (generation) id — keep as-is |
| `PackageStemId` (new) | origin + name only | **No** | Package-level join across versions |
| `SymbolInstanceId` (= today's `SymbolId`) | instance salt + versioned PackageId + path | **Yes** | Per-generation, per-Terminus-instance UUID |
| `LineageId` (new) | stem + lineage moniker (or UUID chain head) | **No** | Optional stable UUID for UI/history; derived, not primary |
| Content hashes | symbol payload | n/a | Change detection, CAS, embed keys |

**Frozen rule:** Never silently rewrite monikers to preserve history. Continuity is **edges + moniker equality**, not mutation of IDs.

---

## 2. Moniker grammar (frozen)

### 2.1 BNF

SCIP-compatible descriptor alphabet (`scip.proto`). Version is **in** the package triple for generation indexes; **stripped** for lineage.

```
<scheme>           ::= "nudox-rust" | "nudox-ts" | "nudox-go" | "nudox-java"
                     | "nudox-py" | "nudox-cs" | "nudox-nix" | <future-scheme>
<ecosystem>        ::= "cargo" | "npm" | "gomod" | "maven" | "pypi" | "nuget"
                     | "nix" | "."   ; "." = unknown / unpublished
<package-name>     ::= UTF-8 without unescaped space ; spaces → double-space
<version>          ::= UTF-8 package version canonical string | "."
<name>             ::= UTF-8 ; spaces escaped as double space; `#` `/` `.` `(` `)`
                     | `[` `]` `!` `:` escaped per SCIP rules if they appear in names

<descriptor>       ::= <namespace> | <type> | <term> | <method>
                     | <type-parameter> | <parameter> | <meta> | <macro>
<namespace>        ::= <name> "/"
<type>             ::= <name> "#"
<term>             ::= <name> "."
<meta>             ::= <name> ":"
<macro>            ::= <name> "!"
<method>           ::= <name> "(" <method-disambiguator>? ")."
<type-parameter>   ::= "[" <name> "]"
<parameter>        ::= "(" <name> ")"
<method-disambiguator> ::= UTF-8  ; overload / trait / receiver disambiguation

; Continuity key (version-stripped)
<LineageMoniker>   ::= <scheme> " " <ecosystem> " " <package-name> " " <descriptor>+

; Generation-qualified (SCIP-shaped; version in package triple)
<GenerationSymbol> ::= <scheme> " " <ecosystem> " " <package-name> " " <version> " " <descriptor>+

; Compact alternate for logs/UI (not a parse primary)
<GlassForm>        ::= <ecosystem> "/" <lang-short> "/" <package-name> "/" <descriptor-path>
```

**`moniker_grammar_version`:** integer field on every IR seal / Index metadata. This freeze is **`moniker_grammar_version = 1`**.

### 2.2 Descriptor suffix table (normative)

| Suffix | Token | IR kind mapping (primary) |
|--------|-------|---------------------------|
| Namespace | `/` | `Module` |
| Type | `#` | `RecordType`, `SumType`, `UnionType`, `TypeAlias`, `PrimitiveType`, `TraitDef` (type-side) |
| Term | `.` | free `Function`, `Constant`, `Variable`, `Field`, `Event`, `Macro` (value-side macros may use `!`) |
| Method | `().` | methods / associated functions with receiver semantics |
| TypeParameter | `[name]` | generic param (rarely lineage-tracked alone) |
| Parameter | `(name)` | formal parameter (rarely lineage-tracked alone) |
| Meta | `:` | metadata symbols |
| Macro | `!` | macros |

**Kind hard filter:** matching never links across incompatible descriptor kinds (function ↔ type, method ↔ field, module ↔ term) unless an explicit T5 multi-map rule applies (extract method from type body is still function↔function).

### 2.3 Scheme and ecosystem

| Language frontend | `scheme` | Default `ecosystem` |
|-------------------|----------|---------------------|
| rust-analyzer producer | `nudox-rust` | `cargo` |
| OXC / TS | `nudox-ts` | `npm` |
| go oracle | `nudox-go` | `gomod` |
| javadoc / java | `nudox-java` | `maven` |
| pyrefly / python | `nudox-py` | `pypi` |
| Roslyn / C# | `nudox-cs` | `nuget` |
| snix / nix | `nudox-nix` | `nix` |

**Invariant:** schemes never collide even if descriptor strings look similar. Cross-language identity is out of scope for auto lineage.

### 2.4 Package name canonicalization

| Ecosystem | Canonical package name rule |
|-----------|----------------------------|
| cargo | crate name as published (`-` kept; not rustc crate renames like `odd_duck`) |
| npm | full scoped name `@scope/pkg` (one descriptor-space string; `/` in name is literal in package field, not a descriptor) |
| gomod | module path as published |
| maven | `groupId/artifactId` with `/` **inside package-name field only** |
| pypi | normalized distribution name (PEP 503) |
| nuget | package id as published |
| nix | flake/attr path agreed by nix producer |

**Package rename** (registry rename of the package itself) is a **package-level** lineage event, not symbol moniker equality. Monikers under the new package name are new lineage roots unless an explicit package-rename patch maps them.

### 2.5 FQN → descriptors algorithm

Producers emit monikers from IR `NudoxPath` + kind, not ad hoc strings.

```
fn path_to_descriptors(path: NudoxPath, kind: EntryKind, disambig: Option<&str>) -> Vec<Descriptor> {
    // 1. Split path into segments with ONE shared normalizer:
    //    path_segments(path) — same fn used by SymbolTable, linker, EntryUri
    // 2. Classify each intermediate segment as Namespace `/` by default
    // 3. Classify the leaf by kind:
    //      Module        → Namespace `/`  (leaf is a module)
    //      Record/Sum/…  → Type `#`
    //      Function free → Term `.`   if no receiver
    //      Function meth → Method `().` with disambiguator if needed
    //      TraitDef      → Type `#`   (trait as type-like)
    //      TraitImpl     → Meta `:` under impl moniker (see §2.7)
    //      Macro         → Macro `!`  or Term `.` per language table
    // 4. For nested types (mod::Type::method):
    //      Namespace… / Type# Method().
}
```

**Shared segment splitter (mandatory, single implementation):**

```
path_segments(s) -> Vec<String>:
  split on "::" first; if single component contains only ".", also split on ".";
  filter empty; reject mixed ambiguous forms in producer tests.
```

This freezes the IR-audit gap: producers, SymbolTable, linker, and moniker emission **must** call the same function (`heart` or `ir` crate).

### 2.6 Method disambiguators (per language)

| Language | Disambiguator content | Example |
|----------|----------------------|---------|
| Rust inherent | empty if unique name under type; else `impl` index or trait path | `Client#get().` vs `Client#get(Display).` |
| Rust trait method | trait path FQ | `Client#clone(Clone).` |
| Java / C# | JVM/CLI signature erasure of params | `Foo#bar(int,java.lang.String).` |
| Go | empty if unique; receiver type name if methods collide across types in same package packaging | `Handler#ServeHTTP().` |
| TypeScript | param type pretty forms if overload | `api#fetch(string).` |
| Python | empty default; arity or param names if overload-like | `mod/Class#run().` |

**Rule:** Disambiguator is part of the moniker. Changing only the disambiguator without a body/sig match is a **new** moniker (T1 miss → cascade may still link via body).

### 2.7 TraitImpl, re-exports, aliases

| Case | Moniker policy |
|------|----------------|
| **Canonical definition** | Moniker of the defining item (implementation identity). |
| **Re-export / `pub use`** | Do **not** mint a second lineage moniker as definition. Store **export alias** facts: `ExportAlias { export_path_moniker, target_lineage_moniker }`. |
| **Type alias** | Own moniker as Type `#`; lineage is independent of target unless body_hash of alias target equality is used only as soft evidence. |
| **TraitImpl** | Moniker form: `nudox-rust cargo pkg Trait# for Type#:` (meta) or structured descriptors `Impl/` namespace — **frozen choice:** use meta descriptor leaf `impl:` under a synthetic namespace path `impl/{Trait}/{Type}:` with scheme-local escaping. Exact string fixtures live in golden tests (§11). |
| **Dual-emitted methods** (inline TraitMethod + Entry::Function) | **One** moniker: the free/associated function entry path that graph projects; collapse duplicates at seal. |

### 2.8 Version strip / qualify

```
fn lineage_moniker(gen: &GenerationSymbol) -> LineageMoniker {
    // drop version field; keep scheme, ecosystem, package-name, descriptors
}

fn generation_symbol(m: &LineageMoniker, version: &str) -> GenerationSymbol {
    // insert version into package triple (SCIP order)
}

fn to_scip(gen: &GenerationSymbol) -> String { gen.as_scip_string() }  // scip crate 0.9.x
fn from_scip(s: &str) -> Result<GenerationSymbol> { … }
```

**Interop:** optional SCIP export/import via `scip` crate **0.9.x**. Do not reintroduce LSIF moniker graphs.

### 2.9 Graph IRI relationship

Existing:

```
symbol_iri(lang, package, fq_name) -> Symbol/{lang}%2F{pkg}%2F{fq}
```

**Frozen bridge:**

```
graph_fq_from_moniker(m: &LineageMoniker) -> String:
  // render descriptors to historical fq form used by from_ir:
  // namespaces joined by "::", types/terms as today ("foo::Bar::baz")
  // MUST match path_segments inverse of current projection
```

Graph IRI remains versionless and **moniker-derived**. When moniker changes (rename), graph shows remove+add in `PackageVersion.declares` **and** a `LineageEdge` connects old→new instances. Symbol document `@id` under the **new** moniker's FQ is a new IRI; history is edges + optional Terminus document history on each IRI separately.

**Product UI** walks `LineageEdge` for "formerly known as", not IRI equality alone.

### 2.10 Examples

**Rust `acme-http` v1.0.0 — method:**

```
LineageMoniker:     nudox-rust cargo acme-http client/Client#fetch().
GenerationSymbol:   nudox-rust cargo acme-http 1.0.0 client/Client#fetch().
Graph IRI:          Symbol/rust%2Facme-http%2Fclient::Client::fetch
Glass:              cargo/rust/acme-http/client/Client#fetch().
```

**Rename fetch→get, same body (v1.1.0):**

```
LineageMoniker:     nudox-rust cargo acme-http client/Client#get().
LineageEdge:        …fetch().@1.0.0 → …get().@1.1.0  kind=Renamed tier=T2 conf≥0.95
embed_key:          unchanged (body_hash + sig_hash + doc_hash stable)
```

**npm package:**

```
nudox-ts npm @acme/http 2.0.0 src/client.ts/Client#request().
```

---

## 3. Content hashes (frozen)

### 3.1 Hash function

All symbol hashes use **BLAKE3-256** via existing `heart::ContentHash` (`ContentHash::of_bytes` / builder). Domain-separate with length-prefixed tags (JobKey style).

```
H(tag, parts...) = BLAKE3(
  for each part: le_u64(len) || part
  with leading tag part first
)
```

### 3.2 Fields per sealed symbol

| Field | Type | Definition |
|-------|------|------------|
| `normalizer_version` | `u32` | Frozen policy version of all normalizers below; **v1 = 1** |
| `sig_hash` | `ContentHash` | API surface identity |
| `body_hash` | `ContentHash` | Implementation identity (empty body allowed) |
| `doc_hash` | `ContentHash` | Documentation identity |
| `ref_hash` | `ContentHash` | Sorted outbound + inbound intra-package monikers/paths (optional part; required when refs available) |
| `embed_key` | `ContentHash` | Embedding cache key |
| `content_hash` | `ContentHash` | Full early-cutoff key for SymbolDelta |
| `simhash64` | `u64` | Winnowing/SimHash of body tokens for LSH blocking only |

### 3.3 Canonical bytes

#### `sig_hash` input (`tag = b"sig-v1"`)

Include, in fixed order:

1. `normalizer_version` as le_u32  
2. `kind` tag (IR `Entry` variant name, stable string)  
3. `visibility` enum  
4. Unqualified `name` (UTF-8)  
5. Normalized signature structure:
   - parameters: sorted by position; each: name (if public API), type_canonical, defaulted?, variadic?
   - outputs / return types canonical  
   - generics: names + bounds canonical  
   - attributes: Async/Unsafe/Const/… sorted  
   - receiver kind if any  
6. **Exclude:** docs, body tokens, spans, aliases, deprecation note text (deprecation *flag* may be included as bool)

**Type canonicalization (`type_canonical`):**

1. Prefer resolved moniker string for `TypeReference` when linker resolves within package.  
2. Else pretty-print with stable rules: no whitespace variance, sorted union/intersection members, primitives via IR enum names.  
3. Never include source spans.  
4. Floats in `ConstExpr`: quantize to canonical decimal string with fixed precision **or** reject float const in sig domain (prefer: format with 15 significant digits + `e` form).  

`sig_hash = H(b"sig-v1", normalizer_version, canonical_sig_bytes)`

#### `body_hash` input (`tag = b"body-v1"`)

1. Prefer **normalized token stream** from tree-sitter / LanguageSpec over raw text.  
2. Normalization:
   - Drop whitespace-only and pure-comment tokens (not doc comments — those go to `doc_hash`).  
   - Keep identifiers as-is for Type-1/2 identity (do **not** de Bruijn-erase public names in v1).  
   - Keep keywords, operators, literals (string literals normalized: escape canonical).  
   - Optional future `body_hash_type2` with identifier normalization — **not** used for auto lineage in v1.  
3. Empty body (abstract/interface): body bytes = `b""` → still well-defined hash.  
4. `body_hash = H(b"body-v1", normalizer_version, lang, token_bytes)`

#### `doc_hash` input (`tag = b"doc-v1"`)

1. Markdown/doc comment text.  
2. Normalize: Unicode NFC, trim trailing whitespace per line, LF newlines, collapse ≥3 blank lines to 2.  
3. Empty/missing docs → hash of empty.  
4. `doc_hash = H(b"doc-v1", normalizer_version, doc_bytes)`

#### `ref_hash` input (`tag = b"ref-v1"`)

1. `ref_out`: sorted unique list of target **lineage moniker strings** (or unresolved path strings tagged `u:`).  
2. `ref_in`: sorted unique list of source monikers if available at seal time; else omit section with flag `in_absent`.  
3. Only intra-package edges for matching; cross-package stubs use graph ~extern monikers.  
4. `ref_hash = H(b"ref-v1", normalizer_version, out_joined, in_joined)`

#### `content_hash` (full) (`tag = b"sym-v1"`)

```
content_hash = H(b"sym-v1",
  normalizer_version,
  lineage_moniker.as_bytes(),
  kind,
  sig_hash.as_bytes(),
  body_hash.as_bytes(),
  doc_hash.as_bytes(),
  ref_hash.as_bytes()  // or zero32 if refs unavailable
)
```

**Note:** moniker is inside `content_hash`. A pure rename with identical sig/body/doc/refs still changes `content_hash` but **not** `embed_key` (see below). SymbolDelta consumers use part hashes, not only full content_hash.

#### `embed_key` (`tag = b"embed-key-v1"`)

```
embed_key = H(b"embed-key-v1",
  sig_hash,
  body_hash,
  doc_hash,
  embed_model_id,      // e.g. b"model@rev"
  embed_prompt_version // e.g. b"prompt-v3"
)
```

**Moniker is intentionally excluded.** Rename with same sig/body/doc → **reuse embedding**.

#### `simhash64`

Charikar SimHash over body token multiset (64-bit). Used only for candidate blocking (T3/T6). Hamming radius default **3** for retrieval.

### 3.4 Per-IR-kind inclusion matrix

| Kind | sig | body | doc | ref | notes |
|------|-----|------|-----|-----|-------|
| Function / Method | yes | yes | yes | yes | primary cascade subject |
| RecordType / Sum / Union / Alias | yes (shape) | fields as structured tokens | yes | yes (extends/impl) | field renames may T2 within type |
| TraitDef | yes | method sig list | yes | yes | |
| TraitImpl | yes (trait+self ty) | method bodies aggregate | yes | yes | |
| Module | name+members list hash as "sig" | n/a empty body | yes | member refs | module move = T3 on children primarily |
| Constant / Variable / Field | yes | value expr tokens if any | yes | yes | |
| Macro | yes (name) | token tree | yes | weak | prefer T1/T2; T6 soft only |
| Info / Primitive | minimal | empty | yes | no | low lineage priority |

### 3.5 Normalizer versioning policy

| Event | Action |
|-------|--------|
| Bugfix that **changes** hash of same source | Bump `normalizer_version`; dual-write old+new for one release if needed; recompute fingerprints offline |
| New optional field ignored by hash | No bump |
| Producer version change without normalizer change | Does not enter symbol hash (lives in JobKey) |

Store `normalizer_version` on every `SymbolFingerprint` row and on every `LineageEdge` evidence blob.

---

## 4. Matching cascade T0–T7 (frozen)

### 4.1 Inputs per symbol (post-seal)

```rust
struct SymbolSnap {
    instance_id: SymbolInstanceId,      // heart SymbolId for this generation
    lineage_moniker: LineageMoniker,
    generation: PackageVersion,         // or GenerationId
    kind: IrKind,
    parent_moniker: Option<LineageMoniker>,
    name_u: String,                     // unqualified
    fqn_display: String,                // graph fq
    sig_hash: ContentHash,
    body_hash: ContentHash,
    doc_hash: ContentHash,
    ref_hash: Option<ContentHash>,
    content_hash: ContentHash,
    embed_key: ContentHash,
    simhash64: u64,
    // Matching features (materialized or lazy):
    body_tokens: Arc<[Token]>,          // or retrieve from CAS
    sig_struct: CanonicalSig,
    ref_out: BTreeSet<LineageMoniker>,
    ref_in: BTreeSet<LineageMoniker>,
    file_path: Option<PathBuf>,         // package-relative
    token_count: u32,
    embed: Option<VectorRef>,           // if already embedded
}
```

### 4.2 Pipeline order (mandatory)

```
T0 content short-circuit
T1 moniker / FQN exact
T2 same-parent rename cascade
T3 move (+ rename) LSH-blocked
T4 signature-evolution labels (annotates T1/T2 edges; not a bipartite pass)
T5 split / merge / extract / inline multi-map
T6 residual bipartite + embed assist (mostly soft)
T7 birth / death residuals
```

Each tier removes matched endpoints from the unmatched pools (except T4 labels and T5 multi-maps which may leave partial coverage).

### 4.3 Tier specifications

#### T0 — Content identity

| Item | Rule |
|------|------|
| Match | `kind` equal ∧ `body_hash` equal ∧ `sig_hash` equal |
| Algorithm | Hash join on `(kind, body_hash, sig_hash)` |
| Guard | If body_hash appears >1× in either generation (boilerplate), require `parent_moniker` equal **or** `name_u` equal for **auto**; else soft only |
| Confidence | `1.00` if unique; `0.99` if parent also matches; `0.85` soft if only body+sig among duplicates |
| Edge | `Identical` (if moniker equal) or `Moved`/`Renamed` if moniker differs (hash-equal rename/move) |
| Auto-link | yes at conf ≥ 0.99 |
| Precision target | ~100% under guard |

#### T1 — Moniker / FQN exact

| Item | Rule |
|------|------|
| Match | `lineage_moniker` equal ∧ `kind` equal |
| Algorithm | Hash join on moniker |
| Confidence | `0.99` if `sig_hash` equal; `0.97` if sig compatible (T4); `0.95` if only moniker (body changed) |
| Edge | `SameMoniker` / `SameMonikerSigChanged` / `SameMonikerBodyChanged` |
| Auto-link | always for unique moniker keys |
| Note | This is the industrial default (cargo-semver-checks / japicmp path equality). |

#### T2 — Rename within same container

| Item | Rule |
|------|------|
| Candidates | Unmatched pairs with same `parent_moniker`, same `kind` |
| Cascade | (1) unique `sig_hash` match → conf `0.98` edge `Renamed` |
|  | (2) body_sim ≥ **0.85** → conf `0.96` |
|  | (3) body_sim ≥ **0.70** ∧ (name_JW ≥ **0.80** ∨ ref_jaccard ≥ **0.50**) → conf `0.93` |
|  | (4) body_sim ≥ **0.70** alone → conf `0.90`; auto only if unique best ∧ margin ≥ **0.05** |
| Body metric | Primary **token Jaccard** on normalized tokens; secondary Jaro-Winkler on normalized text if `token_count < 30` |
| Assignment | Greedy by score desc; Hungarian only if dense collisions within parent |
| Auto-link | conf ≥ **0.93** ∧ decision=auto ∧ margin ok |
| Soft | conf 0.85–0.93 → `maybe_same` |
| Precision target | ~95–99% at auto |

#### T3 — Move (and move+rename)

| Item | Rule |
|------|------|
| Candidates | Cross-parent unmatched, same kind; block with simhash Hamming ≤ 3 **or** inverted token index top-k=**20** |
| Body threshold | body_sim ≥ **0.75** (HistoryFinder cross-file) |
| Boosts | +0.05 conf if `name_u` equal; +0.03 if ref_out Jaccard ≥ 0.40; +0.05 if gix file-rename prior links parents' files |
| Auto-link | composite ≥ **0.88** ∧ margin ≥ **0.05** |
| Confidence | `min(0.97, 0.80 + 0.20*body_sim + boosts)` capped |
| Edge | `Moved` / `MovedRenamed` |
| Soft | composite 0.75–0.88 → `maybe_same` |

#### T4 — Signature evolution (labeler)

Not a separate matching pass. For any auto edge from T0–T3 where moniker or body continuity holds:

| `sig_hash` | Label payload |
|------------|---------------|
| equal | none |
| differ | `sig_delta`: add/remove/reorder param, change return/param types, async/throws modifiers (best-effort structured diff of `CanonicalSig`) |

Edge kind may be multi-tagged: e.g. `Moved` + `SignatureEvolved`. Confidence inherits the structural edge.

#### T5 — Split / merge / extract / inline

| Direction | Rule |
|-----------|------|
| Split 1→N | Deleted `d`, added set `A*`; token multiset cover(d, A*) ≥ **0.80**; each `a` pairwise overlap with `d` ≥ **0.25**; prefer \|A*\| ∈ 2..=4; size sum ≈ size(d) within 20% |
| Merge N→1 | Reverse coverage |
| Extract/Inline | Prefer when statement-level multi-map available; v1 may approximate with token partitions |
| Confidence | `0.85 + 0.10 * cover` capped 0.95 |
| Edge kinds | `Split`, `Merged`, `ExtractedFrom`, `InlinedInto` |
| Auto-link | only if cover ≥ 0.85 ∧ no competing covers within 0.05 |
| Policy | **Accept incomplete recall** (RMiner Extract And Move R can be ~0.70). Prefer false-split. |

**Do not** force 1:1 bipartite assignment for T5.

#### T6 — Residual bipartite (mostly soft)

Composite similarity (weights frozen v1):

```
sim = 0.35*body + 0.20*name + 0.15*sig + 0.15*ref + 0.10*path + 0.05*doc
```

| Metric | Definition |
|--------|------------|
| body | token Jaccard |
| name | Jaro-Winkler on `name_u` |
| sig | structural param/return similarity 0..1 |
| ref | Jaccard on ref_out monikers |
| path | shared parent prefix length / max depth |
| doc | TF-IDF cosine if both non-empty else 0 |

| Threshold | Action |
|-----------|--------|
| sim ≥ **0.92** ∧ kind match ∧ margin ≥ **0.08** ∧ embed_cos ≥ **0.80** (if embed available) | auto `Related` conf=sim |
| sim ≥ **0.75** | soft `MaybeSame` — **never** embed-skip |
| else | no edge |

Embeddings: retrieve top-k cosine ≥ 0.85 as candidate generators only. **Never** auto-merge on embed alone.

#### T7 — Birth / death

| Residual in \(G_n\) | `Added` (birth) |
| Residual in \(G_{n-1}\) | `Removed` (death) |

No lineage edge required; optional Terminus markers for analytics.

### 4.4 Global assignment constraints

1. **Kind hard filter** always.  
2. **Margin:** best − second ≥ δ (tier-specific).  
3. **One-sided greedy** preferred over forced perfect matching when scores sparse.  
4. **Mass-delete guard:** if > **40%** of previous symbols unmatched before T5, skip T5 auto and emit soft-only (partial publish / reformat storms).  
5. **Generated code:** if IR flag `generated=true`, disable cross-parent T0 auto.  

### 4.5 Confidence → consumer policy

| Consumer | Accept auto identity | Soft edges |
|----------|---------------------|------------|
| **Skip re-embed** | conf ≥ **0.95** ∧ `body_hash` equal ∧ `doc_hash` equal (embed_key equal) | never |
| **Skip tantivy body fields** | conf ≥ 0.95 ∧ content parts equal for indexed fields | never |
| **Propagate LineageId / hard history** | conf ≥ **0.93** ∧ decision=auto | never |
| **UI "renamed/moved from"** | conf ≥ **0.90** ∧ tier ∈ {T2,T3,T0-rename} | show as "possible" |
| **Author patch** | conf = 1.0 | n/a |

### 4.6 Matcher versioning

Every edge stores:

```
matcher_id = "nudox-identity"
matcher_version = "1.0.0"
moniker_grammar_version = 1
normalizer_version = 1
```

Bumping matcher thresholds → new matcher_version; old edges remain valid historical claims.

---

## 5. Lineage edge model

### 5.1 Edge type enum (normative)

```rust
pub enum LineageKind {
    Identical,              // T0 moniker+hash
    SameMoniker,            // T1
    SameMonikerSigChanged,  // T1+T4
    SameMonikerBodyChanged, // T1 body only
    Renamed,                // T2
    Moved,                  // T3
    MovedRenamed,           // T3
    SignatureEvolved,       // T4 tag (may combine)
    BodyEdited,             // annotation when moniker stable body changes
    Split,                  // T5 1→N umbrella
    Merged,                 // T5 N→1 umbrella
    ExtractedFrom,          // T5
    InlinedInto,            // T5
    Related,                // T6 auto
    MaybeSame,              // T6 soft
    Reexport,               // alias edge, not origin
    Copy,                   // cross-package or vendored; default off for auto
    Manual,                 // author patch
    Added,                  // T7 marker (optional)
    Removed,                // T7 marker (optional)
}
```

### 5.2 Edge record

```rust
pub struct LineageEdge {
    pub edge_id: ContentHash,           // H(canonical edge bytes) for CAS/dedup
    pub from: SymbolEndpoint,           // generation-qualified
    pub to: SymbolEndpoint,
    pub kind: LineageKind,
    pub tier: u8,                       // 0..=7
    pub confidence: f32,                // 0..=1
    pub decision: Decision,             // Auto | Soft | Manual | Provisional
    pub features: FeatureVector,
    pub evidence: Vec<Evidence>,
    pub matcher_version: semver::Version,
    pub moniker_grammar_version: u32,
    pub normalizer_version: u32,
    pub created_at: Timestamp,
}

pub struct SymbolEndpoint {
    pub package_stem: PackageStemId,
    pub package_version: PackageVersion,
    pub package_id: PackageId,          // versioned heart id
    pub instance_id: SymbolInstanceId,  // heart SymbolId
    pub lineage_moniker: LineageMoniker,
    pub generation_symbol: String,      // SCIP-shaped
    pub graph_iri: String,              // Symbol/…
}

pub enum Decision { Auto, Soft, Manual, Provisional }

pub struct FeatureVector {
    pub body_sim: Option<f32>,
    pub name_sim: Option<f32>,
    pub sig_sim: Option<f32>,
    pub ref_jaccard: Option<f32>,
    pub path_sim: Option<f32>,
    pub doc_sim: Option<f32>,
    pub embed_cos: Option<f32>,
    pub git_rename_score: Option<f32>,
}

pub enum Evidence {
    HashEquality { sig: bool, body: bool, doc: bool },
    MonikerEqual,
    GitFileRename { from_path: String, to_path: String, score: f32 },
    AuthorPatch { patch_id: String },
    CompositeScore { sim: f32 },
    Coverage { cover: f32, partners: u8 },
}
```

**Cardinality:**

- T0–T4, T6: primarily 1:1  
- T5: 1:N or N:1 encoded as **multiple edges** sharing a `group_id = H(kind ‖ primary ‖ gen_from ‖ gen_to)`  
- Never N:M in v1 auto

### 5.3 Transitive confidence

```
path_conf(edges) = min(e.confidence for e in path)   // default, conservative
// alternative product: Π e.confidence — optional for UI "possible history"
```

Only `decision=Auto|Manual` edges participate in hard transitive `LineageId` walks.

---

## 6. Storage shapes

### 6.1 SQLite INDEX (local desktop + cold path)

Extends `08-incremental` constructive-trace store.

```sql
-- Package stem (versionless)
CREATE TABLE package_stems (
  stem_id        BLOB PRIMARY KEY,     -- 16-byte UUID or 32-byte hash
  origin         TEXT NOT NULL,
  ecosystem      TEXT NOT NULL,
  package_name   TEXT NOT NULL,
  UNIQUE (origin, ecosystem, package_name)
);

-- Generation
CREATE TABLE generations (
  id             INTEGER PRIMARY KEY,
  stem_id        BLOB NOT NULL REFERENCES package_stems(stem_id),
  package_id     BLOB NOT NULL,        -- heart PackageId bytes
  version        TEXT NOT NULL,
  git_commit     BLOB,
  parent_gen     INTEGER,
  status         TEXT NOT NULL,        -- running|sealed|failed
  moniker_grammar_version INTEGER NOT NULL DEFAULT 1,
  normalizer_version INTEGER NOT NULL DEFAULT 1,
  sealed_at      INTEGER,
  UNIQUE (stem_id, version)
);

-- Per-symbol head at a generation (L3 fingerprints)
CREATE TABLE symbol_heads (
  generation     INTEGER NOT NULL,
  instance_id    BLOB NOT NULL,        -- SymbolInstanceId / heart SymbolId
  lineage_moniker TEXT NOT NULL,
  graph_iri      TEXT NOT NULL,
  kind           TEXT NOT NULL,
  parent_moniker TEXT,
  name_u         TEXT NOT NULL,
  sig_hash       BLOB NOT NULL,
  body_hash      BLOB NOT NULL,
  doc_hash       BLOB NOT NULL,
  ref_hash       BLOB,
  content_hash   BLOB NOT NULL,
  embed_key      BLOB NOT NULL,
  simhash64      INTEGER NOT NULL,
  ir_blob        BLOB NOT NULL,        -- CAS ContentHash
  file_path      TEXT,
  PRIMARY KEY (generation, instance_id)
);
CREATE INDEX sh_moniker ON symbol_heads(generation, lineage_moniker);
CREATE INDEX sh_body ON symbol_heads(generation, body_hash);
CREATE INDEX sh_embed ON symbol_heads(embed_key);
CREATE INDEX sh_simhash ON symbol_heads(generation, simhash64);

-- Lineage edges (adjacent generations)
CREATE TABLE lineage_edges (
  edge_id        BLOB PRIMARY KEY,
  stem_id        BLOB NOT NULL,
  from_gen       INTEGER NOT NULL,
  to_gen         INTEGER NOT NULL,
  from_instance  BLOB NOT NULL,
  to_instance    BLOB NOT NULL,
  from_moniker   TEXT NOT NULL,
  to_moniker     TEXT NOT NULL,
  kind           TEXT NOT NULL,
  tier           INTEGER NOT NULL,
  confidence     REAL NOT NULL,
  decision       TEXT NOT NULL,        -- auto|soft|manual|provisional
  group_id       BLOB,                 -- T5 multi-edge group
  features_json  TEXT NOT NULL,
  evidence_json  TEXT NOT NULL,
  matcher_version TEXT NOT NULL,
  moniker_grammar_version INTEGER NOT NULL,
  normalizer_version INTEGER NOT NULL,
  created_at     INTEGER NOT NULL
);
CREATE INDEX le_from ON lineage_edges(from_instance);
CREATE INDEX le_to ON lineage_edges(to_instance);
CREATE INDEX le_gens ON lineage_edges(stem_id, from_gen, to_gen);
CREATE INDEX le_decision ON lineage_edges(decision, confidence);

-- Optional stable lineage chain head for UI
CREATE TABLE lineage_ids (
  lineage_id     BLOB PRIMARY KEY,     -- UUIDv5(STEM_NS, stem || moniker_at_birth)
  stem_id        BLOB NOT NULL,
  head_instance  BLOB NOT NULL,        -- latest auto-linked instance
  birth_moniker  TEXT NOT NULL,
  updated_gen    INTEGER NOT NULL
);

-- Constructive traces (from 08) unchanged in spirit
CREATE TABLE stage_traces (
  stage_id       TEXT NOT NULL,
  input_digest   BLOB NOT NULL,
  output_digest  BLOB NOT NULL,
  output_blob    BLOB NOT NULL,
  tool_digest    BLOB NOT NULL,
  created_at     INTEGER NOT NULL,
  PRIMARY KEY (stage_id, input_digest, tool_digest)
);
```

### 6.2 REGISTRY (server) — CAS + metadata

| Object | CAS key | Payload |
|--------|---------|---------|
| IR Index | existing `ir_ref` | + sealed moniker/hash fields on entries or sidecar |
| `SymbolFingerprintSet` | `JobKey.with_tag(b"sym-fp-v1")` | postcard/BTreeMap moniker → fingerprints |
| `LineageEdgeBatch` | `H(stem ‖ from_ver ‖ to_ver ‖ matcher_ver)` | all edges for adjacent pair |
| Author patches | `H(stem ‖ patch_id)` | explicit moniker maps |

**BlobManifest extension (additive):**

```rust
pub struct BlobManifest {
    // existing fields…
    pub fingerprints_ref: Option<ContentHash>, // SymbolFingerprintSet
    pub occurrences_ref: Option<ContentHash>,  // from IR audit (related)
}
```

Postgres metadata (tiering-adjacent):

```sql
-- next to PackageMetadata
lineage_cursor_version TEXT,           -- last matched generation
lineage_batch_hash     BYTEA           -- CAS pointer to LineageEdgeBatch
```

### 6.3 Terminus (hot tier only)

**Do not invent parallel SymbolNode types.** Extend `workspace/compiler/graph/model.rs` with `TerminusDBModel`:

```rust
/// First-class rename/replace edge — product-visible lineage beyond declares set-diff.
#[derive(TerminusDBModel)]
pub struct LineageEdgeDoc {
    pub id: String,              // LineageEdge/{edge_id_hex}
    pub from_symbol: TdbLazy<Symbol>,  // version-agnostic IRI of from moniker's graph node
    pub to_symbol: TdbLazy<Symbol>,
    pub from_version: String,
    pub to_version: String,
    pub kind: String,
    pub tier: i32,
    pub confidence: f64,
    decision: String,
    matcher_version: String,
    // optional: features as sys:JSON
}
```

**Hot lineage queries (07 alignment):**

1. Membership: `PackageVersion.declares` set-diff (always).  
2. Content evolution of stable moniker: Terminus document history on `Symbol/…` when moniker stable.  
3. Rename/move: **query `LineageEdgeDoc`** (this RFC).  
4. Cold: SQLite/CAS edge batch + IR set-diff; no commit log.

**Publish:** only Auto+Manual edges with conf ≥ 0.93 by default; Soft edges stay INDEX-only unless product flag enables provisional graph write.

### 6.4 REGISTRY dual-store rule

| Data | INDEX (SQLite) | REGISTRY CAS | Terminus |
|------|----------------|--------------|----------|
| Fingerprints | yes | yes | no (derive) |
| Soft edges | yes | optional | no |
| Auto edges | yes | yes | yes if Hot |
| Symbol docs | projected | IR | yes if Hot |

---

## 7. heart PackageId / SymbolId interaction (frozen)

### 7.1 Keep versioned PackageId

`PackageId = UUIDv5(PACKAGE, origin ‖ name ‖ version)` stays **generation instance id**. Do not break existing CAS/manifest keys.

### 7.2 New PackageStemId

```rust
// identity_bytes = len_pref(origin) || 0 || len_pref(name.canonical) || 0
// PackageStemId = UUIDv5(PACKAGE_STEM_NS, identity_bytes)
// PACKAGE_STEM_NS = new fixed u128 …0003 (document in heart/identity/namespace.rs)
```

### 7.3 SymbolInstanceId = today's SymbolId

```
EntryUri { package: PackageId /*versioned*/, path: segments }
SymbolInstanceId = UUIDv5(SYMBOL, instance_token || 0 || EntryUri.canonical())
```

**Still version-coupled and instance-salted.** Equality of SymbolInstanceId across versions is **undefined / always false** for different PackageIds. Cross-version join uses moniker or lineage edges.

### 7.4 LineageId (optional derived)

```
LineageId = UUIDv5(LINEAGE_NS, stem_id_bytes || 0 || birth_lineage_moniker.as_bytes())
```

- Minted at **birth** (T7 Added).  
- On Auto rename/move, **update** `lineage_ids.head_instance` but **keep** same LineageId.  
- On false-split recovery (later auto edge), merge policy is **manual only** in v1 (prefer split).  
- Split 1→N: children get **new** LineageIds; parent death; edges `ExtractedFrom` preserve history without ID merge.  
- Merge N→1: survivor keeps one LineageId (highest conf parent or largest body); others end with `InlinedInto`.

### 7.5 Stamp graph `symbol_id`

Compiler projection today leaves `Symbol.symbol_id = None`. **Uploader / registry materializer** stamps:

```
symbol_id = EntryUri { package_id, path_segments_from_moniker }.symbol_id(instance_token)
```

One construction site; never dual-null in production sinks.

### 7.6 resolution.rs supersession

`registry/runtime/graph/resolution.rs` path-only `diff` becomes a **thin adapter** over this RFC:

| Old Resolution | New mapping |
|----------------|-------------|
| Stable | T1 SameMoniker |
| Moved (name+kind Jaccard) | weak subset of T2/T3 — **replace** with full cascade |
| Removed/Added | T7 after cascade |

Keep function name `diff` but implement via `match_generations` returning `Vec<LineageEdge>` + projection to legacy enum for API compat during migration.

---

## 8. Producer emission requirements (normative checklist)

Every language producer **must** stamp on each emitted `Entry` (or parallel fingerprint sidecar sealed with Index):

| Field | Required |
|-------|----------|
| Deterministic `NudoxPath` via shared `path_segments` | yes |
| `kind` accurate | yes |
| `name` leaf | yes |
| `aliases` for re-exports (export paths) | yes when known |
| `visibility` | yes |
| `documentation` | yes if present |
| Signature structure for functions/types | yes |
| Body token stream **or** enough CST to derive body tokens at generate stage | yes for body-bearing kinds |
| Definition file path (package-relative) | yes when known |
| Receiver / overload disambiguator inputs | yes for methods |
| `generated: bool` when detectable | recommended |
| Intra-package references | via OccurrenceSet pipeline (generate), not necessarily raw producer |

**Generate stage responsibilities** (may be shared, not per-language):

1. Moniker emission (`moniker_grammar_version=1`)  
2. Hash computation (`normalizer_version=1`)  
3. `SymbolFingerprintSet` CAS object  
4. Collapse dual-emitted methods  

**Golden tests per language:** ≥20 moniker strings + ≥10 hash stability fixtures (whitespace-only change → same body_hash).

**Producers must not:**

- Publish salsa/interned u32 ids  
- Embed package version inside lineage moniker  
- Use text spans as identity  
- Emit non-deterministic HashMap order in sealed fingerprint sets (sort by moniker)

---

## 9. Rename / move / split / merge policies

### 9.1 Risk preference (frozen)

| Error | Cost | Policy |
|-------|------|--------|
| False merge | High — poisons embed reuse, history, Terminus | Raise τ; margins; soft edges; never embed-skip on soft |
| False split | Medium — orphans history | Accept; allow later Manual patch; UI "maybe related" |

**Operational translation:** when in doubt, emit `MaybeSame` or nothing — not `Auto`.

### 9.2 Rename

1. Prefer T0 hash-equal rename (conf 1.0 / 0.99).  
2. Else T2 cascade.  
3. API tools (cargo-semver-checks) will report remove+add — **lineage still records continuity**.  
4. Authors should `#[deprecated]` + re-export for SemVer; re-export is `ExportAlias`, not a second definition lineage.

### 9.3 Move

1. gix file rename prior when VCS-backed (`tree_with_rewrites`, default similarity 50% **file-level only** as prior, not symbol threshold).  
2. T3 body ≥ 0.75 + margin.  
3. Module move of a type moves children monikers; match type first (UMLDiff hierarchy), then members within mapped parents before global T3.

### 9.4 Signature evolution

- Same moniker + new sig_hash → T1 edge + T4 label; **re-embed** (sig in embed_key).  
- Do not treat as new LineageId.

### 9.5 Body edit

- Same moniker + new body_hash → T1 + BodyEdited; re-embed if body/doc in embed_key.

### 9.6 Docs-only

- doc_hash changes; sig/body same → re-embed (docs in embed_key); graph edges may skip if ref_hash unchanged.

### 9.7 Split / merge

- Multi-edge with `group_id`.  
- Incomplete recall OK.  
- No auto LineageId merge across split children.

### 9.8 Author patches (Unison-shaped)

```json
{
  "package_stem": "…",
  "from_version": "1.0.0",
  "to_version": "2.0.0",
  "replacements": [
    { "from_moniker": "…fetch().", "to_moniker": "…get().", "kind": "Renamed" }
  ]
}
```

Confidence **1.0**, decision **Manual**, overrides conflicting soft edges, conflicts with Auto of different target → **Manual wins**, Auto edge dropped with audit log.

### 9.9 Cross-package Copy

Default **off** for auto. Content-hash equality across packages may suggest `Copy` soft edges for provenance products later — not Terminus hard lineage in v1.

---

## 10. Incremental pipeline integration (08)

### 10.1 SymbolDelta extension

```rust
pub struct SymbolDelta {
    pub generation: u64,
    pub parent: Option<u64>,
    pub added: Vec<SymbolInstanceId>,
    pub removed: Vec<SymbolInstanceId>,
    pub changed: Vec<SymbolChange>,
    pub lineage_edges: Vec<LineageEdge>,  // NEW: cascade output
}

pub struct SymbolChange {
    pub id: SymbolInstanceId,
    pub old_hash: ContentHash,
    pub new_hash: ContentHash,
    pub old_parts: SymbolPartHashes,
    pub new_parts: SymbolPartHashes,
    /// If present, this change is a rename/move continuation of old_id
    pub continues: Option<SymbolInstanceId>,
}

pub struct SymbolPartHashes {
    pub sig: ContentHash,
    pub body: ContentHash,
    pub doc: ContentHash,
    pub refs: Option<ContentHash>,
    pub embed_key: ContentHash,
}
```

### 10.2 Fan-out rules

| Stage | Use |
|-------|-----|
| embed | Run only if `embed_key` new; **reuse** if edge says rename and embed_key equal |
| tantivy | delete+add on instance id **or** update moniker field if continues |
| terminus | Δ symbols + LineageEdgeDocs; stable moniker docs patch in place |
| render | rewrite pages for changed + renamed targets |

### 10.3 Early cutoff layers

```
L0 git tree → L1 file blob → L2 package IR → L3 symbol parts → L4 stage(tool‖parts)
```

---

## 11. Test plan

### 11.1 Golden multi-version packages

| Fixture pack | Languages | Scenarios |
|--------------|-----------|-----------|
| `acme-http` synthetic | Rust | rename, move+sig, extract split, docs-only, pure format |
| `npm-router` synthetic | TS | overload disambiguator, re-export alias |
| `py-util` synthetic | Python | move module, rename class method |
| Real crates (sampled) | Rust | 10 popular crates × 5 consecutive semver pairs |
| Real npm (sampled) | TS | 10 packages × 3 versions |
| Optional Java | Java | RMiner pseudo-oracle on open jars/commits |

### 11.2 Unit tests

1. Moniker parse/print round-trip (grammar v1).  
2. Version strip/qualify bijection.  
3. SCIP export smoke (`scip` 0.9.x).  
4. Hash stability: whitespace-only body → same body_hash.  
5. Docs NFC/LF normalization.  
6. path_segments shared across modules.  
7. T0–T3 cascade on synthetic snaps (table-driven).  
8. Margin rejection when two candidates tie.  
9. Mass-delete guard disables T5 auto.  
10. Author patch overrides soft.  

### 11.3 Metrics (primary KPIs)

| Metric | Target (auto edges) |
|--------|---------------------|
| False-merge rate (precision) | **≥ 99%** precision ⇒ FP merge **< 1%** |
| Rename recall (synthetic) | ≥ 95% |
| Move recall (synthetic) | ≥ 90% |
| Split recall | best-effort; no floor in v1 |
| Embed calls saved on pure rename | **100%** (embed_key hit) |
| Soft edge contamination of hard sinks | **0** |

### 11.4 Oracle construction

1. Multi-signal union (moniker, body, refs).  
2. Dual human review on 100 random auto + 100 soft edges / language / quarter.  
3. Java: optional RMiner mapping compare (calibration, not production dep).  
4. Asymmetric cost: score `Loss = 10 * FP_merge + 1 * FN_split`.  

### 11.5 Performance tests

- Package with 10⁵ symbols: T3/T6 must use LSH; full Cartesian forbidden.  
- Budget: match_generations < 5s for 10k symbols on desktop reference laptop (non-normative SLO, track).  

---

## 12. Phased rollout (multi-PR, no master plan)

| Phase | PR scope | Exit criteria |
|-------|----------|---------------|
| **P0 Spec freeze** | This RFC landed; `moniker_grammar_version=1`, `normalizer_version=1` constants in heart | Review sign-off |
| **P1 Types** | `LineageMoniker`, `PackageStemId`, `SymbolFingerprint`, `LineageEdge` in `heart` | Compiles; serde stable |
| **P2 path_segments** | Single splitter used by ir/graph/producers | Cross-crate tests green |
| **P3 Producer stamp** | Generate emits moniker+hashes sidecar; golden fixtures Rust+TS | Goldens pass |
| **P4 Fingerprint CAS** | `SymbolFingerprintSet` in blob pipeline | Second build identical bytes |
| **P5 Matcher T0–T1–T2** | `match_generations` MVP; SQLite edges | Synthetic rename suite |
| **P6 T3+gix prior** | Move matching + optional gix | Move fixtures |
| **P7 Embed key wiring** | embed stage keys on embed_key; rename reuse | Metric embed_saved |
| **P8 T5 soft/auto** | Split/merge multi-map | Coverage tests |
| **P9 T6 soft** | Residual maybe_same INDEX-only | No hard sink use |
| **P10 Terminus LineageEdgeDoc** | Hot publish Auto edges | WOQL/GraphQL fetch history |
| **P11 Author patches** | Upload API + Manual conf 1.0 | Patch e2e |
| **P12 Replace resolution.rs** | Legacy adapter | No behavior gap alerts |
| **P13 Calibration** | Threshold tuning from live audit | FP_merge < 1% sample |

**Dependency note:** P3–P4 unlock incremental value even before fancy T5/T6. Ship T0–T2 early.

---

## 13. Algorithms (reference implementations)

### 13.1 `match_generations`

```rust
fn match_generations(old: &[SymbolSnap], new: &[SymbolSnap], opts: MatchOpts)
    -> Vec<LineageEdge>
{
    let mut edges = Vec::new();
    let mut old_u: HashMap<InstanceId, SymbolSnap> = index(old);
    let mut new_u = index(new);

    edges.extend(tier0_hash_join(&mut old_u, &mut new_u));
    edges.extend(tier1_moniker_join(&mut old_u, &mut new_u));

    for parent in all_parents(&old_u, &new_u) {
        edges.extend(tier2_renames(parent, &mut old_u, &mut new_u));
    }

    edges.extend(tier3_moves_lsh(&mut old_u, &mut new_u, opts.git_priors));

    if !mass_delete(&old_u, old.len()) {
        edges.extend(tier5_split_merge(&mut old_u, &mut new_u));
    }

    edges.extend(tier6_residual(&mut old_u, &mut new_u, opts));
    annotate_t4_sig_evolution(&mut edges, old, new);
    // T7 implicit residuals
    edges
}
```

### 13.2 Token Jaccard

```
jaccard(A, B) = |A∩B| / |A∪B|  on multisets folded to sets of token strings
// optional multiset generalization: sum min(c_a,c_b) / sum max(c_a,c_b)
```

v1 uses **set** Jaccard on token strings; multiset variant behind flag for calibration.

### 13.3 ISC (Kim) for refs

```
S(A,B) = 0.5 * (|A∩B|/|A| + |A∩B|/|B|)  // empty sets → 0 if both empty treat as 1.0
```

Used for ref_jaccard boosts interchangeably with Jaccard; freeze **Jaccard** for simplicity in v1.

---

## 14. Worked multi-generation example

**Package:** `acme-http` (cargo)

### G1 v1.0.0

| Sym | Moniker | sig | body | embed |
|-----|---------|-----|------|-------|
| A | `… client/Client#fetch().` | H1 | B1 | E1 |
| C | `… util/parse_headers.` | Hc | Bc | Ec |
| D | `… util/normalize_url.` | Hd | Bd | Ed |

### G2 v1.1.0 — rename fetch→get; extract parse_headers

| Sym | Moniker | Continuity |
|-----|---------|------------|
| A' | `… client/Client#get().` | T0/T2 from A; embed E1 reused |
| C1 | `… util/parse_header_name.` | T5 ExtractedFrom C |
| C2 | `… util/parse_header_value.` | T5 ExtractedFrom C |
| D' | `… util/normalize_url.` | T1 SameMoniker |

Edges: `Renamed(A→A')`, `ExtractedFrom(C→C1)`, `ExtractedFrom(C→C2)`, `SameMoniker(D→D')`.

### G3 v2.0.0 — move Client to http/client + add timeout param

| Sym | Moniker | Continuity |
|-----|---------|------------|
| A'' | `… http/client/Client#get().` | T3 MovedRenamed path + T4 sig; new embed E2 |

Query history(A''): A'' ← A' ← A.

---

## 15. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| sig_hash instability across producer versions | normalizer_version domain; golden tests |
| Overloaded methods | disambiguators mandatory |
| Generated boilerplate T0 false merge | parent/name guards; generated flag |
| Partial publishes mass delete | 40% guard; soft-only T5 |
| Ref resolution sparse per language | down-weight ref in composite per-language config |
| RMiner P/R not transferable | IR-level reimplementation; RMiner only calibration |
| Embedding model drift | embed_model_id in embed_key; T6 version pin |
| Graph IRI vs moniker drift | single path_segments + moniker→fq renderer tests |
| Terminus schema evolution | LineageEdgeDoc additive; re-promote hot DBs |
| gix vs git rename parity | pin thresholds; fixtures from real repos |
| Privacy of content hashes | threat model later; hashes not raw source |

---

## 16. Open questions (explicitly unresolved)

1. **TraitImpl moniker exact string** — meta form vs `impl/` namespace: fixtures decide in P3; grammar allows both experimentally under grammar v1.1 if needed.  
2. **Whether `ref_in` is required at seal** — may be computed only in matcher from OccurrenceSet.  
3. **LineageId merge after false split** — manual only in v1; auto-union later?  
4. **Publish soft edges to Terminus** — default no; product flag?  
5. **Multiset vs set Jaccard** — calibrate on fixtures.  
6. **Anonymous closures / lambdas** — exclude from lineage in v1.  
7. **Package registry rename** — package-level patch schema TBD.  
8. **Float const canonicalization** — 15 digits vs exclude from sig.  
9. **Dual write fingerprints during normalizer bump** — duration one release or N days?  
10. **MAX edges per generation pair** — DoS guard number TBD (suggest 10× symbol count).  

---

## 17. Normative constants cheat sheet

```
moniker_grammar_version     = 1
normalizer_version          = 1
matcher_version             = 1.0.0
body_same_parent_auto       = 0.70
body_cross_parent_auto      = 0.75
body_high_precision         = 0.85
name_jw_support             = 0.80
ref_jaccard_boost           = 0.40 / 0.50
composite_auto_t6           = 0.92
composite_soft_t6           = 0.75
margin_t2                   = 0.05
margin_t3                   = 0.05
margin_t6                   = 0.08
embed_assist_min            = 0.80
split_cover                 = 0.80
auto_lineage_min_conf       = 0.93
embed_skip_min_conf         = 0.95
mass_delete_frac            = 0.40
simhash_hamming_radius      = 3
t3_topk                     = 20
weights                     = body 0.35, name 0.20, sig 0.15, ref 0.15, path 0.10, doc 0.05
hash                        = BLAKE3-256 via heart::ContentHash
scip_crate                  = 0.9.x interop
```

---

## 18. Mapping to live code (integration anchors)

| Concern | Path |
|---------|------|
| ContentHash / JobKey | `workspace/heart/content.rs` |
| PackageId | `workspace/heart/package/coordinates.rs` |
| SymbolId / EntryUri | `workspace/heart/identity/symbol.rs` |
| Namespaces | `workspace/heart/identity/namespace.rs` |
| IR Index / Entry | `workspace/ir/entry.rs`, `kind.rs` |
| Graph IRI | `workspace/compiler/graph/link.rs` |
| Graph model | `workspace/compiler/graph/model.rs` |
| Projection | `workspace/compiler/graph/from_ir.rs` |
| Legacy cross-ver diff | `workspace/registry/runtime/graph/resolution.rs` |
| BlobManifest | `workspace/registry/blob/mod.rs` |
| Generate pipeline | `workspace/compiler/generate/mod.rs` |
| Occurrences | `workspace/ir/syntax/occurrence.rs`, `generate/resolve.rs` |

---

## 19. Research grounding (load-bearing claims)

| Claim | Source |
|-------|--------|
| Origin analysis + multi-matchers | Godfrey & Zou TSE 2005 |
| Body strongest factor; thresholds ~0.5 composite | Kim/Pan/Whitehead WCRE 2005 |
| History cascade sig → body 0.70 → cross-file 0.75 | HistoryFinder 2025 |
| Rename/move P≈0.99 class tools | RefactoringMiner accuracy 2026-06-29 |
| Prefer false-split | Entity resolution + 05 framing |
| SCIP version-pinned monikers; strip for continuity | scip.proto / industrial 06 |
| Unison patches = explicit lineage | Unison docs |
| Embed by content hash not path | Cursor indexing |
| Constructive-trace early cutoff | Build Systems à la Carte; 08 |
| Stable Symbol IRI + PackageVersion.declares | live graph model; 07 |
| SymbolId version-coupled | IR audit 04 / heart |

External URLs retained in plans 05/06; this RFC freezes **behavior**, not bibliography.

---

## 20. Executive freeze summary

nudox answers “same symbol across generations?” with a **three-layer architecture**:

1. **SCIP-inspired `LineageMoniker`** (version-stripped) as the readable continuity coordinate; generation indexes keep version-qualified SCIP-shaped strings.  
2. **BLAKE3 part hashes** (`sig`/`body`/`doc`/`ref`/`embed_key`) with `normalizer_version`, enabling rename-safe embed reuse and symbol-level early cutoff.  
3. **Explicit `LineageEdge` records** from a **T0–T7 cascade** with frozen thresholds, preferring false-split; soft edges never skip embeds or hard Terminus identity.

heart `PackageId`/`SymbolId` remain **instance** ids; new `PackageStemId` + moniker (+ optional `LineageId`) carry cross-generation meaning. Storage is SQLite INDEX + CAS REGISTRY for all edges/fingerprints; Terminus gets Auto edges only on hot packages. Producers must stamp monikers and materials for hashes; generate seals fingerprints. Roll out in phases P0–P13, shipping T0–T2 before ambitious split/merge.

---

*End of RFC-19 PLAN. Status: design freeze. No implementation in this document.*
