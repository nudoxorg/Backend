# Invariants

What must not break. Violating any of these compiles fine and fails later,
usually silently, usually in a way no test in this repo would catch.

Ordered by blast radius. Verified against `main` = `b388f36d`, 2026-08-13.

---

## The content-hash encoding is wire-stable

`crates/nudox-ir/src/content.rs:40` — "Once a hash is committed to an `ir-vcs`
checkpoint, it must reproduce identically from the same `Entry` on any platform
and at any future date."

Rules the module states and enforces (`content.rs:44-56`):

- Every sequence is `u32le(count)` then elements — no magic separators.
- Every enum variant has a **distinct opcode byte**, tabulated at
  `content.rs:60-110`. **There are no `_` catch-alls anywhere in the module**
  (`content.rs:47`): a wildcard would silently stop distinguishing new variants.
- No `{:?}`, `Display`, `serde`, or platform-dependent formatting is used as
  hash input. Strings are `encode_str` (u32le length + UTF-8).
- `usize` is widened to `u64` before encoding, so 32- and 64-bit hosts agree.

**If you add a variant to any enum covered by `content.rs`, you must assign it
a new opcode there** (`content.rs:49-50`). Reusing or renumbering an existing
opcode silently invalidates every hash ever committed.

The `Kind` wire discriminants come from the `register_kinds!` literals at
`crates/nudox-ir/src/kind.rs:5`. They are duplicated as a table at
`content.rs:60-76`; both must agree. `Ref::Local` must not appear in a
post-seal entry — it is given opcode `0x00` so the encoder is total, with a
`debug_assert!` to catch seal bugs (`content.rs:86-89`).

Related and equally frozen: `nudox_ir::body::Language::tag()`
(`crates/nudox-ir/src/body.rs:57`) — "wire-visible, so treat them as frozen:
renaming one is a format change" (`body.rs:56`).

## The §L0 dependency law

`crates/nudox-engine/src/lib.rs:3-13`:

```
lindsey  →  nudox-engine  →  {nudox-graph, nudox-store}  →  nudox-ir
```

plus `nudox-mcp → {nudox-engine, nudox-graph}` (root `Cargo.toml:66-70`).

- **`lindsey` must not import `nudox-ir`, `nudox-store`, or `nudox-graph`
  directly.** Reaching past the engine lets a view depend on the shape of the
  IR rather than on the protocol (`workspace/gui/Cargo.toml:34-37`). Its only
  backend dependency is `nudox-engine`.
- **`nudox-engine` must not import `gpui`.** `workspace/gui` is deliberately
  *not* a Cargo workspace member and keeps its own lockfile, precisely so
  `gpui` stays out of the backend dependency graph
  (`workspace/gui/Cargo.toml:6-10`).

`crates/nudox-engine/src/lib.rs:13` claims both are "enforced by
`scripts/lint-gui-no-block.sh`". **That script does not exist** — there is no
`scripts/` directory. The law is enforced today only by the `Cargo.toml` graph
and by review. Do not rely on a lint catching a violation.

## The two planes share only `nudox-ir`

`workspace/*` and `crates/nudox-*` are largely disjoint dependency trees. The
only shared crate is `nudox-ir`, aliased as `ir` in all four server-plane
consumers.

**Do not add a `workspace/* → crates/nudox-producer*` dependency**, and do not
add a `crates/nudox-* → workspace/*` dependency. Today neither exists, and the
separation is what lets the local-first plane ship without a catalog, an object
store, or a vector database. See
[orientation.md](orientation.md#the-two-planes).

## The sealed-compute boundary

`workspace/compiler/sandbox/lib.rs:24-38`, under `#![deny(missing_docs)]`.
These are encoded in the type system; breaking one means weakening a type, not
just changing behaviour:

- **`Env` is only an allowlist.** The ambient host environment is never
  inherited (`lib.rs:26`).
- **`Network` / `NetGrant` is explicit and defaults to off** (`lib.rs:27`).
- **"Sealed with the network on" is unrepresentable.** A sealed job's network
  field is the `NetOff` marker, and the only VM policy it can project to is
  `NetworkPolicy::None`. The machine is built without a NIC — "the typestate
  states a physical fact, not a filter rule" (`lib.rs:28-31`).
- **`Limits` requires every ceiling** — no silent "unlimited" field
  (`lib.rs:32`). `LimitOverride` is sparse via `NonZero*`, so zeros are
  unrepresentable (`lib.rs:33`).
- **`DevPassthrough` cannot be constructed under `Policy::Production`**
  (`lib.rs:34`).
- **`VmForge` is obtainable only over a production-grade cage** (SV-4), so
  untrusted-source `Compile` impls cannot run outside the VM boundary
  (`lib.rs:35-37`).

The OS-sandbox layer (bwrap, seccomp, landlock, Seatbelt) was deleted; cgroups
survive only as a scheduling-fairness wrapper, **never as a security layer**
(`lib.rs:15-18`).

At the call site: the compile phase runs with `NetGrant::Off`, `Env::empty()`,
an RO source mount and a scratch overlay, and no host-side toolchain binds
(`workspace/driver/coordination/indexing.rs:938-949`). A forge node that cannot
run the cage **fails the job loudly** rather than emitting an IR-less blob
(`workspace/driver/error.rs:292-295`).

## `ProducerId` must be version-bumped on any output-shape change

`crates/nudox-producer/src/lib.rs:107`. Convention `"<lang>-<tier>/<version>"`.

The id becomes part of the job key, and the job key gates cached-IR
invalidation (`lib.rs:102-105`). Change what `lower` emits without bumping the
suffix and the cache reports a hit forever: the system serves IR from the
previous output shape, for every already-indexed package.

**Nothing checks this at runtime.** No schema hash, no test. The suffix is the
only mechanism. Bump it for new or removed IR kinds, changed field semantics,
changed span conventions, changed visibility rules, or a different oracle whose
output you lower differently.

## Cargo is authoritative over Buck2

Cargo covers every live crate on both planes; Buck2 covers five targets and
three of them **cannot resolve**, because `build/rust.bzl:12-18` maps
`"ir" → "//workspace/ir:ir"` and `workspace/ir/` was deleted on 2026-07-27,
twelve days after the Buck2 files were last touched.

- **Do not add a Cargo dependency solely to satisfy a `BUCK` file.**
- **Do not treat a `BUCK` file as a source of truth about the dependency
  graph** — root `BUCK` still aliases the deleted `ir`.
- A change that adds a crate needs a root `Cargo.toml` members entry; it does
  **not** need a `BUCK` target.

Detail: [../building-and-testing.md](../building-and-testing.md#the-buck2-break-precisely).

## The wire *is* the domain on the search surfaces

`heart::query::Query` (`workspace/heart/query.rs:264`) is both the domain type
and the wire type. There is no request DTO, deliberately
(`workspace/driver/http/handlers/search.rs:5-11`, INDEX-PLAN §9).

**Any change to `heart::query::Query`, `Target`, `Scope`, `QueryMode`,
`QualityMode`, `QueryReach`, `Routing`, `PageSpecification`, or `AsOf` is an
API break**, whether or not you thought of it as one. `heart` sits below the
vector plane, so those routing enums are owned there as plain wire enums and
the vector crates map `From` them — the arrow never points back into `heart`
(`query.rs:8-14`).

Watch the serde spelling: `Target` and `RankSpecification` are **not**
`rename_all`, so they serialize PascalCase; `QueryMode`, `QualityMode`,
`QueryReach`, and `Language` **are** lowercase. Adding `rename_all` to one of
the PascalCase enums silently breaks every existing client.

Update [../api-contract.md](../api-contract.md) with any change here.

## `StableReference`'s grammar is frozen

`workspace/heart/query.rs:48`, parser at `:71`:

```
F:<ecosystem>/<package>#<intro_hex>
```

`#` is the **sole** terminator (matched with `rsplit_once`) precisely so
`<package>` may contain `/` — registryless repo-slug stems depend on it.
`<intro_hex>` must be lowercase; uppercase is a parse error. It serializes as a
bare JSON string via `#[serde(try_from = "String", into = "String")]`
(`query.rs:47`).

The local-first plane's `SymbolKey` (`ecosystem:name#introhex`) is the same
idea with a different spelling, and MCP's LR-1 forbids inventing a third.

## The instance token salts every derived symbol id

`Endpoints::instance_token()` (`workspace/driver/config.rs:514`) is
`"{instance_organization}/{instance_database}"`, default `nudox/registry`,
passed to `InstanceToken::new` at assembly (`workspace/driver/lib.rs:389`).

**Changing either key re-salts every derived symbol id.** Every previously
indexed symbol becomes unreachable by its old id — existing Qdrant points,
links, and session graphs all reference ids the server will never mint again.
The field's own doc comment says so (`config.rs:201-202`).

These two keys are vestigial: they were the TerminusDB organization and
database. The graph database is gone; the salt remains.

Related: source **names** seed deterministic `SourceId`s
(`workspace/driver/config.rs:399`), which is why `validate()` rejects empty and
duplicate names (`config.rs:456-466`). Renaming a source changes its identity.

## Ingest is idempotent end to end, and the watermark advances last

The pipeline's entire crash-safety argument rests on this. Every step is
idempotent: identity upsert, one-live-job-per-package enqueue, content-addressed
`put_section`/`put_manifest`, `PointId::from_symbol` upsert, idempotent outbox
intents.

**`consume_once` materializes an intent and only then advances the watermark**
(`workspace/driver/poll.rs:131-138`). A crash between the two re-delivers
rather than drops. **Do not reorder those two calls**, and do not add a
non-idempotent sink write without also changing that ordering argument.

Similarly, `serve()`'s shutdown aborts pollers mid-tick on the explicit grounds
that "every unit of poller work is idempotent, so an abort mid-tick is safe"
(`workspace/driver/lib.rs:601-603`).

Caveat: the per-sink lock is **in-process only** — a `tokio::sync::Mutex`
(`workspace/index/coordination/outbox.rs:277`), despite a comment at
`workspace/driver/poll.rs:79-83` claiming a cross-replica advisory lock. Two
`driver` processes will both drain the same sink. Idempotency is currently the
*only* thing making that safe. See
[OQ-12](../open-questions.md#oq-12--is-single-replica-outbox-drain-a-requirement).

## The MCP standing decisions

`crates/nudox-mcp/src/lib.rs:34-53`. Review-blocking, not style:

| # | Rule |
| --- | --- |
| **LR-1** | The tool argument *is* `SymbolKey`, spelled `ecosystem:name#introhex`. No parallel id type. |
| **LR-2** | No `serde_json::Value` in any tool signature. Every schema is `schemars`-derived from a named type. |
| **LR-6** | No `unwrap`/`expect` in Trustfall resolution paths; every fallible step returns `Result<_, GraphError>` (`crates/nudox-graph/src/adapter.rs:3-9`). |
| **LR-7** | One schema. `SCHEMA_SDL` is re-exported from `nudox_graph::SCHEMA_SDL` so there is exactly one `include_str!`; `crates/nudox-mcp/tests/schema_source.rs` asserts byte-identity. |
| **LR-8** | `nudox-mcp` reads no `nudox-ir` type directly, holds no corpus, opens no files, walks no `Entry`. **Every tool bottoms out in an `EngineHandle` call.** "If a tool here ever answers a question the engine could not, that is a bug" (`crates/nudox-mcp/src/lib.rs:15-19`) — it would let the GUI and an agent disagree about the same workspace. |
| **LR-11** | `session::Unauthenticated` and `session::Session` are different types, not a `bool`. |
| **LR-12** | `McpError` is a `#[non_exhaustive]` `thiserror` enum. No `anyhow` outside tests. |

Also: the MCP endpoint binds `LOOPBACK_BIND` = `127.0.0.1:0`, **a constant, not
a setting** (`crates/nudox-mcp/src/endpoint.rs:56`). "There is no code path in
the crate that binds anything else" (`endpoint.rs:20-21`), and a
`debug_assert!` re-checks it (`endpoint.rs:105`). Do not make it configurable
without deciding that deliberately.

## Stream tools must not answer short

Every MCP streaming tool holds its `StreamHandle` for the whole drain —
dropping it cancels the stream (`crates/nudox-mcp/src/tools.rs:317`, `:392`,
`:508`). A stream that ends without its terminal event is
`McpError::TruncatedStream`, never a quietly short result (`tools.rs:335`,
`:415`, `:540`). `get_symbol` additionally enforces that `Head` precedes
everything: a `Done` with no `Head` is a broken engine stream, not an empty
symbol (`tools.rs:418-420`).

Truncation by limit is reported explicitly via the `truncated` flag on
`SearchResult` / `UsagesResult` / `QueryResult`, and the merged list is
re-sorted before truncation so the ceiling cuts by relevance rather than by
section order (`tools.rs:379-384`).

## `driver`'s `registry` facade is load-bearing

`workspace/driver/lib.rs:35` declares a crate-local `pub mod registry` that
**shadows** the extern-prelude `registry` crate; the real crate is imported as
`xregistry` (`workspace/driver/Cargo.toml:36`) and reachable as `::registry`.
A second facade `pub mod vector` sits at `lib.rs:78`.

Inside `driver`, bare `registry::…` and `crate::registry::…` both mean the
facade. **Do not "fix" a `use crate::registry::…` to `use registry::…`** — they
resolve to the same place, and changing the facade's re-export list breaks ~13 k
lines of moved composition code that was authored against the old monolithic
`registry` surface (`lib.rs:17-33`).

## The federation is all-or-nothing at boot

`Driver::assemble` connects the definitive base and every overlay before the
server value exists, so "served a request against a store that wasn't up" is a
compile error rather than a runtime one (`workspace/driver/lib.rs:5-8`). **Any
single source failing to connect aborts boot**, overlays included
(`lib.rs:256-258`).

Degradation is a *runtime* state reported by `/readyz`, not a boot-time one: a
down overlay is `degraded` with status `200`; only a down definitive base gives
`503` (`workspace/driver/http/handlers/health.rs:26`).

Reads resolve overlay-first — first source to claim a key wins, then rank by
score (`workspace/driver/search/mod.rs:66`). Writes go to the base only
(`workspace/driver/coordination/initialization.rs:99`).

## The server is monomorphized over one embedding model

`type EmbedModel = driver::vector::JinaCodeV2` (`workspace/driver/main.rs:13`).
The whole server is generic over this brand "so a store built for a *different
model* (not merely a different dimension) cannot be wired in"
(`main.rs:9-12`). The Qdrant collection schema is bound to it via
`CollectionConfig::for_model::<M>()` (`workspace/driver/lib.rs:416`).

Switching models is a deliberate recompile plus a data migration. It is not a
config change — note that `endpoints.qdrant_collection` exists in config and is
read by nothing.

## Documentation obligations

- A route change **must** update [../api-contract.md](../api-contract.md). It
  is the canonical contract and the Nudox/Web docs link to it rather than
  restating the schema.
- A gap you cannot resolve goes in
  [../open-questions.md](../open-questions.md) with what would resolve it — not
  into a plausible-sounding sentence. OQ numbers are stable; a resolved
  question keeps its number and is marked **RESOLVED**.
- Commit messages must be Conventional Commits (`convco` pre-push hook,
  `flake.nix:258-266`).
