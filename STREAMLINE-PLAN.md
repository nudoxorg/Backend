Streamline Plan — More for Less
===============================

*2026-07-10. Consolidates the codebase audit into one execution plan: delete
what's dead, wire what's valuable, unify what's duplicated, and make the
remaining surface better-typed than what it replaces. Every claim below was
verified against the code (callers, LOC, semantics) — not carried over from
the audit on faith.*

**Corrections to the audit made during verification:**
- `workspace/server/forge.rs` EXISTS and `ForgeRuntime` is wired — FORGE-PHASE4-PLAN
  is in progress, not stale.
- `registry/search/tantivy.rs` (`PackageIndex`) is LIVE (the server path uses it);
  only `RegistrySearch`/`ranking.rs`/`multi_parent.rs` are dead.
- The vector pagination snapshot is *deliberately* advisory (ANN has no cheap
  content hash) — the fix is to encode that in the type, not to bolt on the
  text index's check.

**Already done (2026-07-10):**
- README.md rewritten around the real workspace/ + Buck2 architecture;
  workspace/README.md populated; PLANS.md index created. (Audit Theme 1.)
- Stale worktrees pruned (~4.7 GB freed). Uncommitted deltas preserved on
  branches `worktree-agent-a2e4f64b5a2fba6d9` (2,254-line pyrefly third-party
  vendoring — revisit for the Python producer) and
  `worktree-agent-a74b108d91c4b8ea8`. (Audit Theme 4.)

Principles
----------

1. **Delete-first.** Git is the archive; speculative surface is a liability
   (Meta SCARF: 100M+ LOC deleted, maintenance cost of deleted code is zero —
   dl.acm.org/doi/10.1145/3611643.3613871; typical mature systems carry 25-30%
   dead code — IEEE TSE '18). Anything not wired to a concrete planned endpoint
   defaults to DELETE, not "keep for later".
2. **Parse, don't validate.** Untrusted data (HTTP body, DB row, config)
   crosses the boundary once and emerges as a domain type whose construction
   is the only valid path. No `.expect()` past the boundary.
3. **Proof-carrying APIs.** Where a check matters, return a typed witness
   (capability token, snapshot-carrying cursor, typestate), not `Ok(())`.
   Unauthorized/unchecked calls become unrepresentable, not just rejected.
4. **One source of truth per fact.** Enum↔token maps, version grammars,
   hash definitions, sandbox capability structs: exactly one definition,
   everything else derived (strum / const-assert / shared crate).
5. **CAS is our incremental engine.** No Salsa retrofit — content-addressed
   caching at stage boundaries already gives the same reuse without the
   framework intrusion. Invest in key stability instead.

Phase 0 — Guardrails (do first; makes everything else safe & continuous)
------------------------------------------------------------------------

- Add `-W unreachable_pub -W unused` to the workspace rustflags in the Buck
  rust rules. Then sweep `pub` → `pub(crate)` on everything not consumed
  across crate boundaries — this is the zero-tooling way to make vanilla
  rustc report cross-crate dead code from now on (cargo-machete/udeps don't
  work under Buck2).
- Script `buck2 uquery "rdeps(//..., <target>)"` over all targets to flag
  orphan build targets (no built-in "find orphans" exists).
- These guards are what keeps Phase 1 from recurring.

Phase 1 — Pure deletions (no design work; ~1,300 LOC + a BUCK target)
---------------------------------------------------------------------

| Item | LOC | Note |
|---|---|---|
| `compiler/bin/ra_spike.rs` + `//workspace/compiler:ra-spike` | 313 | Superseded by `compile/rust/ra/`; removes 5 deps from build graph |
| `registry/catalog.rs` (`Catalog`, `PackageHandle`) | 170 | Superseded by postgres-backed `Store`; test-only |
| `server/save/{qdrant,tantivy,terminus}.rs` | 285 | Trivial rebuild/status wrappers; `resync_package_index` duplicates live `PackageSearchIndex::synchronize` |
| `runtime/vector/language.rs` `EmbeddingText` trait | ~20 | Zero impls, zero callers |
| `runtime/vector/model/catalog.rs` `Qwen3`, `OpenAi3Large` | 41 | Never selected in production |
| `CollectionName::for_regime` | ~10 | Server uses `::new`; migration never implemented |
| `registry/search/multi_parent.rs` `Alternate` | ~30 | Zero callers (keep `merge` — Phase 2 wires it) |
| `registry/metadata/hash.rs` `package_generation` + `freshness` | 39 | `BlobManifest::identity_bytes` is a strict superset and IS the live generation stamp; two hash definitions for one concept is a bug vector |
| `registry/metadata/guid.rs` `Minter` | 41 | Thin wrapper; the contract lives in `EntryUri::symbol_id` |
| `util/sandbox/backend/nix_derivation.rs` | ~50 | Unsound stub (no rlimits, synthesized exit); dies with the `Backend` trait in Phase 3 |
| Root `prelude` symlink | — | `.buckconfig` uses `build/prelude-local`; nothing references `root//prelude` |
| Speculative runtime surface: `Graph::{structure,members,implemented_by}`, `resolve_across`/`diff`, `similar_to`, `similar_to_snippet`, `similar_within`, `Snippet::extract` | ~420 | Test-only "eventual API" code. DELETE unless an endpoint lands in the next planning cycle — git preserves it, and the tests document the intended semantics for whoever revives it. If kept, each must name the route that will consume it. |

Keep: `assert_*_futures_send` (zero-cost compile-time `Send` guards) and
`cas/registry.rs` `RegistryCas` (explicit Phase-4 stub; Phase 3e makes it real).

Phase 2 — Salvage-then-wire (functionality gained, then husks deleted)
----------------------------------------------------------------------

**2a. Ranked package search (biggest functional win).** The live
`server/search/registry.rs::PackageSearchIndex::page` is bare BM25. The dead
`registry/search/ranking.rs` (718 LOC) holds the full lib.rs-ported pipeline:
BM25 × quality kink + exact/contains bonus + diversity pass + representative
pull-up + downloads bubble. Wire `fused_scores`/`Candidate` into
`PackageSearchIndex::page` (full pipeline on first pages only), wire
`multi_parent::merge` de-dup, then delete `RegistrySearch` (`search/mod.rs`,
203 LOC) — extracting its `collect_hits` de-dup logic first. This ends the
dual-search-stack split by making the live stack strictly better.
Strangler-fig: shadow-compare old vs new scoring on real queries for one
cycle before deleting.

**2b. Admin verify/rebuild endpoints.** `server/save/blobs.rs` (`verify_blobs`,
`rebuild_from_blobs`) and `save/mod.rs` (`rebuild`, `verify_reproducible`) are
production-grade and unreachable. Wire them behind `POST
/admin/packages/:id/{verify,rebuild}` — gated by the Phase 4a capability
extractor (they become the first real consumers of typed authz). The
orphan-sweep gap noted in `blobs.rs` (needs `Store::list()`) becomes the blob
GC work item (4f).

**2c. Metadata heuristics — decide, don't linger.** `metadata/heuristics.rs`
(792 LOC of `Specifics`/`Synonyms`) runs with `(None, None)` because no data
dir is ever passed (`server/coordination/indexing.rs:832` TODO). Either add a
`metadata_data_dir` config field and ship the data files this cycle, or delete
the module. No third option; "wired to None" is the worst state — full
maintenance cost, zero output.

Phase 3 — Unify duplicates (one abstraction each; better-typed than any copy)
-----------------------------------------------------------------------------

**3a. SymbolKind + all enum↔token maps → derive-generated, const-checked.**
Three byte-identical 8-arm parsers (`text/index.rs`, `graph/mod.rs`,
`registry/index/mod.rs`) become `#[derive(strum::EnumString, strum::Display,
strum::VariantNames)]` on `heart::SymbolKind`; callers use
`SymbolKind::from_str` and map `Err` to their local error type. Then go
further: generate the SQL CHECK arrays in `registry/schema/mod.rs` from
`VariantNames` so `STATE_VALUES`/`PHASE_VALUES`/`SINK_KIND_VALUES` and the
codec match arms cannot drift (`const _: () = assert!(...)` on lengths).
Close the verified gaps: add CHECK constraints for `symbols.kind` and
`packages.language` (today postgres accepts any string; the codec is the only
guard), and add `from_token` codecs for `owner_kind`/`visibility` (today
enforced by postgres but not decoded through a typed error).

**3b. One version resolver.** Four divergent copies (go/java/python
traversal + registry/resolve.rs) with genuinely different grammars (semver §11
vs Maven qualifier ranks vs PEP 440). Don't flatten the grammars — extract the
*shared shape* into a small `version` crate:

```rust
trait VersionGrammar { type V: Ord;
    fn parse_tag(&self, raw: &str, ctx: &TagContext) -> Option<Self::V>;
    fn is_prerelease(&self, v: &Self::V) -> bool; }
fn resolve_from_tags<G: VersionGrammar>(tags, req: &VersionRequest<G::V>, ctx, g) -> Option<G::V>
```

Go's `VersionRequest` enum (cleanest) + Java's multi-prefix tag stripping +
Python's delegation to `uv_pep440` become three ~50-line `VersionGrammar`
impls; the stable-before-prerelease selection loop exists once.
`registry::resolve::select` becomes the async entry point over the same enum.

**3c. One keyset paginator, snapshot policy in the type.** Text enforces
snapshot freshness; vector deliberately can't (ANN has no cheap content
hash) — and today that's an undetectable corrupt-page hazard buried in a
comment. Share the fetch-double/sort/filter loop once, and encode the policy
so it's visible at every call site:

```rust
struct Cursor<P: SnapshotPolicy> { key: (Score, SymbolId), snapshot: ContentHash, _p: PhantomData<P> }
enum Enforced {} enum Advisory {}   // text mints Cursor<Enforced>; vector Cursor<Advisory>
```

A `Cursor<Enforced>` is only constructible by a query that captured the live
snapshot (parse-don't-validate); the vector API's response type openly says
`Cursor<Advisory>` — the completeness caveat moves from a comment into the
signature.

**3d. Sandbox: delete `Backend`, keep `Cage`.** DAEMON-PLAN names `Cage` the
survivor and all production paths (`compiler/compile/isolate.rs:180`,
`server/forge.rs` `ForgeRuntime`) already use it. Sequence: (1) move
`WorkerPool` internals to `&dyn Cage`; (2) delete `sandbox::run`/`run_with` +
`global_backend()` OnceLock (also closes FORGE-PHASE4's last global);
(3) delete the `Backend` trait, `Capabilities` (keep `CageCaps`), both `From`
impls, and `SealedCommand::{into_spec,from_spec}` shims; (4) collapse
`type Captured = Output` to `Captured`. `nix_derivation.rs` dies here (Phase 1
listed it). While in the area: convert the panicking hot paths
(`worker.rs:236`, `budget.rs:102,109`, `limits.rs:182-201`) to typed
`CageError` variants.

**3e. CAS: one trait, registry as L3 — not a merge.** The `cas::Cas` trait
(get/put/put_keyed/invalidate, L1 stampede-cache → L2 disk → L3) is the
abstraction; `registry::Store` is a different concern (manifest *pointers*,
ranged reads, integrity re-verification) and should stay — but implement
`RegistryCas` over `Store::{put,get}_section` so the L3 tier stops being a
stub that `Tiered` silently swallows as a miss. That single impl makes
distributed CAS persistence real (today it effectively doesn't exist) and
ends the "two CAS" split: bytes go through `Cas`, package pointers through
`Store`.

**3f. One hash per meaning.** After deleting `package_generation` (Phase 1),
two intentionally distinct hashes remain: `identity_bytes` (generation stamp)
and the postcard manifest CAS key. Co-locate both in `registry/blob/` with a
doc comment stating they are deliberately different, and add a unit test
pinning each encoding so a refactor can't silently change either.

Phase 4 — Type-driven robustness (the "better-typed" half of more-for-less)
---------------------------------------------------------------------------

**4a. Authorization as a witness, not a no-op.** Replace
`authorize() -> Ok(())` (`server/lib.rs:314`) with capability tokens:

```rust
struct ReadCap { tenant: TenantId, _priv: () }   // private ctor, sealed module
struct WriteCap { ... } struct AdminCap { ... }
fn authorize_read(&self, p: &Principal, pkg: &PackageId) -> Result<ReadCap, Forbidden>
```

Every store/save/search method that touches tenant data takes the capability
as a parameter — handlers physically cannot reach data without one, and the
dead 403 path (`error.rs:199`) becomes reachable. At the HTTP layer, a
`FromRequestParts` extractor hierarchy (`Principal` → `AdminPrincipal`) makes
auth-by-construction: a handler that takes `AdminPrincipal` can't be routed
to without it. Policy can start as allow-all *inside* `authorize_read` — the
point is the plumbing exists and enforcement becomes a one-function change,
honestly documented in the README (done) until then.

**4b. Parse, don't panic, at every row boundary.**
- `registry/schema/codec.rs:96,110`: uuid `.expect()` → `CodecError::CorruptRow`
  (one corrupt row must fail the request, not the process).
- `registry/queue/mod.rs:300`: stop reconstructing `Job::state` as a stub —
  decode the persisted state (DeadLettered jobs are currently mislabeled).
- `heart/cursor.rs:54`: reachable `unreachable!` → typed error.
- Adopt `snafu` (or keep thiserror where single-source) at library-crate
  boundaries for multi-source variants; `miette` only in the server binary.

**4c. Secrets & config hygiene.** Wrap credentials in
`secrecy::Secret<String>` (redacted Debug, explicit `expose_secret()`).
Add a boot guard: `role: production` + default postgres URL
(`config.rs:311`) or default terminus password (`config.rs:387`) → refuse to
start. Delete the `https://example.invalid` origin fallback
(`server/http/dto.rs:109`): make origin required at the DTO boundary for
Go/Java packages — parse-don't-validate; a silent dead-end domain fails later
and worse.

**4d. Producer honesty.** Fix the TypeScript producer emitting placeholder
`SymbolKind` for every symbol (`compile/typescript/item.rs:200-292`) — after
3a this is one mapping table; and the Nix emitter's literal `<body>`
placeholders (`render/emit/nix.rs:157,184,192`). Both are silent wrong-output
bugs, worse than errors.

**4e. Typestate where lifecycle bugs live.** The `Job` typestate already
guards the live seal path — extend the same pattern
(`Job<Pending> → Job<Leased> → Job<Sealed>`, transitions consume `self`) to
the remaining queue paths that today re-check state at runtime, and to the
cursor minting in 3c. No new framework; it's the pattern the daemon already
chose.

**4f. Blob GC.** `server/poll.rs:302` is a permanent no-op and `blobs.rs`
documents the missing `Store::list()`. Implement `list()` on the object-store
key space, then the orphan sweep from 2b's salvaged `verify_blobs` machinery.
(CAS GC cron already exists for the daemon tier; this is the registry tier.)

Phase 5 — Keep it small (continuous enforcement)
------------------------------------------------

- `unreachable_pub` + `dead_code` warnings promoted to deny in CI once
  Phases 1-3 land.
- Const-asserts pin enum/CHECK/codec lockstep (3a); encoding tests pin the
  two hashes (3f).
- PLANS.md is the plan registry; new plan docs get an index row and a status,
  and superseded ones get a banner the day they're superseded.
- Periodic `pub(crate)` sweep + uquery orphan check (Phase 0 script) as a
  maintenance chore.

Sequencing
----------

```
Phase 0 (guardrails)          — ½ day, first
Phase 1 (pure deletion)       — 1 day, immediately after 0
2a ranked search              — 2-3 days   ┐ independent
2b admin endpoints            — 1-2 days   │ 2b consumes 4a's extractor;
2c heuristics decision        — decision   ┘ do 4a plumbing first or gate 2b on it
3a enums/tokens               — 1-2 days, unlocks 4d(TS kinds)
3b version crate              — 2-3 days
3c pagination                 — 1-2 days
3d sandbox collapse           — 2 days, also closes FORGE-PHASE4 last global
3e RegistryCas L3             — 1-2 days (DAEMON-PLAN Phase-4 alignment)
4a authz witness              — 2 days plumbing (policy later)
4b/4c parse+secrets           — 1-2 days
4d producers                  — 1-2 days
4e typestate extension        — opportunistic, with queue work
4f blob GC                    — 2 days, after 2b
```

Net effect: ≈3,000+ LOC deleted outright, two dual stacks (search, sandbox)
and four duplicate logic clusters collapsed to single well-typed
implementations, search quality *improved* (fused ranking replaces bare BM25),
distributed CAS persistence made real, authz made real-izable, and zero
`.expect()`/`unreachable!` on production data paths — strictly more capability
from strictly less code.

References
----------

- Meta SCARF dead-code deletion at scale — dl.acm.org/doi/10.1145/3611643.3613871
- "How much does unused code matter for maintenance" — IEEE TSE '18
- Parse, don't validate — lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/
- Witnesses / typestate — willcrichton.net/rust-api-type-patterns/
- GhostCell (ICFP '21) — plv.mpi-sws.org/rustbelt/ghostcell/
- cap-std — github.com/bytecodealliance/cap-std
- Sans-IO in Rust — firezone.dev/blog/sans-io
- strum / nutype / secrecy / snafu / la-arena — crates.io
- Error design at scale — greptime.com/blogs/2024-05-07-error-rust
- Salsa vs CAS tradeoff — rustc-dev-guide.rust-lang.org/queries/salsa.html
