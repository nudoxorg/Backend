# Handoff: Research symbol identity industrial
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-ab9c801ec66f79949.jsonl`
- Agent id: `agent-ab9c801ec66f79949`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/06-symbol-identity-industrial.md`
- Existing report: False (0 lines)
- Tools used: 42 ({'Agent': 3, 'Bash': 1, 'ToolSearch': 1, 'WebSearch': 20, 'WebFetch': 17})
- Files/greps touched: 0
- Web queries: 37

## Web queries performed
- SCIP sourcegraph symbol descriptor grammar specification 2025
- FETCH: https://scip-code.org/
- FETCH: https://sourcegraph.com/blog/announcing-scip
- FETCH: https://github.com/sourcegraph/scip/blob/main/scip.proto
- Kythe VName signature corpus cross-version identity code navigation 2024 2025
- FETCH: https://kythe.io/docs/schema/
- Unison language hash identity patch update lineage "content addressed" definitions 2025
- FETCH: https://www.unison-lang.org/docs/the-big-idea/
- gitoxide gix rename detection git diff rust API 2025 2026
- FETCH: https://docs.rs/gix/latest/gix/diff/index.html
- cargo-semver-checks how matching works path based symbol identity version 2024 2025
- Meta Glean code intelligence incremental indexing schema facts cross-revision 2024 2025
- github stack-graphs symbol identity "precise code navigation" tree-sitter 2024 2025
- FETCH: https://glean.software/blog/incremental/
- Sourcegraph Cody embedding invalidation incremental indexing symbol hash 2025 2026
- FETCH: https://docs.sourcegraph.com/cody/core-concepts/embeddings
- Nix content addressed derivations fixed-output hashing identity 2024
- FETCH: https://sourcegraph.com/docs/cody/core-concepts/embeddings
- github "blackbird" code search symbol identity indexing architecture 2024
- FETCH: https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/
- IPLD content addressed merkle DAG identity lineage cross-version tracking 2024
- rust-analyzer interned IDs DefMap ItemTree cross-version stability salsa durability 2024
- Dolt prolly tree content addressed versioning structural sharing identity 2024 2025
- FETCH: https://rust-analyzer.github.io/book/contributing/architecture.html
- FETCH: https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/
- glean.software "symbol id" glass "stable" cross-revision identity format
- japicmp java api compatibility matching algorithm renamed moved class 2024
- typescript api-extractor "@microsoft/api-extractor" symbol identity tsdoc cross-version 2024 2025
- FETCH: https://kythe.io/docs/kythe-uri-spec.html
- go apidiff golang.org/x/exp/apidiff how symbol matching works api compatibility 2024
- FETCH: https://api-extractor.com/pages/contributing/architecture/
- FETCH: https://pkg.go.dev/golang.org/x/exp/apidiff
- git similarity index rename detection algorithm "content similarity" threshold internals 2024
- FETCH: https://git-scm.com/docs/gitdiffcore
- scip crate rust bindings SCIP protocol 2024 2025 crates.io
- FETCH: https://docs.rs/crate/scip/latest
- sourcegraph scip rust indexer protobuf bindings crate 2024

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll launch parallel research agents to cover all 10 topics simultaneously, then synthesize and write the full report.

### fragment

Three parallel research agents are running. I'll also check the existing codebase for relevant context while they work.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive INDUSTRIAL-SYSTEMS research for a frontier architecture decision. Today is 2026-07-16 — verify current state with WebSearch/WebFetch; do not trust training data alone. Cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform. Per-language compiler frontends (rust-analyzer, OXC, Pyrefly, go/types, javadoc doclet, Roslyn, snix) lower package source into a shared typed IR of symbols. Each published package version = a new IR "generation". The pipeline is becoming deeply incremental: only changed symbols get re-embedded/re-indexed/re-published, and a TerminusDB graph stores LINEAGE links between symbols across generations. The core question: "when are two symbols, across generations, the same symbol?" — we need the most ambitious defensible identity scheme. A parallel agent is surveying academic literature; you cover how real systems solve identity, naming, and cross-version tracking.

YOUR MISSION — exhaustive survey of industrial symbol-identity systems:
1. SCIP (Sourcegraph Code Intelligence Protocol): the exact symbol grammar (scheme/package/descriptor), how stable SCIP symbols are across versions, how Sourcegraph handles cross-commit navigation with stale indexes, moniker design lessons. Also LSIF's moniker system and why SCIP replaced it.
2. Kythe: VName design (signature/corpus/root/path/language), how Kythe intends cross-version entity continuity, lessons from its (semi-dormant?) state.
3. Unison language: content-addressed definitions (hash of AST = identity), how names are metadata over hashes, how Unison handles "same function, new body" (it's a NEW hash — identity via names/patches), what their patch/update model teaches about lineage.
4. rust-analyzer/salsa: how r-a keeps IDs stable across edits (durability, interned IDs, ItemTree), what of that transfers to cross-VERSION identity rather than intra-session.
5. Meta Glean: schema'd code facts, how they key entities, incremental indexing story, cross-revision handling.
6. Semantic-diff / API-diff tooling: cargo-semver-checks (how it matches items across versions — path-based?), japicmp, apidiff, go apidiff (golang.org/x/exp/apidiff), TypeScript API Extractor. What matching do they use and where does it fail (renames?).
7. GitHub: stack graphs / tree-sitter based code nav — symbol identity approach; also their "blackbird" search if relevant to identity.
8. Git-level tracking: git's rename detection internals (similarity index), git log -L / -M thresholds, libgit2; gitoxide (gix) APIs available in Rust for commit-level file diff + rename detection that nudox could reuse (nudox is Rust; gix is likely already a dependency).
9. Content-addressed stores as identity substrate: Nix derivation hashing, Dolt's structural sharing (prolly trees), Merkle-DAG identity (IPFS/IPLD) — patterns for "same content = same node; changed content = new node + lineage edge".
10. Vector/index invalidation practice: how production semantic-code-search systems (Sourcegraph Cody context, GitHub, Cursor?) key their embeddings (by file hash? symbol hash?) and invalidate incrementally — verify what's publicly documented as of 2026.

SYNTHESIS REQUIRED: end with a concrete recommendation for nudox's identity architecture: (a) a canonical SymbolMoniker grammar (SCIP-inspired, language-scheme-prefixed, descriptor chain) serving as tier-0 identity; (b) a content-hash layer (normalized-AST/signature hashes, Unison-inspired) for change DETECTION and dedup; (c) a lineage-edge layer for when monikers don't match (rename/move), fed by matching; (d) exactly which existing formats/crates to adopt or imitate (scip crate? kythe protos? gix for commit diffing?) with versions. Include a worked example: one symbol through rename + signature change + module move across three generations, showing moniker, hashes, and lineage edges at each step.

DELIVERABLE:
- Write the FULL report (dense markdown, 600-1200+ lines, URLs inline) to /Users/philocalyst/Projects/Backend/docs/research/librarification/06-symbol-identity-industrial.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) the recommended identity architecture as bullets, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### WebSearch: SCIP sourcegraph symbol descriptor grammar specification 2025
```
Web search results for query: "SCIP sourcegraph symbol descriptor grammar specification 2025"

Links: [{"title":"SCIP Format | sourcegraph/scip-java | DeepWiki","url":"https://deepwiki.com/sourcegraph/scip-java/5.1-scip-format"},{"title":"sourcegraph/scip | DeepWiki","url":"https://deepwiki.com/sourcegraph/scip"},{"title":"scip package - github.com/sourcegraph/scip/bindings/go/scip - Go Packages","url":"https://pkg.go.dev/github.com/sourcegraph/scip/bindings/go/scip"},{"title":"SCIP - a better code indexing format than LSIF | Sourcegraph","url":"https://sourcegraph.com/blog/announcing-scip"},{"title":"scip/Development.md at main · sourcegraph/scip","url":"https://github.com/sourcegraph/scip/blob/main/Development.md"},{"title":"SCIP Code Intelligence Protocol","url":"https://scip-code.org/"},{"title":"scip module - github.com/sourcegraph/scip - Go Packages","url":"https://pkg.go.dev/github.com/sourcegraph/scip"}]

Based on the search results, here's what I found about SCIP symbol descriptor grammar specification:

## Overview

SCIP (pronunciation: "skip") is a language-agnostic protocol for indexing source code, which can be used to power code navigation functionality such as Go to definition and Find references.

## Symbol Grammar Specification

SCIP uses a standardized string grammar to identify code entities (classes, methods, variables). The formal grammar specification is:

Symbols have a standardized string grammar defined in scip.proto147-193: `<symbol> ::= <scheme> ' ' <package> ' ' (<descriptor>)+ | 'local ' <local-id>` with `<package> ::= <manager> ' ' <package-name> ' ' <version>` and `<descriptor>::= <namespace> | <type> | <term> | <method> | <type-parameter> | <parameter> | <meta> | <macro>`

## Global Symbols Structure

Global symbols follow the structure: scheme (typically lsif-java or scip-java), manager (maven for Java/Kotlin/Scala dependencies), package name (the artifact coordinates), and descriptor (encodes the hierarchy).

## Descriptors

The list of descriptors for a symbol should together form a fully qualified name for the symbol. It should include one descriptor for every node in the AST (along the ancestry path) between the root of the file and the node corresponding to the symbol.

SCIP was created as a Protobuf schema that is centered around human-readable string IDs for symbols replacing the concept of 'monikers' and 'resultSet'.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlink
```

### WebFetch: https://scip-code.org/
```
# SCIP Symbol Grammar Specification - Not Available

Based on the web page content provided, the detailed symbol grammar specification is **not included** in this resource.

## What's Available

The page indicates that the standardized index format specification exists in the project repository:

> "Standardized index format" is listed as a key feature, with reference to the "scip.proto" file in the GitHub repository.

However, the actual BNF/EBNF grammar rules, descriptor types, symbol structure details, and version stability information are not present on this particular webpage.

## Where to Find It

To access the complete SCIP symbol grammar specification, you would need to:

1. **Review the protobuf definition**: Visit the `scip.proto` file directly in the [GitHub repository](https://github.com/scip-code/scip)
2. **Check the Documentation**: The "[Docs](./docs.html)" link likely contains technical specifications
3. **Engage the Community**: The Discord channel is available for technical questions about symbol structure and versioning

The webpage itself serves as an overview and ecosystem map rather than a technical reference for symbol grammar.
```

### WebFetch: https://sourcegraph.com/blog/announcing-scip
```
# SCIP's Replacement of LSIF: Key Rationale

## Why SCIP Replaced LSIF

Sourcegraph encountered several critical limitations with LSIF as usage scaled to over 45,000 repositories and 4,000+ daily uploads. The organization created SCIP to address fundamental architectural issues rather than incremental improvements.

## Problems with LSIF's Moniker Approach

The original format relied heavily on "opaque ID numbers to encode the graph structure." This created multiple cascading issues:

- **Development friction**: Lack of static types and machine-readable schemas made encoding difficult
- **Debugging challenges**: Numeric IDs obscured meaning, complicating manual inspection
- **Incremental indexing complexity**: "The heavy usage of opaque global IDs imposes an ordering constraint on how symbols...get added to the index"
- **Bookkeeping burden**: Developers had to maintain "unnecessary abstractions like import/export monikers that silently break navigation if you get it wrong"

## SCIP's Design Rationale

SCIP replaced opaque identifiers with "human-readable string IDs for symbols," drawing inspiration from SemanticDB. This Protobuf-based schema provided:

- Static typing for better developer experience
- Clearer symbol relationships without numeric abstraction layers
- Improved testability through snapshot utilities

## Symbol Stability and Cross-Version Considerations

The document does not address symbol stability or cross-version navigation specifics. It mentions anticipated future use cases like "cross-language navigation" but provides no technical details about how symbol definitions remain consistent across versions.
```

### WebFetch: https://github.com/sourcegraph/scip/blob/main/scip.proto
```
# SCIP Symbol Grammar Extraction

Based on the protobuf file, here's the complete symbol grammar:

## BNF Grammar

```
<symbol> ::= <scheme> ' ' <package> ' ' (<descriptor>)+ | 'local ' <local-id>
<package> ::= <manager> ' ' <package-name> ' ' <version>
<scheme> ::= any UTF-8, escape spaces with double space
<manager> ::= any UTF-8, escape spaces with double space. Use '.' for empty
<package-name> ::= same as manager
<version> ::= same as manager
<descriptor> ::= <namespace> | <type> | <term> | <method> | <type-parameter> 
               | <parameter> | <meta> | <macro>
<namespace> ::= <name> '/'
<type> ::= <name> '#'
<term> ::= <name> '.'
<meta> ::= <name> ':'
<macro> ::= <name> '!'
<method> ::= <name> '(' (<method-disambiguator>)? ').'
<type-parameter> ::= '[' <name> ']'
<parameter> ::= '(' <name> ')'
<name> ::= <identifier>
<method-disambiguator> ::= <simple-identifier>
<identifier> ::= <simple-identifier> | <escaped-identifier>
<simple-identifier> ::= (<identifier-character>)+
<identifier-character> ::= '_' | '+' | '-' | '$' | ASCII letter or digit
<escaped-identifier> ::= '`' (<escaped-character>)+ '`'
<local-id> ::= <simple-identifier>
```

## Descriptor Suffixes

The `Descriptor.Suffix` enum defines: Namespace, Type, Term, Method, TypeParameter, Parameter, Meta, Local, and Macro.

## Key Notes

- Scheme must be non-empty and cannot start with "local"
- Local symbols apply only to document-scoped entities
- Descriptors collectively form fully qualified names within packages
- Escaped identifiers require backticks and minimum one non-identifier character
```

### WebSearch: Kythe VName signature corpus cross-version identity code navigation 2024 2025
```
Web search results for query: "Kythe VName signature corpus cross-version identity code navigation 2024 2025"

Links: [{"title":"kythe.io@v0.0.68-0.20240422202219-7225dbc01741/kythe/web/site/examples.md - go module viewer","url":"https://go-mod-viewer.appspot.com/kythe.io@v0.0.68-0.20240422202219-7225dbc01741/kythe/web/site/examples.md"},{"title":"Kythe - Kythe Schema Reference","url":"https://kythe.io/docs/schema/"},{"title":"govname package - kythe.io/kythe/go/extractors/govname - Go Packages","url":"https://pkg.go.dev/kythe.io/kythe/go/extractors/govname"},{"title":"kythe/RELEASES.md at master · kythe/kythe","url":"https://github.com/kythe/kythe/blob/master/RELEASES.md"},{"title":"Kythe - Kythe URI Specification","url":"https://kythe.io/docs/kythe-uri-spec.html"},{"title":"Kythe - Writing a New Indexer","url":"https://kythe.io/docs/schema/writing-an-indexer.html"},{"title":"Study of Ravulizumab in Proliferative Lupus Nephritis (LN) or Immunoglobulin A Nephropathy (IgAN)","url":"https://cdn.clinicaltrials.gov/large-docs/39/NCT04564339/SAP_001.pdf"},{"title":"kythe/kythe/proto/storage.proto at master · kythe/kythe","url":"https://github.com/kythe/kythe/blob/master/kythe/proto/storage.proto"},{"title":"Google Kythe — Grokipedia","url":"https://grokipedia.com/page/google_kythe"},{"title":"Post-market Clinical Follow-up Study of NEMOST V2","url":"https://cdn.clinicaltrials.gov/large-docs/84/NCT07403084/Prot_SAP_ICF_000.pdf"}]

Based on the search results, I found information about Kythe's VName, signature, corpus, and code navigation capabilities:

## Key Findings:

**VName Components:**
The VName signature is a unique string (per corpus, root, path, and language) that should be consistently generated given the same input to the indexer, but does not necessarily need to be stable across different versions of the input.

**Corpus and Path:**
VName configuration requires creating a vnames.json mapping file, which tells the extractor how to assign vnames to files based on their paths.

**Code Navigation:**
In 2024, the Kythe project saw multiple pre-release updates on GitHub, including version 0.0.70 in May, which added support for a Rust extractor to enable indexing of Rust codebases. Additionally, Google's primary focus shifted to internal use and integrations like those in Android Studio's code navigation.

**Cross-Version Identity:**
Indexing the same compilation unit twice should always produce the same data. VNames for objects that are accessible from mul
```

### WebFetch: https://kythe.io/docs/schema/
```
# Kythe Schema: Stability and Entity Continuity

## VName Structure

The VName uniquely identifies nodes using five fields:

- **Language**: The source language
- **Corpus**: The node's containing corpus
- **Root**: A root path relative to the corpus
- **Path**: A path relative to both corpus and root
- **Signature**: "a unique string (per `corpus`, `root`, `path`, and `language`) that should be consistently generated given the same input to the indexer"

## Stability Across Versions

Kythe acknowledges that the signature field "does not necessarily need to be stable across different versions of the input." This allows flexibility in encoding while maintaining consistency within a single indexing run.

The schema permits implementations to "encode the `signature` field arbitrarily, as long as the chance of this encoding causing distinct field values to become indistinguishable is vanishingly small," enabling use of one-way hash functions for large values.

## Cross-Revision Entity Continuity

File nodes receive special treatment for linking across discrete indexer runs. The specification notes that these "nodes are frequently used as points for linking together the output of discrete indexer runs and may have greater stability properties than may be derived using the preceding VName rules."

## Node Kinds and Semantic Meaning

Kythe defines approximately 30 node kinds, including:
- **Semantic nodes**: record, function, variable, interface
- **Type nodes**: tapp, tbuiltin, tvar, talias, tnominal
- **Structural nodes**: anchor, file, package
- **Documentation**: doc, diagnostic

Each node kind represents distinct semantic roles within the code graph.
```

### WebSearch: Unison language hash identity patch update lineage "content addressed" definitions 2025
```
Web search results for query: "Unison language hash identity patch update lineage "content addressed" definitions 2025"

Links: [{"title":"docs Unison | 💡 The big idea · Unison programming language","url":"https://www.unison-lang.org/docs/the-big-idea/"},{"title":"‘Futuristic’ Unison functional language debuts | InfoWorld","url":"https://www.infoworld.com/article/4100673/futuristic-unison-functional-language-debuts.html"},{"title":"Unison 1.0: Content-Addressed Code Hits Production | byteiota","url":"https://byteiota.com/unison-1-0-content-addressed-code-hits-production/"},{"title":"Trying out Unison, part 1: code as hashes | SoftwareMill","url":"https://softwaremill.com/trying-out-unison-part-1-code-as-hashes/"},{"title":"GitHub - unisonweb/unison: A friendly programming language from the future · GitHub","url":"https://github.com/unisonweb/unison"},{"title":"Unison: A Content-Addressable Programming Language | Hacker News","url":"https://news.ycombinator.com/item?id=22156370"},{"title":"docs Unison | 🗺 A tour of Unison · Unison programming language","url":"https://www.unison-lang.org/docs/tour/"},{"title":"A look at Unison: a revolutionary programming language","url":"https://renato.athaydes.com/posts/unison-revolution.html"},{"title":"The Unison language – a new approach to Distributed programming | Hacker News","url":"https://news.ycombinator.com/item?id=33638045"}]

Based on the search results, here's what I found about Unison language's key concepts related to hash identity, patch updates, lineage, and content addressing:

## Content Addressing

Unison uses SHA3-512 cryptographic hashes of the code's syntax tree to identify code. These hashes depend only on the structure of the code, not on the actual names used.

## Hash Identity and Immutability

Because Unison definitions are identified by their hash, they never change. The definition associated with a hash never changes. Every function, type, and definition gets identified by a 512-bit hash computed from its structure plus all its dependencies' hashes.

## Patch and Update Model

Names become metadata pointing to immutable hash addresses in the codebase database. If you write the same function with different names, Unison will just store one function, but add an alias to it. It has a canonical representation for all code.

## Lineage and Version Management

The codebase is append-only: all changes to the codebase, including deletions, are appended to a log representing the codebase state. A Uniso
```

### WebFetch: https://www.unison-lang.org/docs/the-big-idea/
```
# Content-Addressed Code in Unison: A Complete Explanation

## Core Concept

Unison identifies each definition by "a hash of its syntax tree." Rather than mutable text files, code is content-addressed—each unique implementation gets a permanent, unchanging address in an immutable space.

## How Hashes Are Computed

The hashing process normalizes code before computing the SHA3-512 hash:

- All named arguments become positional variable references
- All dependencies are replaced by their own hashes
- Names are stripped away (they're stored as separate metadata)

For example, the function `increment n = n + 1` becomes something like `(#arg1 -> #a8s6df921a8 #arg1 1)` before hashing, where `#a8s6df921a8` represents the hash of the addition operation.

## Names and Hashes Relationship

"Names are just separately stored metadata that don't affect the function's hash." You can reassign a name to point to a different hash, but the definition at any given hash address never changes. This creates a pointer-like system: names are references to immutable content.

## Update Propagation Model

The document doesn't explicitly detail caller updates. However, it explains that because definitions are immutable, "we can parse and typecheck definitions once, and then store the results in a cache which is never invalidated." When you change a name's target hash, dependent code must be handled through Unison's "structured refactoring sessions" rather than breaking cascades of errors.

## Structured Refactoring Approach

Rather than mutating files and causing breakage, refactoring uses a "magic cloning tool" where the old codebase remains functional while constructing changes incrementally, keeping "the codebase never in a broken state."
```

### WebSearch: gitoxide gix rename detection git diff rust API 2025 2026
```
Web search results for query: "gitoxide gix rename detection git diff rust API 2025 2026"

Links: [{"title":"GitHub - GitoxideLabs/gitoxide: An idiomatic, lean, fast & safe pure Rust implementation of Git · GitHub","url":"https://github.com/GitoxideLabs/gitoxide"},{"title":"gix-diff — Rust dev tool // Lib.rs","url":"https://lib.rs/crates/gix-diff"},{"title":"gix::diff - Rust","url":"https://docs.rs/gix/latest/gix/diff/index.html"},{"title":"PickYourTech - Gitoxide: Open Source Alternative to AWS CodeCommit and more","url":"https://www.pickyourtech.com/projects/gh-gitoxidelabs-gitoxide"},{"title":"gitoxide_core - Rust","url":"https://docs.rs/gitoxide-core/latest/gitoxide_core/index.html"},{"title":"gix - Rust","url":"https://docs.rs/gix"},{"title":"git-diff 0.26.2 [deprecated] // Lib.rs","url":"https://lib.rs/crates/git-diff"},{"title":"Crate gix - Rust","url":"https://docs.rs/gix/latest/gix/"}]

Based on the web search results, here's what I found about gitoxide's rename detection and diff capabilities:

## Gitoxide Overview

Gitoxide is an implementation of git written in Rust for developing future-proof applications which strive for correctness and performance while providing a pleasant and unsurprising developer experience.

## Rename Detection and Diff API

The gix::diff module contains a Tracker to detect rewrites, with general diff-related options for configuring rename-tracking and blob diffs, and a structure to capture how to perform rename and copy tracking, used by the rewrites::Tracker.

Rename-tracking relies on line-by-line diffs in some cases when handling tree-diffs. Options can be used to further configure how the rename tracking is performed.

## Rust API Access

The gix crate can be used as a Cargo dependency for API access. Developers can develop diff analysis and tree comparison tools using gix-diff and gix-object for accurate change detection.

The search results indicate that gitoxide's diff functionality, including rename detection, is part of its core library offerings through the `gix` and `gix-diff` crates, though the documentation doesn't show specific implementation details for 2025-2026 developments.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebFetch: https://docs.rs/gix/latest/gix/diff/index.html
```
# Gix Diff APIs: Rename Detection and Rewrite Tracking

Based on the documentation, here are the key APIs available in the `gix::diff` module:

## Core Diff Functions

The module provides several primary diff operations:

- **`tree()`** - "Calculate the changes that would need to be applied to `lhs` to get `rhs`" using object lookups and reusable state
- **`index()`** - Performs entry-by-entry comparison between two indices, supporting pathspec filtering
- **`tree_with_rewrites()`** - Combines tree diffing with rename/copy detection through repeated callback invocations

## Rename and Rewrite Detection

The rename tracking capabilities center on:

- **`Rewrites` struct** - Configures how "rename and copy tracking" operations function during comparisons
- **`Tracker` struct** (in `rewrites` module) - The primary mechanism for "detecting rewrites"
- **`new_rewrites()` function** - Initializes rewrite detection settings from configuration with optional strictness control

## Blob-Level Diffing

For symbol and content comparison:

- **`resource_cache()` function** - Provides "a low-level utility to efficiently prepare a blob-level diff operation" and supports matrix-style comparisons
- **`blob` module** - References `imara-diff` for text-level analysis

## Supporting Infrastructure

- **`Options` struct** - Centralizes "diff-related options for configuring rename-tracking and blob diffs"
- **`tree_with_rewrites` module** - Marked with `blob` feature flag for advanced rewrite scenarios

This architecture separates concerns between structural (tree) diffing and content-based similarity detection for comprehensive change analysis.
```

### WebSearch: cargo-semver-checks how matching works path based symbol identity version 2024 2025
```
Web search results for query: "cargo-semver-checks how matching works path based symbol identity version 2024 2025"

Links: [{"title":"Begin resolving `cargo-semver-checks` blockers for merging into cargo - Rust Project Goals","url":"https://rust-lang.github.io/rust-project-goals/2024h2/cargo-semver-checks.html"},{"title":"SemVer in Rust: Tooling, Breakage, and Edge Cases — FOSDEM 2024","url":"https://predr.ag/blog/semver-in-rust-tooling-breakage-and-edge-cases/"},{"title":"cargo-semver-checks 0.47.0 - Docs.rs","url":"https://docs.rs/crate/cargo-semver-checks/latest"},{"title":"Releases · obi1kenobi/cargo-semver-checks","url":"https://github.com/obi1kenobi/cargo-semver-checks/releases"},{"title":"GitHub - obi1kenobi/cargo-semver-checks: Scan your Rust crate for semver violations. · GitHub","url":"https://github.com/obi1kenobi/cargo-semver-checks"},{"title":"cargo-semver-checks - crates.io: Rust Package Registry","url":"https://crates.io/crates/cargo-semver-checks"},{"title":"GitHub - obi1kenobi/cargo-semver-checks-action: A GitHub Action for running cargo-semver-checks · GitHub","url":"https://github.com/obi1kenobi/cargo-semver-checks-action"},{"title":"cargo-semver-checks - GitHub Marketplace","url":"https://github.com/marketplace/actions/cargo-semver-checks"}]

Based on the search results, I found relevant information about how cargo-semver-checks performs matching:

## How cargo-semver-checks Matching Works

In the new version, cargo-semver-checks attempts to match each function to a corresponding public function at the same public API import path. If no matching function is found in the new version of the crate, then a breaking change has been identified: a function that previously could be imported and used can no longer be imported and used.

## Path-Based Symbol Identity

The matching process uses **path-based symbol identity** as the mechanism for tracking symbols across versions. The process looks at public functions in the old version of the crate and ensures that some public API path could be used to import them. By comparing the public API import paths between versions, cargo-semver-checks can identify when symbols have been removed or made inaccessible.

## Recent Development (2024-2025)

cargo-semver-checks is a linter for semantic versioning (SemVer) in Rust that is broadly adopted by the Rust community, and the cargo team has expressed interest in merging it into cargo itself. As of 2024, work was ongoing to resolve blockers for integration
```

### WebSearch: Meta Glean code intelligence incremental indexing schema facts cross-revision 2024 2025
```
Web search results for query: "Meta Glean code intelligence incremental indexing schema facts cross-revision 2024 2025"

Links: [{"title":"Indexing code at scale with Glean - Engineering at Meta","url":"https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/"},{"title":"Repository Intelligence Graph: Deterministic Architectural Map for LLM Code Assistants","url":"https://arxiv.org/pdf/2601.10112"},{"title":"Indexing code at scale with Glean | daily.dev","url":"https://app.daily.dev/posts/indexing-code-at-scale-with-glean-z5ie7p34h"},{"title":"Indexing Hackage: Glean vs. hiedb · Simon Marlow","url":"https://simonmar.github.io/posts/2025-05-22-Glean-Haskell.html"},{"title":"Blog | Glean","url":"https://glean.software/blog/"},{"title":"Incremental indexing with Glean | Glean","url":"https://glean.software/blog/incremental/"},{"title":"TypeScript Repository Indexing for Code Agent Retrieval","url":"https://arxiv.org/pdf/2604.18413"},{"title":"Glean","url":"https://glean.software/"}]

Based on the search results, here's what I found about Meta's Glean code intelligence system with focus on incremental indexing and related features:

## Incremental Indexing

An important innovation in Glean is the ability to index incrementally. As the codebase grows, and the rate of change of the codebase increases (a monorepo suffers from both of these problems) we find that we can't provide up-to-date information about the latest code because indexing the entire repository can take a long time.

## Technical Approach

Glean solves the first problem with an ingenious method of stacking immutable databases on top of each other. A stack of databases behaves just like a single database from the client's perspective, but each layer in the stack can non-destructively add information to, or hide information from, the layers below.

Glean updates a monolithic index with the changes between the base revision and the current revision, which should hopefully be faster than a full repository indexing job because we only have to look at the things that have changed. The goal is to be able to index the changes in O(changes) rather than O(repository), or as close to that as we can get.

## Schema & Facts

Glean features compact, incremental storage designed to index monorepos with billions of facts. Meta's open-source system Glean collects, derives, and works with data about source code, offering efficient and flexible code indexing.


REMINDER: You MUST inclu
```

### WebSearch: github stack-graphs symbol identity "precise code navigation" tree-sitter 2024 2025
```
Web search results for query: "github stack-graphs symbol identity "precise code navigation" tree-sitter 2024 2025"

Links: [{"title":"Introducing stack graphs - The GitHub Blog","url":"https://github.blog/open-source/introducing-stack-graphs/"},{"title":"Explore using stack graphs for better code search / navigation / context / repo map / etc · Issue #742 · OpenHands/OpenHands","url":"https://github.com/OpenHands/OpenHands/issues/742"},{"title":"Explore whether stack graphs may be useful in this tool · Issue #534 · Aider-AI/aider","url":"https://github.com/paul-gauthier/aider/issues/534"},{"title":"Stack graphs Name resolution at scale Douglas A. Creager ! Ï","url":"https://arxiv.org/pdf/2211.01224"},{"title":"Feature Request: Support stack-graphs to provide code nav in the absence of LSP, building on tree-sitter · Issue #1252 · helix-editor/helix","url":"https://github.com/helix-editor/helix/issues/1252"},{"title":"Explore using stack graphs for better code search / navigation / context / repo map / etc · Issue #742 · All-Hands-AI/OpenHands","url":"https://github.com/All-Hands-AI/OpenHands/issues/742"},{"title":"Explore whether 'smart code search' (AST, stack graphs, scope graphs) would improve performance beyond the current find/grep based approach. · Issue #38 · SWE-agent/SWE-agent","url":"https://github.com/SWE-agent/SWE-agent/issues/38"},{"title":"Some notes and tools on fingerprinting minified JavaScript libraries, AST fingerprinting, source code similarity, etc · GitHub","url":"https://gist.github.com/0xdevalias/31c6574891db3e36f15069b859065267"},{"title":"Tools built on tree-sitter's concrete syntax trees | Lobsters","url":"https://lobste.rs/s/1qmcac/tools_built_on_tree_sitter_s_concrete"},{"title":"codegraph · GitHub Topics","url":"https://github.com/topics/codegraph"}]

Based on the web search results, here's what I found about GitHub's stack-graphs, symbol identity, precise code navigation, and tree-sitter:

## Overview

Precise code navigation is powered by stack graphs, a new open source framework that lets you define the name binding rules for a programming language using a declarative, domain-specific language (DSL). GitHub turned to Tree-sitter, an open source parsing framework, as a natural choice to build stack graphs on.

## Key Capabilities

With stack graphs, code navigation data can be generated for a repository without requiring any configuration from the repository owner, and without tapping into a build process or other CI job. St
```

### WebFetch: https://glean.software/blog/incremental/
```
# Glean's Incremental Indexing Technical Overview

## Core Architecture

Glean implements incremental indexing by stacking databases rather than recreating entire indices. When changes occur, a new DB layer sits atop the original, with selective visibility rules determining which data appears in the composite view.

## The Hide Mechanism

The system relies on **units**—arbitrary string labels (typically filenames or module names) assigned to facts during indexing. To create an incremental update:

> "we specify which units to exclude from the base DB, like this: glean create --repo <new> --incremental <old> --exclude A,B,C"

This command creates a new DB that obscures specified units from the base version while stacking fresh data on top.

## Ownership Tracking

To maintain database validity (preventing dangling references), Glean tracks which facts belong to which units through an **ownership set** system. Facts that depend on hidden units must preserve visibility of their dependencies. The implementation approach includes:

- Unique ID assignment to distinct ownership sets
- Storage using Elias Fano Coding
- Interval mapping for fact-to-ownership associations
- Approximately 7% database size overhead

## Derived Facts

Derived facts inherit composite ownership: "When is a derived fact visible? When all of the facts that it was derived from are visible." Ownership expressions use logical operations (intersection, union) and aren't normalized to disjunctive form currently.

## Multi-Level Stacks

Deeper stacks create tree structures where intermediate database nodes remain queryable simultaneously. The system handles cross-boundary fact dependencies by transitively propagating ownership data upward through the stack.

## Performance Impact

- Indexing overhead: 2-3% (Python)
- Query overhead: <10% for typical queries; ~3x for search-heavy operations
- Incremental derivation optimized for subset operations
```

### WebSearch: Sourcegraph Cody embedding invalidation incremental indexing symbol hash 2025 2026
```
Web search results for query: "Sourcegraph Cody embedding invalidation incremental indexing symbol hash 2025 2026"

Links: [{"title":"Embeddings - Sourcegraph docs","url":"https://docs.sourcegraph.com/cody/core-concepts/embeddings"},{"title":"Cody AI 2026 Guide: Context-Aware Coding & Multi-LLM Support &middot; AI Tools DevPro","url":"https://aitoolsdevpro.com/ai-tools/sourcegraph-cody-guide/"},{"title":"sourcegraph/doc/cody/explanations/code_graph_context.md at main · courier-new/sourcegraph","url":"https://github.com/courier-new/sourcegraph/blob/main/doc/cody/explanations/code_graph_context.md"},{"title":"Generate an Embeddings Index - Sourcegraph docs","url":"https://docs.sourcegraph.com/cody/core-concepts/embeddings/embedding-index"},{"title":"Generate an Embeddings Index to Enable Codebase-Aware Answers - Sourcegraph docs","url":"https://docs.sourcegraph.com/cody/explanations/indexing"},{"title":"Cody - Sourcegraph docs","url":"https://sourcegraph.com/docs/cody"},{"title":"Technical Changelog - Sourcegraph docs","url":"https://sourcegraph.com/docs/technical-changelog"},{"title":"Sourcegraph Cody vs Qodo (2026): Code Search vs Review Gates | Augment Code","url":"https://www.augmentcode.com/tools/sourcegraph-cody-vs-qodo"},{"title":"How Cody understands your codebase | Sourcegraph","url":"https://sourcegraph.com/blog/how-cody-understands-your-codebase"},{"title":"Cody indexing workspace how to","url":"https://community.sourcegraph.com/t/cody-indexing-workspace-how-to/546"}]

Based on the search results, here's what I found about Sourcegraph Cody embedding invalidation, incremental indexing, and related concepts:

## Incremental Indexing and Embeddings

Incremental embeddings allow you to update the embeddings for a repository without re-embedding the entire repository. With incremental embeddings, outdated embeddings of deleted and modified files are removed, and new embeddings of modified and added files are added to the repository's embeddings. This speeds up updates, reduces the data sent to the embedding provider and saves costs.

## Embedding Configuration

Embeddings can be disabled, even with Cody enabled, by using the site configuration with an "incremental": false setting. If you've configured a repository for automated embeddings, the repository will be scheduled for embedding with every new commit. By default, there is a 24-hour time interval that must pass between two embeddings.

## Context Retrieval Approach

Embeddings have been at the ba
```

### WebFetch: https://docs.sourcegraph.com/cody/core-concepts/embeddings
```
REDIRECT DETECTED: The URL redirects to a different host.

Original URL: https://docs.sourcegraph.com/cody/core-concepts/embeddings
Redirect URL: https://sourcegraph.com/docs/cody/core-concepts/embeddings
Status: 302 Found

To complete your request, I need to fetch content from the redirected URL. Please use WebFetch again with these parameters:
- url: "https://sourcegraph.com/docs/cody/core-concepts/embeddings"
- prompt: "Extract details on: how Sourcegraph keys embeddings (by file path? content hash? symbol ID?), how incremental embedding invalidation works, what triggers re-embedding, and any details about per-symbol vs per-file granularity."
```

### WebSearch: Nix content addressed derivations fixed-output hashing identity 2024
```
Web search results for query: "Nix content addressed derivations fixed-output hashing identity 2024"

Links: [{"title":"Content-addressing derivation outputs - Nix 2.28.6 Reference Manual","url":"https://nix.dev/manual/nix/2.28/store/derivation/outputs/content-address.html"},{"title":"Content-addressing derivation outputs - Determinate Nix 3.19.0 Reference Manual","url":"https://manual.determinate.systems/store/derivation/outputs/content-address.html"},{"title":"Advanced Attributes - Nix Reference Manual","url":"https://nix.dev/manual/nix/2.24/language/advanced-attributes.html?highlight=outputhash"},{"title":"Nix Hash - Official NixOS Wiki","url":"https://wiki.nixos.org/wiki/Nix_Hash"},{"title":"Nix Hash - NixOS Wiki","url":"https://nixos.wiki/wiki/Nix_Hash"},{"title":"Nix: what are fixed-output derivations and why use them? | Brian McGee","url":"https://bmcgee.ie/posts/2023/02/nix-what-are-fixed-output-derivations-and-why-use-them/"},{"title":"Derivation - Nix 2.34.8 Reference Manual","url":"https://nix.dev/manual/nix/2.34/protocols/json/derivation/index.html"},{"title":"Advanced Attributes - Nix Reference Manual","url":"https://nixos.org/manual/nix/stable/language/advanced-attributes.html?highlight=outputHash"},{"title":"Glossary - Nix 2.28.7 Reference Manual","url":"https://nix.dev/manual/nix/2.28/glossary.html?highlight=fixed-output+derivation"}]

Based on the search results, here's what I found about Nix content-addressed derivations, fixed-output hashing, and identity:

## Fixed-Output Derivations

Fixed-output derivations are derivations where you need to know and specify the hash of the output in advance. While normal derivations are not allowed network access, fixed-output derivations are a compromise that are provided with network access but must declare in advance a hash of their contents.

## Content Addressing

For outputs with fixed content addresses, the output spec contains a hash, the hash expected from digesting the store object's file system objects. If the ca-derivations experimental feature is enabled, derivation outputs can be stored in a content-addressed location rather than the traditional input-addressed one.

## Purpose and Security

The purpose of fixing an output's content address is carefully controlled impurity; while fixed-output derivation builders have access to the network, the guaranteed content-addresses are supposed to mitigate risk by ensuring that all information passed downstream flows through outputs whose content-
```

### WebFetch: https://sourcegraph.com/docs/cody/core-concepts/embeddings
```
# Sourcegraph Cody Embeddings Documentation

Based on the provided documentation page, I cannot find the specific technical details you're requesting.

The webpage does not contain information about:
- How Sourcegraph keys embeddings (file hash, content hash, symbol-level)
- Incremental invalidation mechanisms
- Granularity specifications (file vs. chunk vs. symbol)
- Stale embedding detection and replacement processes

The documentation describes Cody as using "the powerful Sourcegraph's advanced Search API to pull context from both local and remote codebases," but does not elaborate on the underlying embedding infrastructure, indexing strategies, or cache invalidation patterns.

**Recommendation**: To obtain this architectural information, you would need to consult:
- Sourcegraph's technical architecture documentation
- Developer or engineering documentation not included in this page
- Direct communication with Sourcegraph's engineering team
```

### WebSearch: github "blackbird" code search symbol identity indexing architecture 2024
```
Web search results for query: "github "blackbird" code search symbol identity indexing architecture 2024"

Links: [{"title":"The technology behind GitHub’s new code search - The GitHub Blog","url":"https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/"},{"title":"GitHub releases Blackbird code search engine","url":"https://www.developer-tech.com/news/2023/may/10/github-releases-blackbird-code-search-engine/"},{"title":"Project Blackbird - Github's New Search Engine - MarkTechPost","url":"https://www.marktechpost.com/2023/05/12/project-blackbird-githubs-new-search-engine/"},{"title":"[Research] Codebase indexing & Semantic search · GitHub","url":"https://gist.github.com/tuandinh0801/056bebc4912f5789804119e9e9247cd5"},{"title":"System Design Case Study #2: How GitHub Indexes Code For Blazing Fast Search & Retrieval - Scaleyourapp","url":"https://scaleyourapp.com/system-design-github-code-search-engine/"},{"title":"GitHub replaces code search engine with Rust-based Blackbird","url":"https://www.theregister.com/software/2023/05/09/github-replaces-code-search-engine-with-rust-based-blackbird/1508454"},{"title":"System Design: GitHub (200M Repos, Git Object Storage, Sparse Trigram Code Search, Per-Job VM CI) | CrackingWalnuts","url":"https://crackingwalnuts.com/post/github-system-design"},{"title":"GitHub claims source code search engine is a game changer • The Register","url":"https://www.theregister.com/2023/02/07/github_code_search/"},{"title":"GitHub switches to Rust based Blackbird code search | Hacker News","url":"https://news.ycombinator.com/item?id=35873845"},{"title":"GitHub - p1ngul1n0/blackbird: An OSINT tool to search for accounts by username and email in social networks. · GitHub","url":"https://github.com/p1ngul1n0/blackbird"}]

Based on the search results, here's what I found about GitHub's Blackbird code search engine, its architecture, and symbol indexing:

## Overview

GitHub built its own search engine from scratch in Rust, specifically for the domain of code search, called Blackbird. This revision has been in development for three years and was developed in 2020, two years after Microsoft acquired the company, to enhance GitHub's existing Elasticsearch cluster without expanding its resource demands.

## Architecture Overview

The front end accepts the user query and passes it along to the Blackbird query service where it parses the query into an abstract syntax tree and then rewrites it, reso
```

### WebFetch: https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/
```
# GitHub Blackbird Symbol Indexing: Limited Technical Details

Based on the provided article, specific technical details about Blackbird's symbol indexing approach are notably sparse. Here's what the content reveals:

## Symbol Extraction Service

The article mentions "a service for extracting symbols from code" as part of the ingest pipeline, but provides no implementation details about how this service operates, what constitutes a symbol identity key, or how symbols are deduplicated.

## Index Structure

The system creates multiple index types: "ngram indices (for content, symbols, and paths) and other useful indices (languages, owners, repositories, etc)." Symbols appear indexed similarly to content and paths, but the article doesn't explain the underlying symbol identity mechanism.

## Sharding Strategy

Sharding occurs by Git blob SHA, not by symbol. This means identical content appearing across multiple repositories (a common scenario) gets stored once but indexed as a single blob object. The article states this approach "avoid[s] any duplication," though it's unclear if this extends to symbol-level deduplication.

## Cross-Commit/Version Handling

The document does not address how Blackbird maintains symbol identity across commits or versions. It mentions "query results are consistent on a commit-level basis" but doesn't explain symbol tracking mechanisms across repository history.

**Conclusion:** The article focuses on overall architecture rather than symbol-specific implementation details, leaving these technical mechanisms largely undisclosed.
```

### WebSearch: IPLD content addressed merkle DAG identity lineage cross-version tracking 2024
```
Web search results for query: "IPLD content addressed merkle DAG identity lineage cross-version tracking 2024"

Links: [{"title":"Merkle DAGs(IPFS, GIT) - Overview | AccessDenied","url":"https://rya-sge.github.io/access-denied/2025/04/09/merkle-dag/"},{"title":"ipfs-specs/MERKLE_DAG.md at main · GridSAT/ipfs-specs","url":"https://github.com/GridSAT/ipfs-specs/blob/main/MERKLE_DAG.md"},{"title":"specs/MERKLE_DAG.md at main · ipfs/specs","url":"https://github.com/ipfs/specs/blob/main/MERKLE_DAG.md"},{"title":"Merkle Directed Acyclic Graphs (DAG) | IPFS Docs","url":"https://docs.ipfs.tech/concepts/merkle-dag/"},{"title":"IPLD","url":"https://ipfs.io/ipfs/QmQmAh8R6xvu7cXrTXaxs28dqhJC6viDD6sBAFtt9aEDCP/_site/"},{"title":"IPFS - Content Addressed, Versioned, P2P File System","url":"https://arxiv.org/pdf/1407.3561"},{"title":"IPLD Tutorial | Merkle DAGs: Structuring Data for the Distributed Web (Lesson 8) | ProtoSchool","url":"https://proto.school/merkle-dags/08/"},{"title":"Adaptive Edge Content Delivery Networks for Web-Scale File Systems","url":"https://arxiv.org/pdf/2207.06341"},{"title":"Referencing Source Code Artifacts: a Separate Concern in Software   Citation","url":"https://arxiv.org/pdf/2001.08647"},{"title":"What's inside a node? Malicious IPFS nodes under the magnifying glass","url":"https://arxiv.org/pdf/2306.05541"}]

Based on the search results, here's what I found about IPLD, content-addressed merkle DAGs, and related concepts:

## IPLD and Merkle DAGs

InterPlanetary Linked Data (IPLD) is developing an ecosystem of Merkle-DAG-based data formats and their formal descriptions, supporting wide-ranging data interchange. The original IPFS Merkle DAG specification has been deprecated in favor of IPLD, which offers a clearer description of how to link different kinds of hash-based structures (e.g. linking a file in IPFS to a commit in Git), has a more generalized and flexible format, and uses a JSON-compatible representation, among other improvements.

## Content Addressing and Identity

Identifying a data object (like a Merkle DAG node) by the value of its hash is referred to as content addressing, and the node identifier is named Content Identifier, or CID. A Merkle DAG is a DAG where each node has an identifier, and this is the result of hashing the node's contents — any opaque payload carried by the node and the list of identifiers of its children — using a cryptographic hash function like SHA256.

## Immutability and Version Tracking

Merkle DAG n
```

### WebSearch: rust-analyzer interned IDs DefMap ItemTree cross-version stability salsa durability 2024
```
Web search results for query: "rust-analyzer interned IDs DefMap ItemTree cross-version stability salsa durability 2024"

Links: [{"title":"Port rust-analyzer to salsa 3.0 - HackMD","url":"https://hackmd.io/@salsa/B19OUlA71l"},{"title":"Architecture - rust-analyzer","url":"https://rust-analyzer.github.io/book/contributing/architecture.html"},{"title":"Guide - rust-analyzer","url":"https://rust-analyzer.github.io/book/contributing/guide.html"},{"title":"hir_def::item_tree - Rust","url":"https://rust-lang.github.io/rust-analyzer/hir_def/item_tree/index.html"},{"title":"RustMizan: A Compilable, Contamination-Aware Benchmarking Framework for Rust Vulnerabilities","url":"https://arxiv.org/pdf/2607.04729"},{"title":"Evaluating and Improving Automated Repository-Level Rust Issue Resolution with LLM-based Agents","url":"https://arxiv.org/pdf/2602.22764"},{"title":"Fearless Unsafe. A More User-friendly Document for Unsafe Rust   Programming Base on Refined Safety Properties","url":"https://arxiv.org/pdf/2412.06251"},{"title":"Diff - 216bef36bccb67bcc4c0a4d7940f6dc84c4fac7b^2..216bef36bccb67bcc4c0a4d7940f6dc84c4fac7b - external/github.com/rust-lang/rust - Git at Google","url":"https://chromium.googlesource.com/external/github.com/rust-lang/rust/+/216bef36bccb67bcc4c0a4d7940f6dc84c4fac7b%5E2..216bef36bccb67bcc4c0a4d7940f6dc84c4fac7b/"},{"title":"internal: port rust-analyzer to new Salsa · 74620e64ec - language-servers/rust-analyzer - Forgejo: Beyond coding. We Forge.","url":"https://git.joshthomas.dev/language-servers/rust-analyzer/commit/74620e64ec24861a821ebb9e519461042905668f"}]

Based on the search results, here's what I found about rust-analyzer's architecture related to your query:

## Key Architectural Components

The hir-xxx crates have a strong ECS flavor, working with raw ids and directly querying the database with little abstraction, and integrate deeply with salsa and chalk.

### ItemTree and DefMap

ItemTrees are built per HirFileId from the syntax tree of the parsed file, making them crate-independent and unaware of which #[cfg]s are active or which module they belong to. A key purpose of ItemTree is to provide an "invalidation barrier" for incremental computations: when typing inside an item body, the ItemTree of the modified file is typically unaffected, so name resolution results or item data don't need recomputation.

DefMap contains the module tree of a crate and stores module scopes.

### Interned IDs and Salsa

Each entity is identified by an int
```

### WebSearch: Dolt prolly tree content addressed versioning structural sharing identity 2024 2025
```
Web search results for query: "Dolt prolly tree content addressed versioning structural sharing identity 2024 2025"

Links: [{"title":"Storage Engine - Dolt Documentation - DoltHub","url":"https://docs.dolthub.com/architecture/storage-engine"},{"title":"Dolt's Prolly Trees: A Breakthrough in Version-Controlled Databases - Epw1m.putty P","url":"https://epw1m.putty-p.it.com/a/2747.html"},{"title":"How Dolt Scales to Millions of Versions, Branches, and Rows | DoltHub Blog","url":"https://www.dolthub.com/blog/2025-05-16-millions-of-versions/"},{"title":"Prolly Trees - Dolt Documentation - DoltHub","url":"https://docs.dolthub.com/architecture/storage-engine/prolly-tree"},{"title":"Dolt's Storage Engine | DoltHub Blog","url":"https://www.dolthub.com/blog/2024-02-29-storage-engine/"},{"title":"A Study in Structural Sharing in a Dolt Prolly Tree | DoltHub Blog","url":"https://www.dolthub.com/blog/2024-04-12-study-in-structural-sharing/"},{"title":"Storage Engine | Dolt Documentation","url":"https://www.dolthub.com/docs/architecture/storage-engine/"},{"title":"Prolly Tree | Dolt Documentation","url":"https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree/"},{"title":"Version-Controlled Vector Indexes: Achieving Structural Sharing in Nearest-Neighbor Indexes | DoltHub Blog","url":"https://www.dolthub.com/blog/2025-06-23-vector-index-deep-dive/"},{"title":"Prolly Trees | DoltHub Blog","url":"https://www.dolthub.com/blog/2024-03-03-prolly-trees/"}]

Based on the web search results, here's what I found about Dolt's prolly trees and their implementation regarding content-addressed versioning, structural sharing, and identity:

## Core Concept

A Prolly Tree, or Probabilistic B-tree, is a content-addressed B-tree. Dolt is built on a novel Search Tree, closely related to a B-tree, called a Probabilistic B-Tree, or Prolly Tree for short. Prolly Trees were invented by the Noms team specifically for database version control.

## Content-Addressed Versioning

In Prolly Trees, each internal node in the B-tree is labelled with an immutable, history independent hash or content address built from the node's contents. Content-addressing allows for fast comparison (ie. diff) and structural sharing of versions.

## Structural Sharing

Dolt's Prolly trees solve this by making each operation return a new tree that shares most of its structure with the old one—a technique known as structural sharing. More specifically, sections of the tree that share the same root hash ca
```

### WebFetch: https://rust-analyzer.github.io/book/contributing/architecture.html
```
# Interned IDs and Architecture in rust-analyzer

Based on the provided documentation, I can offer limited insights on your specific topics:

## What the Documentation Covers

The architecture document mentions **interned infrastructure** briefly: `"crates/intern` contains infrastructure for globally interning things via `Arc`."` However, it provides no details about how interned IDs function, their stability properties, or cross-session behavior.

## What's Notably Absent

The documentation explicitly does not address:
- How ItemTree functions as an invalidation barrier
- Specific details about DefMap implementation
- Salsa durability levels and their configuration
- ID stability within vs. across sessions
- Cross-version symbol identity mechanisms

## Key Architectural Principles Mentioned

The document emphasizes one relevant invariant: **"typing inside a function's body never invalidates global derived data."** This suggests the system maintains stability for certain derived structures when local changes occur, but the mechanism isn't detailed.

The closest relevant concept is that `"ItemTree condenses a single `SyntaxTree` into a 'summary' data structure, which is stable over modifications to function bodies,"` indicating ItemTree provides some form of change resistance, though the invalidation-barrier role remains unexplained.

**Bottom line:** Your questions require deeper technical documentation or source code examination beyond this architecture overview.
```

### WebFetch: https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/
```
# Glean's Fact Keying and Entity Identity System

## Fact Organization & Keying

Glean organizes indexed information as **predicates** (similar to database tables) containing **facts** (similar to rows). Facts are keyed by "a prefix of the fields." For example, to retrieve a specific C++ FunctionDeclaration efficiently, you specify known field values like the function's name and namespace, enabling millisecond-level query performance.

## Incremental Indexing Strategy

Rather than reindexing entire repositories, Glean pursues **"O(changes)"** complexity by processing only modified code. However, practical constraints apply: when a header file changes in C++, all dependent files must be reprocessed, creating "fanout" effects that limit optimization to **"O(fanout)"** in real scenarios.

## Cross-Revision Identity Through Database Stacking

Glean solves the cross-revision problem using an innovative "stacking" architecture. Multiple immutable database layers can "non-destructively add information to, or hide information from, the layers below." This allows simultaneous querying across different repository revisions without duplicating full datasets—each layer represents a changeset while maintaining unified query semantics.

## Symbol Identity System

Glass (Glean's abstraction layer) assigns every symbol a **symbol ID**—"a unique string that identifies the symbol." These IDs remain stable even when symbol definitions move, enabling permanent documentation URLs and cross-language reference tracking. The format varies per language but follows patterns like `REPOSITORY/cpp/folly/Singleton`.

This design enables ownership tracking and change analysis through **diff sketches**, which extract mechanical summaries of changesets in machine-readable form.
```

### WebSearch: glean.software "symbol id" glass "stable" cross-revision identity format
```
Web search results for query: "glean.software "symbol id" glass "stable" cross-revision identity format"

Links: [{"title":"Introduction | Glean","url":"https://glean.software/docs/introduction/"},{"title":"Blog | Glean","url":"https://glean.software/blog/"},{"title":"Indexing code at scale with Glean - Engineering at Meta","url":"https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/"},{"title":"Entra ID (OIDC) - SSO","url":"https://docs.glean.com/administration/identity/sso/configuration/entra-id-oidc"},{"title":"Using our identity schema to deliver personalized, permissions-aware results","url":"https://www.glean.com/blog/using-our-identity-schema-to-deliver-personalized-permissions-aware-results"},{"title":"Writing data to Glean | Glean","url":"https://glean.software/docs/write/"},{"title":"Indexing Documents - Glean Developer Portal","url":"https://developers.glean.com/indexing/guides/indexing-documents"},{"title":"About OIDC & SAML - Glean Help Center","url":"https://docs.glean.com/administration/identity/sso/oidc-saml"},{"title":"glean: A system for collecting, deriving and working with facts about source code.","url":"https://hackage.haskell.org/package/glean"},{"title":"Glean","url":"https://glean.software/"}]

Based on the search results, I found relevant information about Symbol IDs in Glean and Glass:

Glass assigns every symbol a symbol ID, a unique string that identifies the symbol. For example, the symbol ID for folly::Singleton would be something like, REPOSITORY/cpp/folly/Singleton.

The symbol ID can be used to link directly to the documentation page for the symbol, so there's a URL for every symbol that doesn't change even if the symbol's definition moves around. This demonstrates that symbol IDs provide stable identity across revisions.

All of this works for every language, although the exact format for a symbol ID varies per language.

Regarding cross-revision consistency, the search results indicate that Glean supports incremental indexing. Glean solves the first problem with an ingenious method of stacking immutable databases on top of each other. A stack of databases behaves just like a single database from the client's perspective, but each layer in the stack can non-destructively add information to, or hide information from, the layers below. This architecture helps maintain stable identities for symbols across different revisions of code.


REMINDER: You MUST include the sources above in your resp
```

### WebSearch: japicmp java api compatibility matching algorithm renamed moved class 2024
```
Web search results for query: "japicmp java api compatibility matching algorithm renamed moved class 2024"

Links: [{"title":"japicmp – japicmp-base","url":"https://siom79.github.io/japicmp/"},{"title":"Versions – japicmp-base","url":"https://siom79.github.io/japicmp/ReleaseNotes.html"},{"title":"GitHub - siom79/japicmp: Comparison of two versions of a jar archive · GitHub","url":"https://github.com/siom79/japicmp"},{"title":"Maven Repository: com.github.siom79.japicmp","url":"https://mvnrepository.com/artifact/com.github.siom79.japicmp"},{"title":"Backward Compatibility - JavaDev Bible","url":"https://karlete.github.io/javadevbible/javabible/topics/java-versions/backward-compatibility.html"},{"title":"Java API Compliance Checker","url":"https://lvc.github.io/japi-compliance-checker/"},{"title":"Maven Repository: com.github.siom79.japicmp » japicmp-maven-plugin","url":"https://mvnrepository.com/artifact/com.github.siom79.japicmp/japicmp-maven-plugin"},{"title":"CLI-Tool – japicmp-base","url":"https://siom79.github.io/japicmp/CliTool.html"},{"title":"Maven Plugin – japicmp-base","url":"https://siom79.github.io/japicmp/MavenPlugin.html"}]

I've performed the web search for information about japicmp's Java API compatibility matching algorithm, specifically regarding renamed and moved classes in 2024. 

Based on the search results, here's what I found:

japicmp is a tool/library aimed at providing a fast and easy to use API comparison for Java. japicmp differentiates between source and binary compatible changes (as described in the Java Language Specification and in the OpenJDK-Wiki).

The search results show various release notes and features related to japicmp's compatibility detection capabilities. While the results include information about how japicmp handles various types of changes, including METHOD_REMOVED_IN_SUPERCLASS is no longer reported if method is moved from superclass to superclass of superclass, the search results don't contain specific information about a matching algorithm for renamed or moved classes, or specific 2024 updates related to this functionality.

If you're looking for more specific information about a particular update or feature related to renamed/moved class detection in japicmp from 2024, you may want to check the project's GitHub repository directly or look at the detailed release notes on the japicmp website for more recent releases.


REMINDER: You MUST include the sources above in your response to the user using markdown h
```

### WebFetch: https://kythe.io/docs/kythe-uri-spec.html
```
# Kythe URI Specification Summary

## VName Fields & URI Encoding

The specification defines Kythe URIs as textual encodings of VNames (unique semantic graph node identifiers). The URI syntax comprises:

- **Scheme**: `kythe:`
- **Corpus**: Optional hostname-like identifier (`//label/path-segment`)
- **Attributes**: Language, path, and root (in fixed order for canonicality)
- **Signature**: Fragment identifier after `#`

## Character Requirements

URIs use UTF-8 encoding with NFKC normalization. The spec notes that "Reserved characters (_e.g._, "/", "?") and whitespace must be percent-escaped per Section 2.1 of RFC 3986."

## Signature Construction

The document does not provide detailed guidance on signature construction for specific entity types. It only establishes that signatures are "1*ipchar" per RFC 3987, with examples like `loc-a90320dafd60` and `class-Foo`.

## Cross-Version Identity & Normalization

**Not addressed in this document.** The specification focuses on URI syntax and format, not temporal identity management or versioning strategies.

## Key Takeaway

"The primary purpose of a Kythe URI is to provide a textual encoding of a Kythe VName, which is a unique identifier for a node in the semantic graph generated by Kythe-compatible tools."
```

### WebSearch: typescript api-extractor "@microsoft/api-extractor" symbol identity tsdoc cross-version 2024 2025
```
Web search results for query: "typescript api-extractor "@microsoft/api-extractor" symbol identity tsdoc cross-version 2024 2025"

Links: [{"title":"@microsoft/api-extractor - npm","url":"https://www.npmjs.com/package/@microsoft/api-extractor"},{"title":"@microsoft/api-extractor@7.58.10 - jsDocs.io","url":"https://www.jsdocs.io/package/@microsoft/api-extractor"},{"title":"tsdoc.json | API Extractor","url":"https://api-extractor.com/pages/configs/tsdoc_json/"},{"title":"@microsoft/api-extractor - npmx","url":"https://npmx.dev/package/@microsoft/api-extractor/v/%5E7.22.2"},{"title":"api-extractor.json | API Extractor","url":"https://api-extractor.com/pages/configs/api-extractor_json/"},{"title":"Architecture notes | API Extractor","url":"https://api-extractor.com/pages/contributing/architecture/"},{"title":"@microsoft/tsdoc - npm","url":"https://www.npmjs.com/package/@microsoft/tsdoc?activeTab=dependents"},{"title":"GitHub - pdabke/ts-api-extractor: Generates API metadata from Typescript type definitions","url":"https://github.com/pdabke/ts-api-extractor"},{"title":"Invoking API Extractor | API Extractor","url":"https://api-extractor.com/pages/setup/invoking/"},{"title":"Bootstrapping API Extractor","url":"https://www.thecandidstartup.org/2024/07/19/bootstrapping-api-extractor.html"}]

Based on the search results, here's what I found about `@microsoft/api-extractor` and related topics:

## Overview

API Extractor provides an integrated, professional-quality solution and is invoked at build time by your toolchain and leverages the TypeScript compiler engine.

## Symbol and Metadata Management

The pipeline produces several different representations including AstDeclaration for overload declarations, AstSymbol for the TypeScript type, CollectorEntity for the entry in the .d.ts file, and DeclarationMetadata, ApiItemMetadata, and SymbolMetadata to annotate the symbol and declaration with more info.

## TSDoc Integration

TSDoc is a standard syntax for TypeScript doc comments that can be extended with custom tag definitions, and API Extractor's custom tags are referred to as "AEDoc" tags. API Extractor serializes these definitions into the .api.json output files (in the "tsdocConfig" field) so that they are accessible to tools that use the @microsoft/api-extractor-model library.

## Cross-Version Compatibility

When API Extractor invokes the compiler engine to analyze your project, it uses its own TypeScript version because the compiler engine API may be incompat
```

### WebFetch: https://api-extractor.com/pages/contributing/architecture/
```
# API Extractor Symbol Identity and Breaking Change Detection

Based on the provided documentation, I cannot find specific information addressing your questions about:

1. **Stable ID assignment to TypeScript symbols** - The architecture notes do not explain how API Extractor generates or assigns persistent identifiers to symbols.

2. **Cross-version symbol tracking** - There is no discussion of mechanisms for tracking symbol changes across multiple versions.

3. **api.json format symbol keying** - While the document mentions that "the `ApiModelGenerator` takes our `CollectorEntity` for `add()` and converts it into an `ApiFunction` that will get serialized into .api.json," it does not detail how symbols are keyed in the resulting JSON format.

4. **Breaking change detection algorithm** - The architecture notes do not describe what matching algorithm identifies breaking changes or how comparisons are performed between API versions.

## What the documentation does cover:

The document explains that API Extractor produces `CollectorEntity` objects representing "top-level items in the .d.ts rollup" and that these get converted into `ApiItem` subclasses like `ApiFunction`. However, the actual mechanisms for symbol identity, versioning, and compatibility detection fall outside the scope of these architectural notes.

To obtain this information, you would likely need to consult the actual source code or additional API Extractor documentation not provided here.
```

### WebSearch: go apidiff golang.org/x/exp/apidiff how symbol matching works api compatibility 2024
```
Web search results for query: "go apidiff golang.org/x/exp/apidiff how symbol matching works api compatibility 2024"

Links: [{"title":"apidiff package - golang.org/x/exp/apidiff - Go Packages","url":"https://pkg.go.dev/golang.org/x/exp/apidiff"},{"title":"apidiff command - golang.org/x/exp/cmd/apidiff - Go Packages","url":"https://pkg.go.dev/golang.org/x/exp/cmd/apidiff"},{"title":"GitHub - joelanford/go-apidiff: Check API compatibility between different revisions of a Go project","url":"https://github.com/joelanford/go-apidiff"},{"title":"Go dependencies and API diffs · Marc Dougherty","url":"https://www.marcdougherty.com/2025/go-dependencies-and-api-diffs/"},{"title":"exp/apidiff at master · golang/exp","url":"https://github.com/golang/exp/tree/master/apidiff"},{"title":"x/exp/apidiff: interface compatible assumption may be wrong · Issue #45420 · golang/go","url":"https://github.com/golang/go/issues/45420"},{"title":"go-apidiff - GitHub Marketplace","url":"https://github.com/marketplace/actions/go-apidiff"},{"title":"apidiff package - golang.org/x/tools/internal/apidiff - Go Packages","url":"https://pkg.go.dev/golang.org/x/tools/internal/apidiff"},{"title":"Checking Go API Compatibility","url":"https://go.googlesource.com/exp/+/refs/heads/master/apidiff/README.md"},{"title":"go-apidiff command - github.com/joelanford/go-apidiff - Go Packages","url":"https://pkg.go.dev/github.com/joelanford/go-apidiff"}]

I've found comprehensive information about how symbol matching works in golang.org/x/exp/apidiff for API compatibility. Here are the key findings:

## How Symbol Matching Works

An old defined type corresponds to a new one if they have the same name, or one can be renamed to the other without otherwise changing the API. However, a single old type can correspond to only one new type.

To extend the definition to all types, apidiff parallels the language's definition of type identity. For instance, an old and a new slice type correspond if their element types correspond.

## API Compatibility Classification

apidiff reports two kinds of changes: incompatible ones, which require incrementing the major part of the semantic version, and compatible ones, which require a minor version increment.

To a first approximation, the tool reports a change as incompatible if it could cause client code to stop compiling. However, apidiff ignores five ways in which code may fail to compile after a change.

## Type Compatibility Rules

Only five kinds of types can differ 
```

### WebSearch: git similarity index rename detection algorithm "content similarity" threshold internals 2024
```
Web search results for query: "git similarity index rename detection algorithm "content similarity" threshold internals 2024"

Links: [{"title":"JGit’s similarity threshold for rename detection (60%) is different from git’s (50%) · Issue #110 · eclipse-jgit/jgit","url":"https://github.com/eclipse-jgit/jgit/issues/110"},{"title":"Git - git-diff-index Documentation","url":"https://git-scm.com/docs/git-diff-index"},{"title":"Question: How does git detect renames? – Chelsea Troy","url":"https://chelseatroy.com/2020/05/09/question-how-does-git-detect-renames/"},{"title":"Git - git-diff Documentation","url":"https://git-scm.com/docs/git-diff/2.6.7"},{"title":"Support setting the limit of similarity for rename detection · Issue #2904 · jesseduffield/lazygit","url":"https://github.com/jesseduffield/lazygit/issues/2904"},{"title":"Git - gitdiffcore Documentation","url":"https://git-scm.com/docs/gitdiffcore"},{"title":"Notifying users of file updates on computing devices using content signatures","url":"https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/8868501"},{"title":"Information source agent systems and methods for distributed data storage and management using content signatures","url":"https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/9678967"},{"title":"Information source agent systems and methods for distributed data storage and management using content signatures","url":"https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/8392705"},{"title":"Information source agent systems and methods for distributed data storage and management using content signatures","url":"https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11561931"}]

Based on the search results, here's what I found about Git's similarity index rename detection algorithm:

## Similarity Index and Rename Detection

The default similarity index is 50%. The similarity index is a threshold on the amount of addition/deletions compared to the file's size. For example, -M90% means Git should consider a delete/add pair to be a rename if more than 90% of the file hasn't changed.

## The Algorithm

In both rename and copy detection, the same "extent of changes" algorithm used in diffcore-break is used to determine if two files are "similar enough". The similarity score can be customized by providing a number after the "-M" or "-C" option. For example, "-M8" is used to set it to 8/10 = 80%.

## Recent Issues (2024)

File rename operations in a git commit a
```

### WebFetch: https://pkg.go.dev/golang.org/x/exp/apidiff
```
# Go `apidiff` Symbol Matching Algorithm

## Overview

`apidiff` uses a **correspondence** system to match symbols between old and new API versions. It determines compatibility by checking if exported names have corresponding equivalents with compatible types.

## Type Correspondence Definition

Rather than using Go's standard type identity (which requires exact same definition), `apidiff` defines a relaxed notion of **correspondence**:

> Two types correspond if they have the same name, or one can be renamed to the other without otherwise changing the API.

### Key Principles

1. **Defined Type Matching**: For defined types, correspondence requires the same name
2. **Renaming Support**: Unexported types can be renamed compatibly since clients cannot directly reference them
3. **Alias Support**: Type aliases allow defined types with different names to correspond

### Examples of Correspondence

**Type alias example** (compatible):
```go
// old
type E int

// new
type t int
type E = t
```
Old `E` corresponds to new `t` because the alias equates them.

**Unexported type rename** (compatible):
```go
// old
type u1 int
var V u1

// new
type u2 int
var V u2
```
`u1` and `u2` correspond because `u1` is unexported—only the exported variable `V` matters to clients.

**Breaking rename** (incompatible):
```go
// old
type T = struct { X int }

// new
type T = struct { X, Y int }
```
Type aliases with literal types don't correspond; the names don't match.

## Package Matching Algorithm

### Basic Compatibility Rules

**Package Compatibility requires:**

1. Each exported name in the old package appears in the new package with a compatible object
2. For every exposed type implementing an exposed interface in the old package, its corresponding type implements the corresponding interface in the new package

### Object Matching Rules

| Object Type | Compatibility Rule |
|-------------|-------------------|
| **Constants** | Same name, corresponding types, identical values |
| **Variables** | Same name, corresponding types |
| **Functions** | Same name, corresponding signatures |
| **Types** | Same name, corresponding underlying types |

### Symbol Lookup Limitations

The algorithm has **critical limitations for renames and moves**:

1. **Name-Based Matching**: Symbols are matched by name only
   - If a type is renamed, it appears as a new symbol (incompatible removal + addition)
   - No heuristic attempts to match by structure or semantic similarity

2. **Import Path Matching**: For modules, packages must have matching import paths
   - Package moves break compatibility, even if API is identical
   - No cross-package correspondence tracking

3. **No Move Detection**: The tool explicitly states it ignores many categories of changes:
   - Type switches involving merged types (esoteric but breaking)
   - Behavioral changes
   - Complex multi-type dependencies

## What "Exposed" Types Mean

A type is **exposed** if clients can declare variables of that type, even if unexported:

```go
type u1 int  // unexported but exposed
var V u1     // clients can use: var x uint32 = V
```

Exposed types can be renamed internally but maintain API compatibility.

## Handling Limitations

The README explicitly acknowledges the matching algorithm's constraints:

> The definitions given here don't account for [situations involving]:
> - Two types with identical underlying types (splits that break conversions)
> - Unexported methods affecting interface implementation across packages
> - Changes breaking downstream packages through embedding

For symbol renames, `apidiff` will report them as **incompatible changes** (removal + addition) rather than renames, requiring major version bumps even when the actual API is functionally preserved.
```

### WebSearch: scip crate rust bindings SCIP protocol 2024 2025 crates.io
```
Web search results for query: "scip crate rust bindings SCIP protocol 2024 2025 crates.io"

Links: [{"title":"scip - crates.io: Rust Package Registry","url":"https://crates.io/crates/scip"},{"title":"scip 0.9.0 - Docs.rs","url":"https://docs.rs/crate/scip/latest"},{"title":"scip-sys - crates.io: Rust Package Registry","url":"https://crates.io/crates/scip-sys"},{"title":"scip-sys 0.1.26 - Docs.rs","url":"https://docs.rs/crate/scip-sys/latest"},{"title":"SCIP — Rust text processing library // Lib.rs","url":"https://lib.rs/crates/scip"},{"title":"GitHub - scipopt/scip-sys: Raw rust bindings to SCIP's C-API · GitHub","url":"https://github.com/scipopt/scip-sys"},{"title":"crates.io: Rust Package Registry","url":"https://crates.io/crates/scip-sys/versions"},{"title":"russcip - crates.io: Rust Package Registry","url":"https://crates.io/crates/russcip/0.1.14/dependencies"},{"title":"GitHub - scipopt/russcip: Rust interface for SCIP · GitHub","url":"https://github.com/scipopt/russcip"},{"title":"scip_sys - Rust","url":"https://docs.rs/scip-sys"}]

API Error: Connection closed mid-response. The response above may be incomplete.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebFetch: https://git-scm.com/docs/gitdiffcore
```
# Git Rename Detection: Complete Analysis

## Overview of diffcore-rename

The `diffcore-rename` transformation detects renames and copies in Git diffs. It's controlled by:
- **`-M` option**: Detects renames only
- **`-C` option**: Detects both renames and copies

## Similarity Index Algorithm

### Core Mechanism

Git uses an "extent of changes" algorithm to determine if two files are "similar enough" to be considered a rename or copy. The algorithm:

1. **Calculates the amount of change** between original and modified content
2. **Adds deletion and insertion counts together**
3. **Compares against a similarity threshold**

### Default Similarity Score

- **Default: 50%** (can be customized with `-M<number>` or `-C<number>`)
- Example: `-M8` uses 80% (8/10) similarity threshold
- The similarity score represents the proportion of content that must match

### Content Similarity Computation

From the documentation:

> The same "extent of changes" algorithm used in diffcore-break is used to determine if two files are "similar enough", and can be customized to use a similarity score different from the default of 50% by giving a number after the "-M" or "-C" option (e.g. "-M8" to tell it to use 8/10 = 80%).

## Rename Detection Process

### Basic Rename Detection (-M flag)

**Input scenario:**
```
:100644 000000 0123456... 0000000... D fileX
:000000 100644 0000000... 0123456... A file0
```

**Process:**
- Compares deleted file `fileX` with created file `file0`
- If contents are similar enough, merges filepairs into rename

**Output:**
```
:100644 100644 0123456... 0123456... R100 fileX file0
```

### Copy Detection (-C flag)

When `-C` is used, the algorithm also considers:
- Original contents of **modified files**
- **Deleted files** as source candidates
- **Unmodified files** (only with `--find-copies-harder` flag)

**Input scenario:**
```
:100644 100644 0123456... 1234567... M fileY
:000000 100644 0000000... bcd3456... A file0
```

**Process:**
- Compares original contents of `fileY` with resulting contents of `file0`
- If similar enough, marks as copy operation

**Output:**
```
:100644 100644 0123456... 1234567... M fileY
:100644 100644 0123456... bcd3456... C100 fileY file0
```

## Preliminary Same-Filename Matching

When rename detection is enabled (but copy and break detection are off), a **preliminary optimization step** runs first:

### Purpose
- Checks if files are moved across directories while **keeping the same filename**
- Reduces computational cost of the full quadratic comparison

### Algorithm
1. If a deleted file (e.g., `docs/ext.txt`) and an added file (e.g., `docs/config/ext.txt`) have the same name and similar content
2. They are marked as a rename **immediately**
3. Excludes them from later pairwise comparison steps

### Key Constraint
> At most, one comparison is done per file in this preliminary pass; so if there are several remaining ext.txt files throughout the directory hierarchy after exact rename detection, this preliminary step may be skipped for those files.

### Higher Threshold
This preliminary step uses a **higher threshold** than the standard similarity score to avoid false positives and prevent better matches from being overlooked in later stages.

## The Break-Complete Rewrite Cycle

### Complete Rewrite Detection: diffcore-break

The `-B` flag (e.g., `-B50`) controls break detection:

**Trigger:**
- Detects when a file is "completely rewritten"
- Addition of new material + deletion of original contents > break score
- Default: 50% of the smaller file size

**Input:**
```
:100644 100644 bcd1234... 0123456... M file0
```

**Output (after complete rewrite detected):**
```
:100644 000000 bcd1234... 0000000... D file0
:000000 100644 0000000... 0123456... A file0
```

### Merging Back: diffcore-merge-broken

After break detection, `diffcore-merge-broken` **merges broken filepairs back together** if they weren't matched as renames/copies.

**Key difference in algorithm:**
> It counts only the deletion 
```

### WebFetch: https://docs.rs/crate/scip/latest
```
# SCIP Crate Analysis

**Version:** 0.9.0 (released 2026-06-29)

**What It Is:** This is the Sourcegraph SCIP protocol bindings, not the constraint solver. The crate provides: "a language-agnostic protocol for indexing source code, which can be used to power code navigation functionality such as Go to definition, Find references, and Find implementations."

**Key Components:**
- Protobuf schema for SCIP
- Rich Go and Rust bindings with utility functions
- Auto-generated TypeScript and Haskell bindings
- CLI tool for working with SCIP indexes

**Documentation Coverage:** 74.39% of items documented

**Dependencies:** protobuf =3.7.2 (normal), pretty_assertions ^1.4.1 (dev)

**Primary Use Case:** Building indexers and tools for code navigation across multiple programming languages (Java, TypeScript, Rust, C++, Python, Ruby, PHP, Dart, etc.)
```

### WebSearch: sourcegraph scip rust indexer protobuf bindings crate 2024
```
Web search results for query: "sourcegraph scip rust indexer protobuf bindings crate 2024"

Links: [{"title":"scip - crates.io: Rust Package Registry","url":"https://crates.io/crates/scip"},{"title":"Rust bindings: please update protobuf dependency · Issue #284 · sourcegraph/scip","url":"https://github.com/sourcegraph/scip/issues/284"},{"title":"SCIP — Rust text processing library // Lib.rs","url":"https://lib.rs/crates/scip"},{"title":"GitHub - scip-code/scip: SCIP Code Intelligence Protocol · GitHub","url":"https://github.com/scip-code/scip"},{"title":"scip 0.9.0 - Docs.rs","url":"https://docs.rs/crate/scip/latest"},{"title":"Rust Bindings | sourcegraph/scip | DeepWiki","url":"https://deepwiki.com/sourcegraph/scip/3.1-rust-bindings"},{"title":"scip module - github.com/sourcegraph/scip - Go Packages","url":"https://pkg.go.dev/github.com/sourcegraph/scip"},{"title":"sourcegraph/scip - protodoc.io","url":"https://protodoc.io/sourcegraph/scip/readme"},{"title":"GitHub - sourcegraph/scip-protobuf: SCIP indexer for Protocol Buffers (WIP)","url":"https://github.com/sourcegraph/scip-protobuf"},{"title":"GitHub - sourcegraph/scip: SCIP Code Intelligence Protocol · GitHub","url":"https://github.com/sourcegraph/scip/tree/main"}]

Based on the search results, here's what I found about Sourcegraph SCIP Rust indexer and protobuf bindings:

## SCIP Overview

SCIP (pronunciation: "skip") is a language-agnostic protocol for indexing source code, which can be used to power code navigation functionality such as Go to definition, Find references, and Find implementations.

## Rust Bindings and Protobuf

Rich Go and Rust bindings for SCIP include many utility functions to help build tooling on top of SCIP. If you're interested in consuming SCIP data, you can either use one of the provided language bindings, or generate code for the SCIP Protobuf schema using the Protobuf toolchain for your language ecosystem.

## Recent 2024 Updates

In 2024, there was discussion around protobuf dependency updates. An issue was opened on October 16, 2024, regarding a bug in the protobuf crate where it wouldn't interact properly with build systems that use RUSTC_WRAPPER, which was released as a semver-compatible update (v3.7.1), but the crate pins protobuf to =3.2.0.

## Latest Rust Bindings Improvements

Recent updates to the Rust bindings include bumping minimum supported Rust version (MSRV) to 1.81.0 and updating the pinned version of the protobuf crate to 3.7.2.


REMINDER: You MUST inclu
```
