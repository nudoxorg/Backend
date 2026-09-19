# Compiler semantic cutover ledger

This ledger compares the deleted compiler at `HEAD:compiler/**` with the replacement in
`crates/compile` and `frontends/*`. It is a deletion gate, not a claim that compiling and parsing
are equivalent. A green syntax fallback establishes source navigation only. Semantic parity
requires a language authority that emits admitted records for every required relation.

## Result

The replacement is materially stronger in its shared process and evidence boundary and still
weaker in five languages' semantics and only partially restores Python. The old compiler contains 156,182 lines of Rust plus committed
native helpers. Most of the removed
code was not framework duplication: it encoded language-specific typing, name resolution, package
discovery, documentation, occurrences, diagnostics, and canonical lowering.

The compiler cutover is therefore **not semantically complete**. Go now executes its bundled
`go/packages` authority from product ingestion and publishes admitted records. Python executes a
bundled CPython AST producer as a partial semantic-index authority; its richer in-process Ruff and
Pyrefly components are not yet the product authority. The other five product lanes retain
`Unavailable`; they do not silently promote their explicit tree-sitter
baseline to semantic coverage.

Status notation: **S** is stronger than the old design, **E** retains the old capability, **P** is a
real but partial replacement, **M** is missing from the shipped implementation. “Protocol” means the
new envelope has a generic record kind, but no production language helper in this tree produces the
old oracle's validated payload.

| Language | Old implementation | Parse | declarations / names | types | docs | packages / dependencies | occurrences / edges | diagnostics | malformed / unresolved |
|---|---:|---|---|---|---|---|---|---|---|
| C/C++ | 3,822 LOC | P: tree-sitter C++ baseline for C/C++/ObjC extensions | P: tag facts; typed native adapter admits closed declaration states | M: adapter admits a closed shape, but no libclang producer emits it | P: adjacent comment text | M: compilation DB, include graph, PURL | M: adapter vocabulary exists without a libclang producer | M: adapter admits severity without a producer | P: malformed syntax and malformed native cells reject; unresolved producer facts are missing |
| C# | 8,852 LOC + Roslyn helper | P: tree-sitter baseline; cold Roslyn helper source parses one compilation | P: typed adapter and helper prototype emit named declarations | P: helper prototype emits display signatures; old structural nullability/generic/constraint products remain missing | P: adjacent XML comments plus helper XML documentation strings | P: helper prototype emits referenced assemblies; package/PURL lifecycle is missing | P: helper prototype emits resolved identifier references | P: compiler diagnostics with code, severity, span, message | P: malformed native records reject; helper is cold-only, unbundled, and not verified on this host |
| Go | 8,407 LOC + `go/packages` helper | E: production helper parses through `go/packages`; tree-sitter remains baseline | P: package-scoped declarations, closed kinds, exact name spans | P: recursive `go/types` signatures and generic parameters render canonically; structural child roles/method-set satisfaction remain missing | P: declaration-owned Go docs | P: module/import rows; product ingestion is currently file-scoped and therefore records `Partial` across sibling/build-tag alternatives; PURL lifecycle remains missing | P: local/foreign resolved uses with exact spans inside the file-scoped helper request; enclosing declaration ownership remains incomplete | P: structured `go/packages` errors | P: malformed payloads reject; missing/bootstrap-failed toolchain is typed unavailable; file scope cannot mint package-complete coverage |
| Java | 5,313 LOC + javac/doclet helper | P: tree-sitter baseline | P: tag facts; typed javac payload adapter | M: adapter validates balanced signatures, but no javac attribution producer exists | P: adjacent Javadoc; adapter retains helper documentation bytes | M: modules, packages, JAR/Maven/repository acquisition producer | M: typed reference payload exists without a javac Trees producer | M: typed diagnostic payload exists without a producer | S: release is closed; P: malformed syntax and payloads reject without attribution failures |
| Python | 4,238 LOC | P: product CPython AST producer; typed Ruff AST extraction exists beside it but is not wired through the product authority | P: product emits classes, functions, fields, parameters and owned docstrings | P: written annotations become bounded type records; Pyrefly checker integration is not yet the product type authority | P: declaration-owned Python docstrings | P: import modules and explicit negative dependency; environment/PURL resolution remains missing | P: product emits local/foreign/unresolved name edges with byte spans; heuristic scope resolution remains weaker than Pyrefly | P: structured syntax diagnostics; checker diagnostics are not yet published | S: version is closed; P: malformed syntax and hostile payloads reject; unresolved reasons remain partial |
| Rust | 2,434 LOC + rust-analyzer | P: tree-sitter baseline | P: written tag facts; M: HIR-only/macro-expanded declarations | M: typed boundary validates records but has no HIR producer | P: adjacent rustdoc | M: Cargo workspace/features/PURL | M: methods, fields, macros, intra-doc links and resolved targets | M: rust-analyzer spans | P: malformed syntax and hostile records reject; source-map/macro provenance and unresolved type terminals are missing |
| TypeScript | 3,275 LOC + OXC/tsc helper | P: tree-sitter TS/TSX baseline | P: tag facts; typed boundary validates records | M: no producer for unions/intersections/tuples/mapped/conditional/template types, overloads, narrowing | P: adjacent JSDoc text | M: package/module authority and imports | M: bound references, calls, properties and confidence | M: OXC/tsc diagnostic sets | S: TS/TSX profile is closed; P: malformed syntax and hostile records reject without UTF-16 coordinates or structured errors |

## Shared execution and lifecycle comparison

| Concern | Replacement | Verdict |
|---|---|---|
| Input identity | Complete positive and negative input manifest; content-derived identities | S |
| Toolchain identity | Verified executable content, copy lease against in-place mutation, dependency closure | S |
| Native execution | One bounded supervisor for stdin, stdout, stderr, filesystem growth, process count, CPU time, deadline and process-group cleanup | S |
| Cancellation | Shared cooperative token plus process-group termination; persistent requests carry sequence identity | S |
| Caching | Content/version keyed preparation cache and bounded persistent-session cache; unchanged fact state path-copies | S |
| Coverage | Complete/partial/unavailable/unsupported are distinct; complete output requires admitted scope evidence | S |
| Profiles | Closed profile vocabulary exists; Java, Python and TS parse to typed variants before environment validation and aliases normalize before manifest hashing | S for those three; M for extracting Rust/Go/C/C#/C++ profiles from their configuration inputs |
| Semantic wire | Bounded, ordered native record envelope with six coarse record kinds | P |
| Semantic values | Every frontend owns a parse-admit-lower adapter. Go and Python execute typed adapters in product ingestion. Java and C# decode typed payloads; Clang admits a closed cell vocabulary; Rust and TypeScript currently prove only boundary invariants without a product producer | P |
| Production helpers | Go ships a source-distributed `go/packages` helper on the new cold/persistent wire. Python ships an isolated source-distributed CPython AST helper; this is not a Pyrefly type checker. C# contains an unbundled cold Roslyn helper prototype. This host has no `dotnet`, so C# was not built here | P overall; E for executable Go/Python call paths, P for their current file-scoped product requests |
| Product selection | A typed native-first registry selects verified Go and Python authorities. Each missing authority is retained as semantic `Unavailable`; tree-sitter runs only as an explicit structural browsing baseline. Health exposes Go type/index slots when Go is installed, Python semantic-index only when Python is installed, and keeps Python type-check unavailable | S boundary; P language coverage |
| Product native batching | The Go product path currently starts a bounded cold helper for each file. It neither batches a package/workspace manifest nor reuses one persistent helper across the scan, so its coverage is partial and its startup cost scales with file count | M |
| Canonical lowering | `PSR6` retains coverage, authority, manifest and revision, but its fact payload is still a transitional kind/key/value envelope. Go NUL edge records currently reconstruct a typed Trustfall target; Python JSON edges and the other relation families still flatten into display rows. This does not satisfy the final typed `backend-semantic` relation contract | M for final cutover; P for visible facts |

## Required semantic contract

Every production language authority must pass the same normalized fixture families while retaining
language-specific facts. The checked-in fixtures under `tests/compatibility/fixtures/parity` establish
the common declaration floor. Each authority must then add its oracle rows:

1. Parse exact bytes under a closed language profile and retain every structured diagnostic.
2. Admit bounded declarations and stable names with exact UTF-8 byte spans; TypeScript also retains
   checked UTF-16 conversion.
3. Lower recursive types without rendering them to display strings. Child roles and unresolved
   reasons remain typed.
4. Preserve documentation ownership, flavor, code fragments, and resolved links where the language
   oracle provides them.
5. Bind package/module/assembly/workspace identity and both positive and negative dependencies.
6. Emit occurrences with kind, owner, span, target, and confidence. Only sufficiently resolved
   occurrences project graph edges.
7. Return explicit partial/unavailable coverage on cancellation, missing tooling, bounds, or
   incomplete resolution. These terminals never authorize deletion.
8. Prove deterministic cold and warm output against the same input, profile, toolchain, helper,
   contract, dependency, and authority identities.

The gate passes only when the production helper for each language regenerates its normalized oracle
fixtures byte-exactly, mutation tests identify the exact rejected field, and the end-to-end journey
shows those semantic rows through local storage, search, Trustfall, MCP, CLI, and desktop clients.

## Structural rule

The target shape is one small shared kernel for process/evidence/session mechanics and seven narrow
language adapters. Language semantics stay in language-owned modules because Rust HIR, Roslyn
nullability, Go method sets, javac attribution, Python annotation reasons, TypeScript narrowing, and
Clang layout are different facts. Factoring those differences into strings or a universal syntax
record would reduce code by deleting meaning.

Within each adapter the control flow is `parse -> admit -> lower`: parsing creates language-native
borrowed data, admission proves bounds/profile/coordinates and complete authority, and lowering
projects only admitted values into canonical relations. A syntax fallback stops after its own
admitted source records and cannot construct semantic coverage.
