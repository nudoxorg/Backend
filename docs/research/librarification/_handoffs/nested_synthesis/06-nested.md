# Nested synthesis for task 06

# Nested agent agent-ac4199cc395c66067
Description: Research SCIP, LSIF, Kythe, and Unison symbol identity
Size: 309523

Good initial results. Now let me fetch the primary source documents for deep, specific details.

---

Excellent. Now let me fetch deeper details on SCIP symbol stability, LSIF moniker design specifics, Kythe's active development state, and Unison's patch model.

---

I have enough comprehensive information to synthesize a thorough report. Let me compile all findings.

---

# Research Findings: SCIP, LSIF, Kythe VName, and Unison Content-Addressing

---

## TOPIC 1: SCIP — Sourcegraph Code Intelligence Protocol

### Exact Symbol Grammar

The canonical BNF grammar from `scip.proto` ([https://github.com/sourcegraph/scip/blob/main/scip.proto](https://github.com/sourcegraph/scip/blob/main/scip.proto)):

```
<symbol>              ::= <scheme> ' ' <package> ' ' (<descriptor>)+ | 'local ' <local-id>
<package>             ::= <manager> ' ' <package-name> ' ' <version>
<scheme>              ::= any UTF-8, escape spaces with double space
                          MUST NOT be empty, MUST NOT start with 'local'
<manager>             ::= any UTF-8, escape spaces with double space
                          Use '.' to indicate empty value
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
<name>                ::= <identifier>
<method-disambiguator>::= <simple-identifier>
<identifier>          ::= <simple-identifier> | <escaped-identifier>
<simple-identifier>   ::= (<identifier-character>)+
<identifier-character>::= '_' | '+' | '-' | '$' | ASCII letter or digit
<escaped-identifier>  ::= '`' (<escaped-character>)+ '`'
<local-id>            ::= <simple-identifier>
```

Descriptor suffix semantics:

| Suffix | Token | Meaning |
|--------|-------|---------|
| Namespace | `/` | Package/module/namespace scope |
| Type | `#` | Type/class/interface/enum |
| Term | `.` | Term-level definition (function, value, field) |
| Method | `().` | Method, with optional disambiguator for overloads |
| TypeParameter | `[name]` | Generic/type parameter |
| Parameter | `(name)` | Function/method parameter |
| Meta | `:` | General metadata |
| Macro | `!` | Macro definition |

**Important:** The descriptor chain must form a fully qualified name. Local symbols (`local <id>`) are document-scoped only and MUST NOT be referenced outside their Document vertex.

### Concrete Symbol Examples by Ecosystem

From confirmed sources:

- **TypeScript (scip-typescript, npm):** `scip-typescript npm @sourcegraph/scip-typescript 0.2.0 src/FileIndexer.ts/scriptElementKind().`
  - Scheme: `scip-typescript`, Manager: `npm`, Package: `@sourcegraph/scip-typescript`, Version: `0.2.0`
- **Rust (rust-analyzer, cargo):** `rust-analyzer cargo ra-test 0.1.0 Point#`
  - Scheme: `rust-analyzer`, Manager: `cargo`, Package: `ra-test`, Version: `0.1.0`, Descriptor: `Point#` (a type)
- **Java (scip-java, Maven):** scheme `scip-java` (or `lsif-java`), manager `maven`, package `org.slf4j/slf4j-api`, descriptor chain like `com/example/MyClass#myMethod().`
- **Go (scip-go):** scheme `scip-go`, manager `gomod`, package like `example.com/api`, descriptor like `Handler().`

The '.' character is used as a placeholder for any of the package fields (manager, name, version) when the value is not applicable or unknown (e.g., for local/unpublished crates).

### Stability Across Versions

**SCIP symbols are version-pinned by design.** The package triple `(manager, name, version)` is embedded in every symbol, so cross-repository navigation works by treating the concatenation of `(package_name, package_version, qualified_symbol_name)` as the unique global ID. This means:

- A symbol from `lib@v1.0.0` and the same symbol from `lib@v2.0.0` are **different** SCIP symbol strings. They do not alias each other.
- The version field **must be consistent** across indexing runs and **must not be reused** for different code snapshots.
- There is no explicit cross-version continuity mechanism — a consumer library pinned to `lib@v1.0.0` navigates to `lib@v1.0.0`'s SCIP index, not `v2.0.0`'s.
- The `SymbolFormatter` API in the Go bindings allows programmatically stripping the version component for version-agnostic matching if needed, but this is not standard protocol behavior.

Sources: [https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip](https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip), [https://github.com/sourcegraph/scip-clang/blob/main/docs/CrossRepo.md](https://github.com/sourcegraph/scip-clang/blob/main/docs/CrossRepo.md)

### Cross-Commit Navigation with Stale Indexes

Sourcegraph's strategy for stale indexes is **graceful degradation to search-based navigation**:

1. When precise SCIP data exists for a commit, it is used directly.
2. When browsing a commit **between** indexed commits, search-based code navigation (symbol search) is used as fallback.
3. When a line containing the symbol was "created or edited between the nearest indexed commit and the commit being browsed," search-based navigation fills the gap.
4. When repo A has a SCIP index and dependency repo B does not (or has it at a different version), cross-repo results fall back to search-based for the missing side.
5. SCIP's design enables **incremental indexing** (only changed files need re-indexing, unlike LSIF's globally-incrementing ID requirement), which reduces the staleness window significantly.

Sources: [https://sourcegraph.com/docs/code_intelligence/explanations/precise_code_navigation](https://sourcegraph.com/docs/code_intelligence/explanations/precise_code_navigation), [https://sourcegraph.com/blog/cross-repository-code-navigation](https://sourcegraph.com/blog/cross-repository-code-navigation)

### Moniker Design Lessons from SCIP

SCIP explicitly eliminated the LSIF moniker abstraction. The key design insight: **replace the import/export moniker graph with human-readable, self-describing string identifiers for symbols**. The entire symbol string — scheme + package + descriptor chain — replaces what LSIF needed a graph of moniker vertices, packageInformation vertices, and nextMoniker edges to express. This also eliminates the need to determine export vs. import status at indexing time (a problem for C++ and other languages without explicit export syntax).

---

## TOPIC 2: LSIF and Why SCIP Replaced It

### LSIF Moniker System Design

LSIF (Language Server Index Format) represents cross-repository symbol identity as a **graph of vertices and edges**:

- **Moniker vertex** — carries a `kind` (export | import | local), a `scheme` (e.g., `tsc`, `npm`), and an `identifier` string (e.g., `lib/index:Emitter.emit`)
- **PackageInformation vertex** — carries `name`, `manager`, `version`, optionally `repository`
- **`packageInformation` edge** — links a moniker to its package
- **`nextMoniker` edge** (v0.4.0) / **`attach` edge** (v0.5.0) — chains monikers together: a compiler-specific moniker (e.g., `tsc`) chains via `nextMoniker` to a package-manager-aware moniker (e.g., `npm`)
- **Unique property** (v0.5.0) — added to encode uniqueness scope: `document | project | group | scheme | global`

The directional edge `nextMoniker` encoded the export vs import direction: export went from tsc→npm moniker; import went from npm→tsc. In v0.5.0 this was replaced by the `unique` property + generic `attach` edge, because the directional encoding was confusing.

Sources: [https://microsoft.github.io/language-server-protocol/specifications/lsif/0.4.0/specification/](https://microsoft.github.io/language-server-protocol/specifications/lsif/0.4.0/specification/), [https://microsoft.github.io/language-server-protocol/specifications/lsif/0.5.0/specification/](https://microsoft.github.io/language-server-protocol/specifications/lsif/0.5.0/specification/)

### Failure Modes That Led to SCIP

Four specific failure modes documented by Sourcegraph:

1. **No static schema**: LSIF has no machine-readable schema; the dynamic graph structure makes encoding and validation difficult. Runtime errors dominate LSIF tooling bugs.

2. **Performance**: The graph encoding required holding large in-memory structures during both write (indexing) and read (query). Globally incrementing opaque vertex IDs made partial updates impossible — the entire index had to be regenerated on any change.

3. **Incremental indexing impossibility**: Globally incrementing IDs mean you cannot update an existing index with new information for only a subset of documents. SCIP's document-centric design with string symbol IDs allows this.

4. **Import/export moniker fragility**: "Unnecessary abstractions like import/export monikers that silently break navigation if you get it wrong." The C++ case is the clearest: C++ has no explicit export syntax, making it impossible to determine at indexing time whether a symbol is definitively exported or imported. The spec acknowledges this with the `kind` field becoming optional in v0.5.0 — but this also means you lose the directional information needed for cross-repo linking. For symbols defined in shared headers (inline functions, templates), there is no single "home" project, breaking the import/export classification entirely.

5. **Opaque IDs**: Numeric vertex IDs make indexes impossible to inspect manually and impossible to merge across separately-indexed sub-projects.

Sources: [https://sourcegraph.com/blog/announcing-scip](https://sourcegraph.com/blog/announcing-scip), [https://github.com/microsoft/lsif-node/issues/44](https://github.com/microsoft/lsif-node/issues/44)

### The "External Moniker" Concept

An external moniker in LSIF is an import-kind moniker that references a symbol defined in an external package. The workflow is:

1. Compiler emits a tool-specific moniker (e.g., tsc scheme, identifier `node_modules/lib/index:Emitter`)
2. A post-processing step transforms this to a package-manager moniker (npm scheme, identifier `lsif-ts-sample::Emitter`)
3. A `nextMoniker` edge (v0.4) or `attach` edge (v0.5) links these
4. The package-manager moniker is linked to a `packageInformation` vertex with name/version/manager
5. Cross-repo linking works when a dependent repo's export moniker matches this import moniker by scheme+identifier

The fundamental problem: **the match must be exact**. Any discrepancy in identifier format between the exporting indexer and the importing indexer silently breaks navigation with no error signal.

---

## TOPIC 3: Kythe VName Design

### VName Fields and Semantics

Kythe identifies every graph node with a **VName** — a 5-tuple of UTF-8 strings ([https://kythe.io/docs/kythe-uri-spec.html](https://kythe.io/docs/kythe-uri-spec.html)):

| Field | Role | Stability |
|-------|------|-----------|
| **corpus** | Repository/project identifier (e.g., `github.com/foo/bar`). Acts like a hostname. Meaning is corpus-defined. | Stable per project identity |
| **root** | Subdivision within a corpus (branch, build variant). Often empty. | Variable; corpus-specific |
| **path** | File path relative to corpus+root, never starting with `/`. | Stable per file identity |
| **language** | Language identifier (`c++`, `java`, `go`). Empty for file nodes. | Stable |
| **signature** | The entity-specific unique string within (corpus, root, path, language). Should be deterministically generated from the same input. | Consistent within one indexer run; not guaranteed stable across versions |

URI format: `kythe:[corpus]?[lang=<language>][path=<path>][root=<root>]#[signature]`

The field order in the URI is fixed (for canonical encoding): corpus, language, path, root, then signature in the fragment. All reserved characters must be percent-escaped with NFKC normalization.

Key quote from the spec: "the meaning of the strings generated by the `corpus` production is not defined in this specification" — corpus semantics are deliberately left to each deployment.

Source: [https://kythe.io/docs/kythe-uri-spec.html](https://kythe.io/docs/kythe-uri-spec.html)

### Cross-Version Entity Continuity

Kythe has **no built-in cross-version identity mechanism**. The schema explicitly states that signatures need not be stable across different versions of the input — only consistency within a single indexer run is required. There is no "generates", "supersedes", or "version-of" edge type for tracking entity lineage across commits.

The only version-related mechanism is pragmatic: if you re-index the same code, you get the same VNames (deterministic generation). But if the code changes, the signatures for changed entities will differ with no protocol-level link to the old signatures.

File nodes are noted to have "greater stability properties" — files are a natural stable anchor point since they are identified by corpus+root+path without signatures.

The `vnames.json` configuration file used by all Kythe indexers externalizes corpus/root assignment via regex matching, which means version-handling (e.g., encoding a commit SHA or version tag in the corpus or root) is left to deployment configuration.

Source: [https://kythe.io/docs/schema/writing-an-indexer.html](https://kythe.io/docs/schema/writing-an-indexer.html)

### Current Development State

**Actively maintained as of July 2026.** Release history from the RELEASES.md:
- v0.0.76: July 16, 2026
- v0.0.75: March 12, 2026
- v0.0.74: November 7, 2025
- v0.0.73: August 22, 2025

Release cadence is roughly every 2-3 months. Recent work spans Rust extractors, Go indexers, C++ indexers, Java tools, and TypeScript support. The project is a Google-originated open-source effort with multiple language implementations and an active test suite.

Source: [https://github.com/kythe/kythe/blob/master/RELEASES.md](https://github.com/kythe/kythe/blob/master/RELEASES.md)

---

## TOPIC 4: Unison Content-Addressed Definitions

### How Hash = Identity

In Unison, every definition is identified by a **512-bit SHA3 hash of its normalized AST**. The normalization process is precise:

1. **Names are erased**: All named references (function name, parameter names, local variable names) are replaced by positional/de Bruijn indices.
2. **Dependencies are replaced by their hashes**: All references to other definitions are replaced by those definitions' hashes recursively. This makes the hash of any definition a Merkle tree over the entire transitive closure of its dependencies.
3. **Whitespace and comments are excluded**: Only structural/semantic content participates.

Two functions with identical logic but different parameter names produce the **same hash**. Two functions with different logic but the same name produce **different hashes**.

The codebase is an append-only, content-addressed store: definitions are stored by hash, never overwritten, never deleted from the hash store.

Sources: [https://www.unison-lang.org/docs/the-big-idea/](https://www.unison-lang.org/docs/the-big-idea/), [https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/](https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/)

### Names as Metadata Over Hashes

Names exist in a separate **namespace layer** — a mapping from names to hashes. This is entirely separate from the definition store. Consequences:

- **Renaming is free and non-breaking**: Moving a name to point to a different hash does not alter the hash or any dependent's compiled representation. Dependents reference hashes, not names. A name change takes effect at the namespace level only.
- **Multiple names can point to the same hash**: Aliases are natural.
- **A name can change what hash it points to**: This is how "updating" a function works at the name level.
- **Names are ephemeral; hashes are permanent**: The identity of code persists even when all names are removed.

Source: [https://www.unison-lang.org/docs/faq/](https://www.unison-lang.org/docs/faq/)

### The "Same Function, New Body" = New Hash

When you edit a function body, Unison creates an entirely new definition with a new hash. The old definition remains in the store, permanently accessible by its old hash. The `update` command:

1. Adds the new definition (new hash) to the store
2. Reassigns the name to point to the new hash
3. Identifies all transitive dependents of the old hash
4. **Automatically propagates** the update to all dependents **if the type signature is compatible** (type-preserving edit): it re-elaborates each dependent against the new hash
5. If the type changes (e.g., added a parameter, changed a data constructor), automatic propagation fails for that dependent and Unison opens a scratch file with all unresolved dependents for manual intervention
6. Once the immediately-failing dependent is manually fixed and its type signature preserved, Unison resumes automatic propagation upward through the call chain

This propagation is **transitive and recursive** — it follows the entire dependency graph upward.

Sources: [https://www.unison-lang.org/docs/usage-topics/workflow-how-tos/update-code/](https://www.unison-lang.org/docs/usage-topics/workflow-how-tos/update-code/), [https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/](https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/)

### Patch/Update Model and What It Teaches About Lineage

A **patch** in Unison is a first-class data structure that records a set of **hash-to-hash replacement mappings** — it encodes the statement "old hash H1 is superseded by new hash H2." Patches are:

- **Name-independent**: They refer to hashes, so renaming operations do not affect them.
- **Composable**: Multiple patches can be combined to represent a sequence of library upgrades.
- **Published by library authors**: When a library releases a new version, it publishes a patch alongside the new code. Library consumers apply the patch to their own codebase, which propagates the update through their transitive dependents automatically.
- **Explicit lineage records**: A patch is an explicit, durable record that B is the successor of A. This is lineage encoded as data.

The commands `update`, `replace.term`, `replace.type` all build patch records. The `ucm` command `patch` applies them.

**Lessons for IR/symbol design:**
1. Content identity and name identity should be decoupled layers. Hash = what the thing IS; name = what humans call it.
2. Cross-version continuity requires explicit lineage recording (patch = hash-to-hash map). Without it, you can only detect "same name" not "same entity, evolved."
3. Propagation can be automated only when types are compatible. Type-changing updates require human guidance.
4. The append-only store means you can always navigate to any old version by hash — stale references never become dangling.

Sources: [https://www.unison-lang.org/blog/reducing-churn/](https://www.unison-lang.org/blog/reducing-churn/), [https://www.unison-lang.org/docs/faq/](https://www.unison-lang.org/docs/faq/)

---

## Synthesis: Key Cross-Cutting Observations

| System | Identity Primitive | Cross-Version Continuity | Stale Index Strategy |
|--------|-------------------|-------------------------|---------------------|
| **SCIP** | `scheme manager name version descriptor-chain` string | Version is embedded in symbol; no cross-version alias protocol | Fall back to search-based navigation; incremental re-index reduces staleness |
| **LSIF** | Graph: moniker vertex (kind+scheme+identifier) + packageInfo + edges | Same as SCIP — version-pinned, no alias mechanism | Same degradation model |
| **Kythe** | VName 5-tuple (corpus/root/path/language/signature) | No built-in mechanism; signatures need not be stable across versions | Deployment-defined (corpus/root can encode commit/version) |
| **Unison** | 512-bit SHA3 hash of normalized AST (Merkle) | Explicit patch = hash-to-hash replacement map; propagation is automated for type-preserving changes | N/A — append-only store, old hashes permanently resolvable |

The most sophisticated continuity model is Unison's: it makes lineage a first-class, queryable data structure (patch records) rather than an implicit convention. SCIP/LSIF/Kythe all rely on consumers to implement version-matching externally.

---

Sources:
- [SCIP proto grammar](https://github.com/sourcegraph/scip/blob/main/scip.proto)
- [SCIP announcement blog — LSIF failure modes](https://sourcegraph.com/blog/announcing-scip)
- [SCIP Go bindings — Symbol/Package/Descriptor types](https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip)
- [SCIP cross-repo design — scip-clang](https://github.com/sourcegraph/scip-clang/blob/main/docs/CrossRepo.md)
- [Sourcegraph cross-repository navigation](https://sourcegraph.com/blog/cross-repository-code-navigation)
- [Sourcegraph precise code navigation docs](https://sourcegraph.com/docs/code_intelligence/explanations/precise_code_navigation)
- [LSIF 0.4.0 specification](https://microsoft.github.io/language-server-protocol/specifications/lsif/0.4.0/specification/)
- [LSIF 0.5.0 specification](https://microsoft.github.io/language-server-protocol/specifications/lsif/0.5.0/specification/)
- [LSIF moniker export/import issue — C++ case](https://github.com/microsoft/lsif-node/issues/44)
- [Kythe URI specification](https://kythe.io/docs/kythe-uri-spec.html)
- [Kythe schema reference](https://kythe.io/docs/schema/)
- [Kythe writing an indexer](https://kythe.io/docs/schema/writing-an-indexer.html)
- [Kythe storage model](https://kythe.io/docs/kythe-storage.html)
- [Kythe RELEASES.md](https://github.com/kythe/kythe/blob/master/RELEASES.md)
- [Unison — The Big Idea](https://www.unison-lang.org/docs/the-big-idea/)
- [Unison FAQ](https://www.unison-lang.org/docs/faq/)
- [Unison update workflow docs](https://www.unison-lang.org/docs/usage-topics/workflow-how-tos/update-code/)
- [Unison reducing ecosystem churn (patch model)](https://www.unison-lang.org/blog/reducing-churn/)
- [SoftwareMill: Unison code-as-hashes deep dive](https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/)
- [rust-analyzer SCIP PR](https://github.com/rust-lang/rust-analyzer/pull/13456)
- [SCIP Java format — DeepWiki](https://deepwiki.com/sourcegraph/scip-java/5.1-scip-format)
- [SCIP indexer writing guide](https://sourcegraph.com/docs/code-search/code-navigation/writing_an_indexer)


# Nested agent agent-a145c0316bfa164a4
Description: Research rust-analyzer/salsa, Meta Glean, API diff tools
Size: 377741

Good initial results. Now let me fetch deeper content from the most important pages.


# Nested agent agent-ae5bdb20a67c6b35d
Description: Research GitHub stack graphs, git rename detection, content-addressed stores, vector embedding invalidation
Size: 355094

I now have comprehensive data across all four topics. Let me compile the dense findings report.

---

## Research Findings: All Four Topics

---

## TOPIC 1: GitHub Stack Graphs / tree-sitter Code Navigation

### What Stack Graphs Are

Stack graphs are a Rust framework for language-agnostic name resolution (go-to-definition, find-all-references) that operates incrementally per file without requiring build system invocation. The project lives at https://github.com/github/stack-graphs — **archived as read-only on September 9, 2025**.

The theoretical basis is scope graphs (from Eelco Visser's group at TU Delft). The paper is: *Stack Graphs: Name Resolution at Scale* — https://arxiv.org/abs/2211.01224.

### Symbol Identity Approach

Symbols in stack graphs are **simple string identifiers**. There is no hashing or content-addressing of symbols. The graph uses:

- **Push Symbol nodes** (green, solid): represent a reference — push a symbol name onto the symbol stack
- **Pop Symbol nodes** (red, thick-bordered): represent a definition — pop a matching symbol from the stack
- **Push/Pop Scoped Symbol**: same, but also manage a scope identifier
- **Scope nodes**: internal (local) or exported (cross-file connection points)
- **Root node**: global singleton linking all files
- **Jump to Scope / Drop Scopes**: non-linear scope traversal

Symbol identity is **exact string equality**: a pop node only fires if its symbol string exactly matches the top of the symbol stack. Special synthetic symbols (`.` for field access, `()` for function calls) encode language constructs.

**Fully qualified names** emerge from path traversal: if a definition is found in a file named `stove.py`, the framework knows the symbol is `stove.broil` — the module path is embedded in the graph structure via how files connect through the root node.

### Path-Finding Algorithm

Two stacks are maintained simultaneously during resolution:

1. **Symbol Stack**: tracks symbols being pushed/popped as traversal proceeds
2. **Scope Stack**: controls which scope context is active (enables type-directed lookup)

For type-directed lookups (e.g., resolving `A.foo` where `A` must be resolved first), the algorithm "pauses" the current lookup by pushing the pending symbol onto the stack, resolves `A`, then resumes.

**Partial paths** are precomputed per file at index time. Each partial path has:
- Symbol stack precondition (what must be on stack when entering)
- Symbol stack postcondition (what's on stack when leaving)
- Scope stack precondition/postcondition (possibly containing variables)

**Path stitching** (`ForwardPartialPathStitcher`): concatenates compatible partial paths from different files into complete paths. Compatibility requires: (1) the ending node of left path matches starting node of right, (2) symbol stack satisfies right's precondition, (3) scope stack satisfies right's scope precondition. A **bindings map** resolves scope list variables to concrete identifiers.

### Incremental / File-Level Design

Each source file is analyzed **completely in isolation** at index time. The file's partial paths are stored in **SQLite**. At query time, the system loads relevant partial paths and stitches them. No other files need to be known at index time.

Storage: `SQLiteWriter` writes partial paths per file; `SQLiteReader` retrieves them. The `Database::get_incoming_path_degree` method counts partial paths ending at a given node, supporting efficient query filtering.

### tree-sitter-stack-graphs Crate

Source: https://crates.io/crates/tree-sitter-stack-graphs — version 0.10.0 at archive time.

The workflow: source code → tree-sitter parse (CST) → TSG (tree-sitter-graph) DSL rules → stack graph nodes/edges → SQLite. Language-specific stanzas in `.tsg` files match syntax tree patterns and emit graph node/edge constructions.

Key TSG attributes: `type` (scope/push_symbol/pop_symbol/push_scoped_symbol/pop_scoped_symbol/drop_scopes), `source_node` (maps back to CST position), `is_definition`, `is_reference`, `is_exported`, `precedence` (controls shadowing on edges), `symbol` (the string literal).

Implementations exist for Python (`tree-sitter-stack-graphs-python`), TypeScript/JavaScript (`tree-sitter-stack-graphs-typescript`), Ruby (`tree-sitter-stack-graphs-ruby`), Java.

### Relationship to GitHub Blackbird Search

**Blackbird and stack graphs are separate systems serving different purposes:**

- **Blackbird** (blog post: https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/) is a Rust-based text/symbol *search index*. It tokenizes code into ngrams (content, symbol, path ngrams) and builds precomputed search indices. Symbol extraction is a pipeline step ("a service for extracting symbols from code"). Queries are parsed into an AST, then rewritten to look up ngram indices (`symbols_grams_iter`). Blackbird handles ~640 queries/second, indexes ~120,000 documents/second.

- **Stack graphs** power *precise code navigation* ("go to definition", "find all references") — semantic name resolution, not text search.

GitHub uses both: Blackbird for keyword/symbol search, stack graphs for precise navigation. They share the tree-sitter parsing layer but are architecturally separate.

---

## TOPIC 2: Git Rename Detection Internals

### Core Algorithm (diffcore-rename)

Full documentation: https://git-scm.com/docs/gitdiffcore

**Two-phase pipeline:**

1. **Exact OID match**: If a deleted file and an added file have identical blob SHA1/SHA256 — same content hash — they are immediately matched as a rename (score = 100). No content diffing needed.

2. **Inexact match via `estimate_similarity()`**: For remaining unpaired files, content is hashed into "chunks" and `diffcore_count_changes()` counts commonalities. The similarity score (0–100) is: `(unchanged_content / max(old_size, new_size)) * 100`. Default threshold: **50%** (`-M` flag).

**Greedy matching**: All candidate pairs are sorted by similarity score descending; highest-scoring pairs are matched first, removing both files from the pool. This is O(n²) in the worst case across all unpaired add/delete pairs.

**Optimization — preliminary exact-filename pass**: Before the quadratic comparison, git checks if a deleted file and an added file share the same basename (moved across directories). These are matched at a higher-than-default threshold in a single O(n) pass, excluding them from the later quadratic phase.

**Size filter**: Files are only compared if their sizes are within ~10% of each other, pruning the candidate matrix.

**`-M` flag format**: `-M8` = 80% threshold; `-M90%` = 90% threshold.

**`-C` (copy detection)**: Uses original contents of modified+deleted files as candidate sources. `--find-copies-harder` additionally considers unmodified files, which is significantly slower.

**diffcore-break (`-B`)**: Before rename detection, files that are mostly rewritten (below `break_rewrite_threshold`) are split into separate delete + add pairs, making them available for rename matching.

**`-L` (line-level log)**: Tracks a function or range across history; uses heuristics to follow the range through renames by applying rename detection between each adjacent commit pair.

### libgit2 `git_diff_find_similar` API

Full reference: https://libgit2.org/docs/reference/main/diff/git_diff_find_options.html

```c
typedef struct git_diff_find_options {
  unsigned int version;
  uint32_t flags;                      // GIT_DIFF_FIND_* combination
  uint16_t rename_threshold;           // default 50
  uint16_t rename_from_rewrite_threshold; // default 50 (source eligibility)
  uint16_t copy_threshold;             // default 50
  uint16_t break_rewrite_threshold;    // default 60
  size_t   rename_limit;               // default 1000 (max candidates)
  git_diff_similarity_metric *metric;  // NULL = default hash-sampling metric
};
```

Key `git_diff_find_t` flags include: `GIT_DIFF_FIND_RENAMES`, `GIT_DIFF_FIND_COPIES`, `GIT_DIFF_FIND_COPIES_FROM_UNMODIFIED`, `GIT_DIFF_FIND_REWRITES`, `GIT_DIFF_FIND_BREAK_REWRITES`, `GIT_DIFF_FIND_RENAMES_FROM_REWRITES`, `GIT_DIFF_FIND_EXACT_MATCH_ONLY`, `GIT_DIFF_FIND_IGNORE_WHITESPACE`, `GIT_DIFF_FIND_BY_CONFIG` (default — respects `diff.renames` config).

The pluggable `git_diff_similarity_metric` pointer allows injecting a custom similarity algorithm. `rename_limit` caps the number of candidates examined per file (libgit2's interpretation differs slightly from git's `-l` option).

### gitoxide (gix) Rename Detection

Crate: https://lib.rs/crates/gix-diff | Docs: https://docs.rs/gix-diff

**Key public API (requires `blob` feature flag):**

- `gix::diff::Rewrites` — struct configuring rename/copy tracking (thresholds, modes), consumed by `rewrites::Tracker`
- `gix::diff::rewrites::Tracker` — implements rename and copy detection
- `gix::diff::tree_with_rewrites()` — compares two trees with rename/copy detection; accepts a resource cache for blob similarity checks and a `Rewrites` configuration
- `gix::diff::tree()` — bare tree comparison without rewrite detection
- `gix::diff::index()` — compares two index (staging area) states
- `gix::diff::new_rewrites()` — initializes rewrite config from git config
- `gix::diff::resource_cache()` — prepares blob-diff cache for matrix comparisons

**Implementation notes from crate-status.md:**
- Exact content-address match detection: **complete**
- Similarity-based identification with configurable thresholds: **complete**
- Separate similarity factors for renames vs. copies: **complete**
- Basename-assisted matching (prevents file loss during moves): **complete**
- Directory-level tracking: **complete**
- `find-copies-harder` mode: **complete**
- **Deviation from git**: gix uses the first-found candidate meeting the similarity threshold; git keeps up to four candidates. This means gix may identify a different rename source and does not factor in filename similarity as a tiebreaker.
- Rename tracking in `gix blame`: **not yet supported** (noted as future work).

Uses `imara-diff` (v0.2 behind feature flag) for underlying line-level diffs.

---

## TOPIC 3: Content-Addressed Stores as Identity Substrate

### Nix Derivation Hashing

Full reference: https://nix.dev/manual/nix/2.28/store/derivation/outputs/content-address.html

Nix has two identity regimes:

**Input-addressed derivations** (default): The store path of an output is computed as a hash of the *derivation itself* (its inputs, build script, env vars). The output path is determined before building. If any input changes, the output path changes — even if the actual built artifact is identical.

**Content-addressed derivations** (two subtypes):

1. **Fixed-output derivations**: The *expected content hash* of the output is declared upfront in the derivation. The builder is allowed network access. After building, Nix computes the actual hash; if it mismatches the declared hash, the build fails. The store path is derived from the *declared content hash*, not from inputs. This means: same declared hash = same store path regardless of how/when it was built. Used by `fetchurl`, `fetchFromGitHub`, etc.

   Hash methods for fixed outputs: `flat` (raw file hash), `nar` (Nix Archive serialization hash), `text`, `git`. Hash algorithms: `md5`, `sha1`, `sha256`, `sha512`.

2. **Floating content-addressed derivations** (experimental): Builders are sandboxed (no network). The output hash is computed after building. The store path is content-addressed post-build. Requires deterministic builds (identical output across every invocation regardless of build path choices).

**Key identity invariant**: A fixed-output derivation with hash `H` always maps to store path `/nix/store/<H>-name`. Changing a URL but keeping the same content hash → same store path → no downstream rebuilds. This is the fundamental "same content = same node" invariant.

**Downstream propagation**: Since a fixed output's store path doesn't depend on inputs (only on output content), consumers can be cached even when input metadata changes. This is the "content-addressed = lineage-break" property that enables reproducible builds.

### Dolt Prolly Trees

Blog: https://www.dolthub.com/blog/2024-04-12-study-in-structural-sharing/ | Docs: https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree

**Data structure**: A probabilistic B-tree (Prolly Tree), invented by the Noms team, where every node is content-addressed.

**Chunk boundary algorithm (CDC/rolling hash)**:
1. Keys are sorted; a strong hash is computed on key data only (not values — this differs from the original Noms design)
2. When the hash value falls below a target probability threshold → start a new chunk boundary
3. The probability formula uses a CDF: `(CDF(end) - CDF(start)) / (1 - CDF(start))` — this normalizes chunk sizes around 4KB and avoids pathological size distributions
4. Result: chunk boundaries are determined by key content alone

**History independence (the critical property)**: Because boundaries are determined by key content, not by insertion order, the same key-value pairs always produce the same chunk structure regardless of how mutations happened. This makes the tree **history-independent**: same data = same chunks = same hashes, always.

**Identity invariant**: Same key-value data → identical chunk → identical content address. Different data → different hash. The content address of a tree root is computed by hashing internal nodes bottom-up, so root hash = identity of the entire table state.

**Structural sharing**: The content-addressed block store stores each chunk once; identical chunks across versions share storage. Comparing two versions = compare root hashes, recurse only into differing subtrees. Diff time is proportional to diff size, not table size.

**Version identity hierarchy**: row data (chunks) → table hash = `hash(schema_hash, data_root_hash)` → database root hash = `hash(all_table_hashes)` → commit hash = `hash(commit_metadata, database_root_hash)`.

**Limitation**: Insert patterns that scatter across the sorted key space (e.g., secondary index updates) force creation of near-duplicate chunks with minor differences, reducing structural sharing. Sequential appends at tree boundaries achieve near-optimal sharing.

### IPFS/IPLD Merkle-DAG Identity

Docs: https://docs.ipfs.tech/concepts/merkle-dag/ | https://docs.ipfs.tech/concepts/content-addressing/

**Content Identifier (CID)**: Every IPFS block is identified by a CID — a self-describing hash. CID encodes: version (v0 = legacy Base58/SHA256, v1 = explicit), codec (dag-pb for files, dag-cbor for structured data), and multihash (algorithm ID + digest).

**Identity invariant**: Any change to a node's payload changes its CID. Since parent nodes embed children's CIDs, a change to any leaf propagates upward, changing all ancestor CIDs. This creates verifiable lineage: the root CID is a cryptographic commitment to the entire DAG beneath it.

**Immutability + lineage**: A new version of a DAG that changes node N produces a new root CID but shares all unchanged sub-DAGs with the original via CID references. This is structural sharing: old and new versions point to common unchanged subtrees by CID. The two root CIDs encode a lineage relationship: "this new CID was derived from that old CID by changing this subtree."

**IPLD role**: Defines a universal data model for Merkle-DAG-based formats. Any IPLD-aware system can traverse CID links across different block formats. A Git commit is an IPLD-addressable object; a blockchain block is IPLD-addressable; they can reference each other via CIDs.

**Pattern for code symbols**: Applying this to symbol identity: a symbol definition node could be CID-addressed by hashing its (file_path, symbol_name, AST_span, content_hash). Renaming = new CID + a lineage edge from old CID to new CID in the graph. Unchanged symbol in a changed file = depends on whether the hash includes file content or just symbol content.

---

## TOPIC 4: Vector/Index Invalidation in Production Semantic Code Search

### Sourcegraph Cody

Blog: https://sourcegraph.com/blog/how-cody-understands-your-codebase | Docs: https://sourcegraph.com/docs/cody/core-concepts/embeddings

**Historical approach (deprecated 2024)**: Used OpenAI `text-embedding-ada-002` to embed source code. Incremental embeddings were supported: outdated embeddings of deleted/modified files were removed, new embeddings for modified/added files were inserted. The `incremental` config flag (defaulting to `true`) enabled this. Keying was by file path.

**Current approach (2024–2026)**: Sourcegraph **abandoned embeddings for primary context retrieval** in Cody Enterprise. The replacement is Sourcegraph's native search platform using an adapted BM25 ranking function + learned signals, with no third-party code transmission. The rationale: embeddings required sending code to OpenAI, were hard to scale to >100K repositories, and required admin configuration.

**Autocomplete** still uses tree-sitter parsing locally to identify intent (function body, docstring, etc.) and retrieves context from local open files/tabs.

**Embedding future**: Sourcegraph has stated embeddings remain "an active area of research" and may be re-introduced in some form.

### GitHub Copilot

Source: https://yasithrashan.medium.com/how-github-copilot-knows-your-code-inside-its-indexing-magic-aba59a0ce0e8 | https://markaicode.com/architecture/semantic-search-architecture-with-github-copilot/

**Embedding keying**: Embeddings are keyed by **URI + content version**. The system checks a SQLite cache for existing embeddings before requesting new ones.

**Chunk strategy**: Files are split into 100–250 token chunks. Each chunk gets a 512-dimensional embedding vector. Chunks are sent to a chunking endpoint which returns semantic chunks + embeddings, then stored in SQLite.

**Invalidation mechanism**: File system listeners detect file changes. A debounced `Delayer` triggers re-indexing of only changed files. Changed files' cache entries (keyed by URI + content version) are invalidated; new embeddings are fetched for the new content version.

**2025–2026 updates**: Copilot CLI now supports semantic indexing for natural-language repository queries. Pre-indexing, parallel context loading, and session-level caching reduced agent initialization time by ~50% for enterprise-scale codebases.

### Cursor

Blog: https://cursor.com/blog/secure-codebase-indexing | Deep analysis: https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast

**Embedding keying**: By **chunk content** (not file path or URI). Unchanged chunks hit the cache regardless of file path changes. This means moving a file without changing content reuses cached embeddings.

**Incremental mechanism**: Cursor builds a **Merkle tree** of SHA-256 hashes over all workspace files. Every 10 minutes, the client computes the current Merkle root and compares against the server's stored root. Hash mismatches at internal nodes identify which subtrees (directories) have changed; only those branches are walked to find changed files. Only changed files are re-chunked and re-uploaded.

**Chunk strategy**: AST-based chunking via tree-sitter (preferentially) or fallback to character/line splitting, respecting token limits.

**Storage**: Vector embeddings + metadata (line numbers, obfuscated file paths) stored in **Turbopuffer** (remote vector database optimized for millions of code chunks).

**Privacy**: No source code stored on server after request lifecycle. The Merkle tree also serves as a **proof-of-possession** mechanism: the client must prove it holds a file's content (by providing its hash) before receiving search results for that file. "The client can never see results for code it doesn't already have."

**Initialization**: On first open, client sends filenames + hashes (~3.2 MB for 50K files). Server identifies missing embeddings. Client sends changed/new file content. Server returns any entry the client is missing or has stale.

### continue.dev

Source: https://deepwiki.com/continuedev/continue/3.4-codebase-indexing

**No Merkle tree**: Uses direct per-file hashing + timestamps stored in SQLite `tag_catalog` table. Schema tracks `(directory, branch, artifact_id)` → `(file_path, last_updated, hash)`.

**Incremental logic**: `getComputeDeleteAddRemove()` compares live filesystem state against `tag_catalog` to classify files as added/deleted/modified. Only modified files are re-indexed.

**Embedding storage**: LanceDB with schema fields: `uuid`, `path`, `cachekey` (invalidation key), `vector`, metadata. The `cachekey` enables chunk-level invalidation.

**Chunk-level invalidation**: When a file changes, only chunks with changed `cachekey` values are re-embedded. Default batch size: 200 files per embedding provider request.

### Augment Code

Blog: https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable

**Real-time latency**: Index updates within a few seconds of file changes (branch switches, search-and-replace across hundreds of files, auto-formatting).

**Infrastructure**: Google Cloud PubSub for change event queuing, BigTable for storage, AI Hypercomputer GPUs for embedding generation. Multiple PubSub queues ensure real-time user updates are not blocked by bulk onboarding operations.

**Proof of possession security**: IDE must send a **cryptographic hash of file content** to the backend before retrieving content. This prevents unauthorized access to files the user shouldn't see — same principle as Cursor's Merkle proof.

**Index sharing**: Parts of search indices that overlap between users from the same tenant are shared in RAM (deduplication across users).

**Embedding model**: Custom model trained specifically for code context, not generic text embeddings.

### General Pattern (2025–2026 Industry Convergence)

The dominant production pattern is:
1. **Content hash as cache key** (not path, not mtime): Cursor uses chunk content hash; GitHub Copilot uses URI+content-version; continue.dev uses per-file hash + per-chunk cachekey. This ensures renames/moves that don't change content don't trigger re-embedding.
2. **Merkle tree or equivalent for efficient change detection**: Cursor uses explicit Merkle tree; continue.dev uses flat SQLite hash table (functionally equivalent for single-level file tracking).
3. **Chunk-granularity invalidation**: Only changed chunks within a modified file are re-embedded.
4. **AST-based chunking**: tree-sitter used broadly (Cursor, Cody autocomplete, stack-graphs) as the chunking/symbol-extraction layer.
5. **Sourcegraph's pivot away from embeddings** is the notable counterpoint — at massive scale (>100K repos), BM25+learned-ranking outperforms embedding-based retrieval in their evaluation.

---

## Key URLs

- Stack graphs repo (archived): https://github.com/github/stack-graphs
- Stack graphs docs: https://github.github.io/stack-graph-docs/
- Stack graphs paper (EVCS 2023): https://arxiv.org/abs/2211.01224
- tree-sitter-stack-graphs crate: https://crates.io/crates/tree-sitter-stack-graphs
- Introducing stack graphs blog: https://github.blog/open-source/introducing-stack-graphs/
- GitHub Blackbird search internals: https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/
- gitdiffcore documentation: https://git-scm.com/docs/gitdiffcore
- libgit2 git_diff_find_options: https://libgit2.org/docs/reference/main/diff/git_diff_find_options.html
- gix-diff crate: https://lib.rs/crates/gix-diff | https://docs.rs/gix-diff
- gix diff API (high-level): https://docs.rs/gix/latest/gix/diff/index.html
- gitoxide repo: https://github.com/gitoxidelabs/gitoxide
- Nix content-addressing: https://nix.dev/manual/nix/2.28/store/derivation/outputs/content-address.html
- Dolt prolly trees docs: https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree
- Dolt structural sharing blog: https://www.dolthub.com/blog/2024-04-12-study-in-structural-sharing/
- Dolt hashes blog: https://www.dolthub.com/blog/2024-11-11-dolt-hashes/
- IPFS Merkle-DAG docs: https://docs.ipfs.tech/concepts/merkle-dag/
- IPFS content addressing: https://docs.ipfs.tech/concepts/content-addressing/
- Sourcegraph Cody context: https://sourcegraph.com/blog/how-cody-understands-your-codebase
- Cursor secure indexing: https://cursor.com/blog/secure-codebase-indexing
- Cursor indexing deep-dive: https://read.engineerscodex.com/p/how-cursor-indexes-codebases-fast
- Augment Code real-time index: https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable
- continue.dev indexing: https://deepwiki.com/continuedev/continue/3.4-codebase-indexing
- GitHub Copilot indexing: https://yasithrashan.medium.com/how-github-copilot-knows-your-code-inside-its-indexing-magic-aba59a0ce0e8


