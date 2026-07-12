# REFERENCES-PLAN — Tree-sitter-driven usage references, answered in IR symbols

**Status:** PROPOSED (2026-07-12)
**Scope:** `workspace/compiler` (ir/syntax, generate, graph, treesitter), `workspace/registry/blob`, `workspace/server`, `workspace/runtime`.

---

## 0. The dream, stated precisely

Given any function (or type, macro, …) identified by a fully-qualified IR path, answer:

> **Where is `calculator::yo` used?**
> → In `calculator::hello` (fn, calls it at `src/lib.rs` bytes 24..28), in `calculator::report::summarize`, and in `other-pkg::main` (external consumer).

Both the question and every answer are `NudoxPath`s — the same identity the IR, graph,
and search layers already speak. Each answer can be rendered as an **example**: the
enclosing function's source snippet with the call site highlighted.

This is SCIP/LSIF-shaped "find references," built from tree-sitter parses, resolved
against the IR surface index, persisted next to the IR, and served from the graph.

---

## 1. Where we are today (survey findings, 2026-07-12)

The machine is roughly **one-third built**. Extraction and storage plumbing exist;
resolution, attribution, and the query surface do not. Precise inventory:

### What exists and works

| Piece | Where | State |
| --- | --- | --- |
| Per-file tree-sitter parse over the whole package | `generate/cst.rs:63` (`cst::extract`) | Runs in the pipeline as its own CAS-cached stage (`generate/mod.rs:107`, tag `b"cst"`), all 6 languages + TSX/JS via `grammar_for_extension` (`cst.rs:37`) |
| Reference walking | `ir/syntax/walker.rs:7` (`walk_references`) | Pre-order DFS, per-node classifier callback, byte spans retained |
| Per-language classifiers | `treesitter.rs:134-326` | 6 hand-written `(kind, parent_kind)` matchers; emit `FunctionCall`, `TypeReference`, `MacroInvocation`, `FieldAccess`, `VariableUse`, `Import` |
| Reference vocabulary | `ir/syntax/types.rs:3-19` | `ResolvedReference { target: NudoxPath, span: Range<usize>, kind: ReferenceKind }` |
| Blob persistence | `registry/blob/mod.rs:60,215` | `CstSet` → `ReferenceSet` (postcard) stored in CAS under `BlobManifest.references_ref` |
| Graph vocabulary for the call graph | `graph/model.rs:1027` | `Reference { source, target, kind, span_start, span_end }` — reified, value_hash-keyed, wave-4 emitted |
| Name→symbol resolution ladder | `graph/link.rs:110-235` | `Linker`: exact fq → alias → unique-suffix → stub mint; used today only for declaration-level links |
| Runtime query verbs | `runtime/graph/mod.rs:83-93` | `get_occurrences`, `get_references`, `are_related` + `RelationKind::{Reference, Occurrence}` |
| Snippet extractor | `treesitter.rs:446` (`parse_and_extract`) | Enclosing-fn extraction + centered-window fallback + sexp; **test-only, no production caller** |
| Symbol search | `runtime/text/index.rs` | tantivy over `name`/`fq_name`/subtokens → `SymbolId`; the natural "question" entry point |

### The gaps (why the dream doesn't work yet)

1. **No resolution.** Every `ResolvedReference.target` is a placeholder —
   `NudoxPath::Local(PathBuf::from(raw_identifier))` (`treesitter.rs:123-127`, comment:
   "Full path resolution happens later against the surface index"). *No code performs
   that resolution anywhere.* The `ReferenceSet` in CAS is a pile of unqualified names.
2. **No attribution.** Nothing maps a reference's span to the *enclosing definition*.
   We know `yo` is called at bytes 24..28 of `src/lib.rs`; nothing knows that's inside
   `hello`. There is no (file, offset) → `NudoxPath` structure in the codebase, and no
   IR entry carries a declaration span in any language.
3. **The IR-side channel is structurally dead.** `Function.body: Option<ParsedBody>`
   (`ir/function.rs:43`, serde-skip, yoked live C tree) is set to `None` by **all six**
   producers; `graph/from_ir.rs:1004-1013` reads it to build `Reference` nodes, so the
   graph's reference corpus is always empty. Two artifacts were built to meet in the
   middle and never did.
4. **The CST output is discarded at assembly.** `BlobInfo::assemble` takes `surface`
   and `cst` and drops both (`generate/blob_info.rs:34`, `let _ = (surface, cst)`).
5. **Classifier fidelity is one-level-deep.** The classifier sees `(text, kind,
   parent_kind, span)` only: scoped paths (`foo::bar::baz`) arrive as disjoint
   identifiers, `ReferenceKind::MethodCall` is declared but never emitted, imports are
   captured as bare names without their binding structure, and definitions are not
   captured at all.
6. **No query/serve path.** `get_references` exists but nothing populates the data it
   reads, and no server endpoint turns symbol → usages → snippet.

### Standing assets that make this cheap

- The **Linker's ladder** (exact → alias → suffix) is exactly the package-index
  resolution a syntactic resolver needs — it just resolves to IRIs at emit time
  instead of `NudoxPath`s at generate time. Lift it; don't rewrite it.
- The **source archive** stage already content-addresses every source file
  (`generate/mod.rs`, stage 3) — snippet serving needs no new storage.
- `Reference` graph nodes are **content-addressed** (value_hash) — re-emission after a
  resolver improvement dedups instead of duplicating.
- Symbol IRIs are **version-agnostic** (`Symbol/{lang}%2F{pkg}%2F{fq}` — GRAPH-ARCHITECTURE
  §7), so cross-package "used by" aggregates across versions for free.
- Producers with real semantic resolution already exist per the active plans: RA
  `Semantics` (RUST-ANALYZER-PLAN P4/P5 explicitly earmarks bodies → `mentions`/
  `resolves_to`), go/types (`Info.Uses` is one oracle field away), `oxc_semantic`
  (OXC-PLAN Phase 3's reference→symbol chain), pyrefly. These become **precision
  upgrades**, not prerequisites.

---

## 2. Design principles

1. **One artifact, many producers.** Define a single language-agnostic *occurrence*
   contract. Tree-sitter is the universal baseline producer (works for all 6 languages
   the same day); semantic oracles upgrade individual languages later by emitting the
   *same artifact* at higher confidence. Consumers never know which produced it.
2. **The IR stays declaration-only.** Do not smuggle syntax back into `ir::Entry`
   (README non-goal). Occurrences are a *sibling corpus* keyed by `NudoxPath` +
   file/span, stored beside `ir_ref` in the blob, projected beside the surface into
   the graph. This also retires the dead `Function.body` channel.
3. **Resolution is a ladder with honesty.** Every resolved occurrence carries a
   confidence tier. Uncertain answers are kept (blob) but not asserted (graph). The
   system's precision is *observable* (per-package resolution-rate stats), so oracle
   upgrades are measurable, not vibes.
4. **Attribution never fails.** An enclosing definition that isn't in the surface
   index (private helper, local fn) still yields a syntactic FQN — "used in
   `crate::internal::helper`" is a correct answer even if `helper` has no IR entry.
   Anchored-vs-unanchored is a flag, not a filter.
5. **Deterministic and cache-honest.** The resolve stage is keyed on (source hash ×
   surface hash × resolver version). Bumping classifier/resolver logic bumps the key.

---

## 3. The core contract: `Occurrence`

New module `ir/syntax/occurrence.rs` (IR crate so producers, generate, graph, and
registry all share it):

```rust
/// Whether this span *is* the thing or *uses* the thing.
pub enum Role { Definition, Reference }

/// How the target path was established. Ordered: higher = more trustworthy.
pub enum Confidence {
    /// Raw identifier only; target is a best-effort name. Never graph-asserted.
    Syntactic,
    /// Unique last-segment match against the package index.
    Suffix,
    /// Exact/alias match against the package index, or module-scope sibling.
    Index,
    /// Resolved through the file's import table (incl. external deps).
    Import,
    /// Resolved by the language's semantic oracle (RA / go-types / oxc / pyrefly).
    Oracle,
}

pub struct Occurrence {
    /// Byte range in the file (tree-sitter native; line/col derived lazily).
    pub span: Range<usize>,
    /// Fully-qualified target. External deps use NudoxPath::External.
    pub target: NudoxPath,
    pub kind: ReferenceKind,          // existing enum; MethodCall finally emitted
    pub role: Role,
    /// Innermost enclosing definition (syntactic FQN), None at module top level
    /// (target then attributes to the module entry).
    pub enclosing: Option<NudoxPath>,
    /// True when `enclosing` matched an entry in the surface index.
    pub anchored: bool,
    pub confidence: Confidence,
}

pub struct FileOccurrences { pub path: PathBuf, pub occurrences: Vec<Occurrence> }

pub struct OccurrenceSet {
    pub files: Vec<FileOccurrences>,          // sorted by path
    pub stats: ResolutionStats,               // totals per kind × confidence,
                                              // unresolved count — the honesty meter
}
```

Notes:

- `Role::Definition` occurrences give every IR item a **declaration span for free**
  (file + byte range), closing the "IR has no spans" gap without touching IR structs
  or the wire schema. The examples feature, doc deep-links, and future go-to-def all
  read the same rows.
- `Occurrence` replaces `ResolvedReference` at package level. `ResolvedReference`
  survives (unchanged) inside the snippet path (`TreesitterRepr`) until Phase 3
  unifies them; the walker tests keep their subject.
- Unresolved references are **not** stored as occurrences; they're tallied in
  `stats`. (Rejected: an `Ambiguous(Vec<NudoxPath>)` target variant — it pollutes the
  wire and every consumer for a diagnostic concern. Candidate sets can be logged.)

---

## 4. Architecture: five boxes

```
             ┌────────────────────────────────────────────────────────┐
 source ────▶│ 1 EXTRACT (per file, per language: LanguageSpec)       │
             │   definitions · imports · qualified references         │
             └───────────────┬────────────────────────────────────────┘
                             ▼
 surface ───▶┌────────────────────────────────────────────────────────┐
 (ir::Index) │ 2 RESOLVE + ATTRIBUTE (language-agnostic engine)       │
             │   scope ladder → NudoxPath · span containment → encl.  │
             └───────────────┬────────────────────────────────────────┘
                             ▼  OccurrenceSet
             ┌───────────────┴──────────────┬─────────────────────────┐
             ▼                              ▼                         ▼
 3 BLOB (CAS, occurrences_ref)   4 GRAPH (Reference nodes)   [oracle merge, P5]
                                            ▼
             ┌────────────────────────────────────────────────────────┐
             │ 5 QUERY  symbol → References → group by enclosing      │
             │          → snippet via source archive + parse_and_extract │
             └────────────────────────────────────────────────────────┘
```

### 4.1 Extract — `LanguageSpec` replaces the flat classifiers

The current classifier signature (`fn(&str, &str, Option<&str>, Range) →
Option<ResolvedReference>`) cannot express nesting, qualifier chains, or import
structure. Replace it with a per-language trait in `compiler/treesitter/` (one file
per language, superseding the match-arm blocks at `treesitter.rs:134-326`):

```rust
pub trait LanguageSpec: Send + Sync {
    /// Named definition sites with nesting: fn/method/type/trait/impl/class/mod…
    fn definitions(&self, tree: &Tree, src: &str) -> Vec<RawDefinition>;
    /// The file's name-binding table: use/import/from-import/static import/inherit.
    fn imports(&self, tree: &Tree, src: &str) -> Vec<ImportBinding>;
    /// Use-sites with their FULL qualifier chain captured as one reference.
    fn references(&self, tree: &Tree, src: &str) -> Vec<RawReference>;
    /// File path → module path under this language's layout conventions.
    fn module_path(&self, rel: &Path, layout: &PackageLayout) -> Vec<String>;
}

pub struct RawDefinition {
    pub name: String,
    pub kind: DefKind,                 // Fn, Method, Type, Trait, Impl{of}, Mod, Class…
    pub name_span: Range<usize>,
    pub body_span: Range<usize>,       // full item extent — the containment interval
    pub parent: Option<usize>,         // index into the defs vec → nesting chain
}

pub enum ImportSource {
    Internal(Vec<String>),                       // use crate::a::b  /  from .a import b
    External { dependency: String, path: Vec<String> },  // use serde::Serialize
    Glob { prefix: ImportPrefixRef },            // use x::*  /  from x import *
}
pub struct ImportBinding { pub local: String, pub source: ImportSource, pub span: Range<usize> }

pub struct RawReference {
    pub segments: Vec<String>,         // ["foo","bar","baz"] for foo::bar::baz — ONE ref
    pub span: Range<usize>,            // span of the whole chain
    pub kind: ReferenceKind,
    pub receiver: Option<ReceiverShape>, // Some for x.m(): the syntactic receiver text
}
```

Implementation stays cursor-walk (as today), not `.scm` queries — **decision**: we
need structured output (parent indices, chain assembly, receiver shapes) that query
captures would only defer to Rust post-processing anyway; no `.scm` files exist in
the repo or arborium wiring today; and hand-written walks are directly unit-testable
against the 22-case walker suite. Revisit only if grammar-kind churn (see §7 risk 3)
becomes the dominant maintenance cost.

Each language's extractor is small and testable; the walker (`syntax/walker.rs`)
grows a cursor helper that yields `(node, depth, parent_kind)` so extractors share
traversal. `MethodCall` is emitted for receiver-based calls in every language, at
last.

### 4.2 Resolve + attribute — one engine, zero language knowledge

New `generate/resolve.rs`. Inputs: `Vec<(path, Extraction)>` from 4.1 + `ir::Index`.
Language specifics end at the `Extraction` boundary.

**SymbolTable** — lift the Linker's index construction (`graph/link.rs:126-181`) into
a shared component (new `ir/syntax/symtab.rs` or `graph/symtab.rs`) that maps to
`NudoxPath` instead of IRI strings:

- `exact: HashMap<String /* fq "a::b::c" */, NudoxPath>` — from `Index` paths **and
  every alias spelling** (`Symbol.aliases` already carries re-exports, `kind.rs:38`).
- `suffix: HashMap<String, Option<NudoxPath>>` — last segment, `None` on collision.
- `by_module: HashMap<Vec<String>, HashSet<String>>` — members per module path, for
  glob-import and sibling-scope checks.

The Linker then *consumes* this table (thin IRI-encoding wrapper), so graph emit and
occurrence resolution can never drift apart. One resolution semantics, two callers.

**Resolution ladder** for each `RawReference` in a file with module path `M`,
enclosing-definition chain `C`, and import table `I`:

1. **Local shadow check** — if the head segment names a local binding/parameter/
   inner definition whose scope contains the span, the reference is `VariableUse`
   at `Syntactic` and never escalates. (Prevents a local `fn yo` or `let yo` from
   claiming `crate::yo`'s call sites.)
2. **Qualifier-chain absolute** — chain starting with the crate/package root or a
   language absolute marker (`crate::`, `self::`, `super::` folded against `M`;
   full dotted module path in Python; package qualifier in Go/Java) → join →
   `exact` lookup → `Confidence::Index`.
3. **Import binding** — head segment ∈ `I`: substitute the binding, re-join the
   tail. `Internal` → `exact` lookup (`Import` confidence); `External` →
   `NudoxPath::External { dependency, path }` (`Import`); `Glob` → try each glob
   prefix via `by_module` (`Import` if unique, else unresolved).
4. **Lexical scope walk** — try `M ⧺ C′ ⧺ chain` for each enclosing prefix `C′`
   from innermost to file root (nested fns, methods on the surrounding impl/class,
   file-level siblings) → `exact` → `Index`.
5. **Package-wide exact/alias** — chain joined as-is against `exact` → `Index`.
6. **Unique suffix** — leaf segment against `suffix` → `Suffix`. For
   `MethodCall`, suffix match is attempted against method-bearing entries only
   (the index knows `Function.members` / record members); non-unique → unresolved.
7. **Unresolved** — tallied in `stats`, not emitted.

**Attribution**: sort each file's `RawDefinition` body spans; innermost containment
(spans are properly nested by construction — a tree) via binary search over start
offsets with a nesting stack. The enclosing definition's syntactic FQN = `M ⧺` names
along its `parent` chain, normalized per language (Rust `impl T` frame contributes
`T`; Python/TS/Java class frames contribute the class name; anonymous frames —
closures, lambdas, static blocks — are skipped so attribution lands on the nearest
*named* ancestor; no named ancestor → module entry, `enclosing: None`). Then anchor:
FQN ∈ `exact` (including aliases) ⇒ `anchored: true` and the *index's* canonical
path is used (so `impl` method spellings normalize to the IR's `Type::method` form);
otherwise emit the syntactic FQN unanchored (principle 4).

**Definition occurrences**: every `RawDefinition` also emits
`Occurrence { role: Definition, target: <its own FQN>, span: name_span, … }` —
anchored ones give IR entries their declaration coordinates.

### 4.3 Pipeline & blob

`generate/mod.rs` stage 2 changes from independent-of-surface to downstream-of-surface:

```
1 surface::build            (unchanged, key: job)
2 occurrences::build        (extract + resolve; key: job ⊕ b"occ" ⊕ RESOLVER_VERSION ⊕ surface_hash)
3 source_archive::build     (unchanged, key: job ⊕ b"archive")
4 BlobInfo::assemble        (unchanged identity: archive-only fold — occurrences are
                             derived data and MUST NOT perturb the snapshot hash)
```

- `CstSet`/`cst.rs` is renamed/absorbed into `generate/occurrences.rs`; the file-walk,
  grammar dispatch, skip rules, and non-UTF-8 handling at `cst.rs:63-126` carry over
  verbatim.
- Blob: new CAS section `BlobManifest.occurrences_ref: ContentHash` with a
  **version-prefixed** postcard encoding (postcard isn't self-describing; the section
  header byte is the format version). `references_ref` and its `ReferenceSet` codec
  (`blob/mod.rs:215-367`) are kept through one release for the migration window, then
  deleted — nothing external consumes them today (`indexing.rs:628` is the only
  writer-side caller). Update the `blob_hash_pins` golden values (manifest field
  addition changes `manifest_cas_key()`; extend `identity_bytes()` **only if** we
  decide occurrences affect freshness — we decide they don't).
- **Retire `Function.body`.** Delete the field (`ir/function.rs:43`), the six
  `body: None` sites, and the dead read at `from_ir.rs:1004-1013`. `ParsedBody`/
  `FunctionBody` (yoke machinery, `syntax/body.rs`) remain solely as the snippet
  extractor's internal type. The IR gets *simpler* here, not richer.

### 4.4 Graph

- `graph::from_ir::project(index, ctx)` → `project(index, occurrences: &OccurrenceSet, ctx)`.
  Reference corpus is built from occurrences instead of the never-populated body path;
  `Reference.source` = the enclosing symbol (resolved via the shared SymbolTable),
  `target` = the occurrence target (external targets mint stubs exactly as
  `resolve_name` does today, `link.rs:218`).
- `model::Reference` (`model.rs:1027`) gains additive optional fields:
  `file: Option<String>`, `confidence: Option<String>` (schema re-upload; value_hash
  keying means re-emit dedups).
- **Assertion policy** (graph ⊂ blob): only `role: Reference`, kinds
  `{FunctionCall, MethodCall, TypeReference, MacroInvocation, Import}`, confidence
  `≥ Index`, and anchored-or-external targets are emitted as `Reference` nodes.
  `VariableUse`/`FieldAccess` and `Suffix`-confidence rows stay blob-only — the
  graph asserts, the blob records. (This also caps graph volume: variable uses
  dominate raw counts by an order of magnitude.)
- Definition occurrences emit nothing new in the graph (the Symbol node already
  exists); their spans are served from the blob.

### 4.5 Query & the examples feature

Server-side (`workspace/server` + `runtime`):

1. **Question** — resolve user input to a symbol: existing tantivy fq/name/subtoken
   search (`runtime/text/index.rs`) → `SymbolId` → symbol IRI.
2. **Who** — `get_references(SymbolId)` (`runtime/graph/mod.rs:89`), now non-empty.
   Group by `Reference.source` (= enclosing fn), yielding exactly the dream's answer
   shape: *"used in `calculator::hello`"* — both sides fully qualified.
3. **Show** — for each usage: `file` + span → fetch the file's bytes from the source
   archive via the blob manifest (path → ContentHash → CAS) → `parse_and_extract`
   (`treesitter.rs:446`) with the occurrence span → enclosing-function snippet with
   centered-window fallback. This is `parse_and_extract`'s first production caller;
   its `TreesitterRepr` payload (sexp + refs) rides along for the embedding pipeline
   unchanged.
4. **Rank** — order usages: `Oracle > Import > Index` confidence, then prefer
   anchored enclosings, doc-comment-rich callers, and span diversity (≤ N per file);
   paginate.
5. **Cross-package "used by"** — reverse lookup on External-target `Reference` nodes:
   all packages whose occurrences resolved to `Symbol/{lang}/{pkg}/{fq}` stubs.
   Version-agnostic IRIs aggregate across versions automatically. Corpus-wide
   backfill = re-run the occurrence stage per package (CAS-keyed, so it's a walk,
   not a redesign).

---

## 5. Per-language extraction specifics & edge-case catalog

The engine (§4.2) is shared; this catalog is what each `LanguageSpec` must get right,
and doubles as the adversarial-fixture checklist (§6). *Handled* = resolved by the
syntactic tier; *oracle* = needs the Phase-5 semantic tier; *out* = documented
non-goal.

**Rust** — module path from file layout (`lib.rs`/`mod.rs`/`foo.rs` + inline `mod`
frames as definitions); `use` trees incl. nested `{}`, `as` renames, glob; `crate::`/
`self::`/`super::` folding; impl frames normalize to the ADT path; UFCS
`Type::method()` (handled — it's a qualifier chain); trait-method calls through a
receiver (*oracle*); references inside `macro_rules!` bodies and macro-invocation
token trees (*oracle*; syntactic tier records the `MacroInvocation` itself and skips
inside — token-tree identifiers are unclassifiable); `#[path]` module attrs (*out*,
tallied unresolved); `#[cfg]` duplicate defs (both emitted; anchoring picks the one
the index kept); shadowing locals (ladder step 1); free-fn `source_map`
(`ra/source.rs`) extends to methods when snippets need them, or is superseded by
Definition occurrences + archive slicing (preferred — one snippet path, not two).

**Python** — module path from package layout (`__init__.py`, namespace dirs);
`import x.y`, `from .rel import z` (fold relative dots against `M`), `as` renames,
`from x import *` (glob); class frames; `self.method()`/`cls.method()` attribute to
the enclosing class if the method exists on it (`by_module`), else *oracle*;
decorators are references (handled); module-level statements attribute to the module
entry; dynamic access (`getattr`, `__getattr__`) *out*.

**TypeScript/JS** — module path = file specifier (mirror deno/oxc
`assign_unique_module_names`); ESM imports incl. default, namespace
(`import * as ns` — a bound prefix, not a glob), renames, re-export barrels
(follow one hop through the index's aliases; deeper chains *oracle* until OXC-PLAN
Phase 3 lands and its semantic `type_links` machinery feeds the Oracle tier);
`export default` anonymous fns attribute to the module; JSX component usage =
`TypeReference` (handled — `tsx` grammar wired); CommonJS `require` (best-effort
pattern for `const x = require('y')`, else unresolved); method calls *oracle*.

**Go** — package import table maps package identifiers → import paths, so
`fmt.Println` and any cross-package call resolves **syntactically** — Go is the
strongest baseline language; dot-imports = glob; receivers/promoted methods via
embedding (*oracle* — but `go/types` `Info.Uses` makes the Go oracle the cheapest
Phase-5 win, and `oracle.Decl.pos` fields already deserialized-and-dropped at
`compile/go/oracle.rs:116` prove the plumbing); `func (r T) m()` frames normalize to
`T.m` matching the producer's `import/path::Type.Method` scheme (`go/context.rs:88`).

**Java** — package decl + explicit imports + static imports; same-package references
need no import (module-scope step 4 covers); inner classes via nesting chain; method
references `Foo::bar` (handled — qualifier chain); unqualified instance-method calls
and overload selection *oracle* (name-level answers are still correct — overloads
share an FQN); `java.lang.*` implicit import = a built-in glob prefix.

**Nix** — attrpath definitions from the static rnix layer's conventions;
`inherit (src) a b` = imports; `with pkgs;` is scope-destroying — references under a
`with` resolve only if unique in the `with` subject's attrset when the index knows it,
else tallied unresolved (honesty over guessing); `callPackage` argument injection
*oracle* (snix); builtins catalogued (`compile/nix/builtins.rs`) resolve as External.

**Universal** — skip vendored/generated dirs (extend the `cst.rs:78` skip list with
`vendor`, `.venv`, `dist`, `build`, generated-file heuristics); non-UTF-8 skip stays;
per-file parse timeout + max-file-size guard (huge minified JS); references inside
strings/comments are structurally impossible (tree-sitter node kinds), which is half
the reason this beats grep.

---

## 6. Testing strategy

1. **Extractor unit tests** — per-language `LanguageSpec` tests in the walker-suite
   style (`syntax/tests/walker.rs`'s 22 cases retarget to the Rust extractor);
   definitions/imports/references asserted separately.
2. **Occurrence snapshots** — new `tests/snap_occurrences_<lang>.rs` for all six
   languages over the existing snippet fixtures (the 106-snap insta harness pattern):
   snapshot the resolved table `(file, span, kind, target, enclosing, confidence,
   anchored)` sorted. This is the primary regression net.
3. **Adversarial fixtures** — one fixture per §5 catalog row that's *handled*
   (shadowing, renamed imports, globs, nested defs, same-leaf-different-module,
   re-export alias hit, UFCS, JSX, dot-import, static import, `with`-scope). Rows
   marked *oracle*/*out* get a fixture asserting the honest outcome (unresolved
   tallied, not mis-resolved) — wrong answers are worse than no answers.
4. **The dream test, literally** — e2e on the `calculator` fixture: build, emit,
   query `get_references` for a known fn, assert the exact enclosing-FQN set; then
   snippet-serve one usage and assert the call site is inside the snippet.
5. **Differential harness (Phase 5 gate)** — Rust fixtures resolved by both tiers;
   assert the syntactic tier never *contradicts* RA on references both resolve
   (agreement metric), and record the coverage delta. Repeat per oracle.
6. **Grammar-pin tests** — per language, parse a sentinel snippet and assert the
   node-kind names the extractor matches on (`call_expression`,
   `member_expression`, …) still exist. Arborium bumps then fail loudly at test
   time, not silently as empty extractions (the `property_access_expression` /
   `member_expression` dual-accept at `treesitter.rs:198` shows this drift is real).
7. **Stats floor** — per-language fixture resolution-rate assertions
   (e.g. Rust fixture ≥ 90% of call references resolved at ≥ Index) so precision
   regressions fail CI.

---

## 7. Risks & mitigations

1. **Suffix-tier false positives** (unique-in-package leaf that's actually a local
   or an external). Mitigated: shadow check runs first, suffix rows are graph-excluded
   by the assertion policy (blob-only), and method suffix-matching is restricted to
   method-bearing entries.
2. **Postcard rigidity.** Every wire struct here is postcard-in-CAS; additive change
   = new section version byte + new ContentHash. The version-prefixed section (§4.3)
   is the mechanism; the `blob_hash_pins` goldens are the tripwire.
3. **Grammar churn** (arborium 2.18.1 pinned, node-kind renames across bumps).
   Grammar-pin tests (§6.6) + extractor-per-language isolation keep the blast radius
   to one file.
4. **Volume.** Reference nodes for a large package can reach 10⁵–10⁶. The assertion
   policy caps graph writes to call/type/import kinds at high confidence; blob holds
   the rest; value_hash dedup makes re-emits idempotent. If still hot, shard wave-4
   emission per file (the emit path is already wave-ordered).
5. **Stage coupling** (occurrences now depend on surface). Cache key composition
   (§4.3) keeps correctness; cost is that a producer bump re-runs resolution —
   acceptable, resolution is cheap relative to producers.
6. **Two resolvers drifting** (Linker vs occurrence engine). Structurally prevented:
   both consume the one SymbolTable (§4.2).

---

## 8. Phases

**Phase 0 — Contracts & foundations** *(small, unblocks everything)*
- `Occurrence`/`OccurrenceSet`/`Role`/`Confidence` in `ir/syntax/occurrence.rs`.
- Extract `SymbolTable` from `graph/link.rs`; Linker becomes its IRI-encoding consumer;
  existing graph snapshots must be byte-identical (pure refactor gate).
- Delete `Function.body` + the six `None` sites + the dead `from_ir.rs:1004` path.
- Exit: workspace green, graph emit output unchanged.

**Phase 1 — Structural extraction (`LanguageSpec` × 6)**
- Trait + cursor helper; port/replace the six classifiers; emit definitions, imports,
  qualified chains, receivers; `MethodCall` live.
- Rust and Go first (best-understood layouts), then Python/TS, then Java/Nix.
- Exit: extractor unit suites green per language; walker tests migrated.

**Phase 2 — Resolution + attribution engine**
- `generate/resolve.rs`: ladder, module-path derivation, containment attribution,
  anchoring, definition occurrences, stats.
- Exit: occurrence snapshots (§6.2) landed for all six fixtures; adversarial fixtures
  (§6.3) green; stats floors set.

**Phase 3 — Pipeline, blob, graph**
- `cst.rs` → `occurrences.rs`; stage re-keyed on surface hash + RESOLVER_VERSION;
  `occurrences_ref` blob section (versioned); `project()` takes occurrences; additive
  `Reference` fields; assertion policy; hash-pin goldens updated; `references_ref`
  deprecated.
- Exit: e2e dream test (§6.4) passes against a live emit: question in, FQNs out.

**Phase 4 — Query & examples surface**
- Server endpoint: symbol → usages (grouped by enclosing, ranked, paginated) →
  snippets via archive + `parse_and_extract`. Cross-package used-by via External
  stubs.
- Exit: fixture-backed API test returns the `hello`/`yo` answer with a snippet.

**Phase 5 — Oracle precision tier** *(independent per language, any order)*
- Producers emit `Confidence::Oracle` occurrences through `AuxOutputs`; engine merges
  with oracle-precedence per (file, overlapping span).
- Go first (`Info.Uses` — days, not weeks), then Rust (RA `Semantics`, per
  RUST-ANALYZER-PLAN P4/P5), TS (rides OXC-PLAN Phase 3), Python (pyrefly), Java
  (Trees API, best-effort), Nix (snix fusion, best-effort).
- Exit per language: differential harness agreement + coverage delta recorded; method
  calls resolved.

**Phase 6 — Corpus hardening**
- Backfill walk over indexed packages; volume tuning; ranking polish; resolution-rate
  dashboards from `stats`; retire `ReferenceSet` codec.
- Exit: cross-package "used by" live on the corpus; stats visible per package.

Dependencies: 0 → 1 → 2 → 3 → 4 are sequential; 5 starts any time after 3 (per
language, independently); 6 after 4.

---

## 9. Decisions (settled here, with the rejected branch)

| Decision | Rejected alternative | Why |
| --- | --- | --- |
| Occurrences are a sibling corpus keyed by path+span | Populate `Function.body` per producer | serde-skip yoked C trees can't persist, forces every producer to parse, violates IR's no-syntax rule; the field is deleted instead |
| One shared SymbolTable under both Linker and resolver | Second resolver in generate | Drift between emit-time and generate-time resolution would be unfindable |
| Cursor-walk extractors | `.scm` query files | Structured output (nesting/chains/receivers) needs Rust post-processing regardless; nothing in repo/arborium wires queries today; revisit on grammar-churn pain |
| Confidence tiers + graph assertion policy | Assert everything / oracle-only | Baseline everywhere now, upgrades measurable, graph never lies |
| Unresolved → stats, not wire | `Ambiguous(Vec<…>)` target variant | Keeps wire and consumers simple; honesty lives in stats |
| Occurrences excluded from snapshot identity | Fold into `identity_bytes` | Derived data must not churn package freshness |
| Byte spans on the wire, lines derived | Line/col storage | Tree-sitter native, snippet slicing exact; line index is cheap and lazy |
| Snippets from source archive + `parse_and_extract` | Extend RA `source_map` to methods, per-language body capture | One language-agnostic snippet path; `source_map` becomes redundant and can retire |
