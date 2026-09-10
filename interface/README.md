# Interface layer

Three surfaces — a GPUI desktop app, a CLI, and an MCP server — over one shared
local library. A package added from any surface is readable from the other two
on their next read. No daemon: durable state lives under one workspace root and
every writer bumps an epoch file that readers watch.

```
interface/
  core/        ApplicationService, ClientIndex sync control plane   (existing)
  protocol/    framed/JSON wire decoding                            (existing)
  identity/    Address · PackageCoordinate · SymbolPath · ContentKey
  documents/   Page · Outline · Symbol · Signature · Prose · visitors
  search/      SearchRequest · SearchTerminal · Lane · Coverage · Graph*
  library/     Library · Shelf · Admission · Image · Command/Reply · render
  cli/         `nudox` binary: Command ← argv, Reply → text/markdown/json
  mcp/         `nudox-mcp` binary: Command ← tools/call, Reply → markdown
  gui/         `nudox-gui` binary: Command ← palette/clicks, Reply → elements
```

Dependency direction is strictly downward in that list. `gui`, `cli`, and `mcp`
depend on `library` and never on each other. `library` is the only crate that
touches the compiler runtime, the durable publication store, Tantivy, Trustfall,
or Qdrant.

## The one dispatch

```rust
let reply: Reply = library.execute(Command::Search(request), &mut progress);
```

`Command` is a closed enum (`library/command.rs`). `COMMANDS` is the registry
every surface projects: CLI subcommand names, MCP `tools/list`, and the GUI
palette are three renderings of the same nine rows, with the same one-sentence
descriptions. A capability that exists on one surface and not another is a bug
in that surface, not a product decision.

## Invariants the types carry

| Invariant | Type |
|---|---|
| You cannot compile without exclusive permission | `Admission<'lib>`: move-only, borrows the library, consumed by `run`; dropping it unrun records a cancelled shelf row |
| Image bytes never escape | `Library::read(&card, |image: &Image<'_>| …)`: views live inside the closure, only owned document data comes out |
| A page-bearing address is always resolvable | `ExactAddress` is minted only by a projector holding an image; parsed user text is a plain `Address` until resolved |
| Every symbol row is the same row | `documents::Symbol` is the header of `Page`, `MemberRow`, `OutlineNode`, `Hit`, `GraphEdge`, crumbs |
| A lane that did not run says so | `Coverage::Unavailable { reason }` beside every lane on every `SearchTerminal`; zero rows never masquerade as "no matches" |
| Text budgets are types | `QueryText`, `ResultLimit`, `Depth`, `SymbolPath`, `PackageCoordinate` validate on construction |
| No public primitive fields | Counts, epochs, offsets, and flags are newtypes or enums (`.config/dylint` `SEMANTIC_SCALAR`) |

## Identity

```
cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]#<family>.<variant>
└──── coordinate ───┘ └────────── path (kind tags optional) ──────────┘ └───── key ─────┘
```

The key is the compiler's `DeclarationIdentity`: 16-byte family (stable across
overloads and moves) and 16-byte variant fingerprint, lower hex. The abbreviation
shown beside a symbol is the first eight hex characters of the family and is
never accepted as input. Legacy bare keys parse as an address with an empty
path. Ecosystem tags are `cargo npm pypi go maven nuget cpp`; the package-url
spellings `golang` and `generic` are accepted on input and never emitted.

## Storage beneath the workspace root

```
<root>/            NUDOX_DATA_ROOT, else ~/Library/Application Support/Nudox
  artifacts/       compiler-owned immutable publications
  journal/         compiler-owned publication journal
  library/
    shelf.db       requested packages, status, publication locators (SQLite via turso)
    epoch          decimal counter, atomically renamed into place after every mutation
    lock           compile lock: pid + heartbeat; stale after 3 missed heartbeats
    tantivy/       durable lexical projections keyed by lexical segment identity
```

## Faults are content, not chrome

Every failure in this layer is a typed value with the exact operand retained:
which package, which phase, which byte offset, which lane. Surfaces render those
values in place — beside the row, inside the panel, under the field — never as
a modal, a toast that disappears, or a string that says "something went wrong".
A degraded lane, a detached compiler, an unconfigured Qdrant endpoint are all
ordinary states with ordinary rendering.

## Rendering discipline

* Markdown never puts an address or a signature inside a table cell; cell
  escaping corrupts lifetimes (`\'`) and union pipes (`|`), which are exactly the
  spellings a reader copies back. One record per line, signature on its own.
* Coverage and scope are one-line signals (`~lanes: exact✓ names✓ graph✓ semantic✗ no-embedder`),
  not sentences.
* Renderers are driven by `documents::walk_page` so Markdown and terminal output
  cannot disagree about what a page contains.
