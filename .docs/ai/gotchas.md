# Gotchas

Things that fail silently or actively mislead. Ordered by how quickly they will
waste your time.

Verified against `main` = `b388f36d`, 2026-08-13.

---

## The build is broken right now

**Seven files that committed source `include_str!`s are absent from disk AND
untracked in git.** `include_str!` resolves at compile time, so `nudox-graph`
and `index` do not compile, and `nudox-engine` → `nudox-mcp` → `lindsey` fail
with them.

| Missing file | `include_str!` site |
| --- | --- |
| `crates/nudox-graph/schema.graphql` | `crates/nudox-graph/src/lib.rs:31` and `:51` |
| `crates/nudox-graph/src/queries/find_symbol_by_key.trustfall` | `crates/nudox-graph/src/queries/mod.rs:23` |
| `crates/nudox-graph/src/queries/list_package_functions.trustfall` | `…/mod.rs:27` |
| `crates/nudox-graph/src/queries/find_usages.trustfall` | `…/mod.rs:31` |
| `crates/nudox-graph/src/queries/find_implementors.trustfall` | `…/mod.rs:35` |
| `crates/nudox-graph/src/queries/symbols_mentioning_type.trustfall` | `…/mod.rs:39` |
| `workspace/index/ecosystem/assets/cpp_alias_seed.ron` | `workspace/index/ecosystem/cpp/alias.rs:65` |

**Root cause: `.gitignore` is deny-by-default.** `.gitignore:2` is `/**/*`,
followed by an allowlist of `!` patterns covering `*.rs`, `*.nix`, `*.md`,
`*.snap`, `*.cue`, `*.nu`, `*.patch`, producer fixtures, and named JSON files.
It does **not** cover `*.graphql`, `*.trustfall`, or `*.ron`.

```
$ git check-ignore -v crates/nudox-graph/schema.graphql
.gitignore:2:/**/*      crates/nudox-graph/schema.graphql

$ ls workspace/index/ecosystem/assets/
ls: cannot access '…': No such file or directory
```

**What this means for you:**

- **Any new non-`.rs` asset you create will be silently ignored by git.** You
  will `git add` it, see nothing staged, and commit a build that works on your
  machine and nowhere else. Check `git check-ignore -v <path>` before assuming
  a file is tracked.
- `.gitignore:76-79` records this exact failure happening before, for the Go
  and Java oracle sources: "fresh checkouts / git worktrees build an empty
  target". Same shape, second occurrence.
- Do not "fix" this as a side effect of another change — it is
  [OQ-3](../open-questions.md#oq-3--are-the-missing-graphql--trustfall--ron-files-an-accident),
  and whether the files should be regenerated or recovered is unresolved.

Detail: [../building-and-testing.md](../building-and-testing.md#the-repo-does-not-build-from-a-clean-checkout).

## `README.md` is fiction

The root `README.md` is substantially wrong. **Do not read it for facts and do
not "modernize" it** — write from source.

| It claims | Reality |
| --- | --- |
| "Built with Buck2. No Cargo workspace." (`README.md:11`) | a 21-member Cargo workspace exists and is authoritative |
| graph store is TerminusDB (`:6`, `:27`) | no graph store; TerminusDB removed |
| global index is Postgres (`:27`, `:41`) | DoltLite, a versioned SQLite opened from a directory |
| crates `runtime`, `cas`, `util/caching`, `util/sandbox`, `server`, `compiler/intermediate-representation` (`:41-45`) | none exist |
| producers include Nix (`:5`, `:19`) | no `crates/nudox-producer-nix`; the seventh crate is `clang` |
| Buck2 aliases `//:server //:runtime //:cas //:caching` (`:82-84`) | root `BUCK` declares only `heart`, `ir`, `registry`, `compiler`, `sandbox` |
| config keys `terminus*` (`:129-132`) | replaced by `instance_organization` / `instance_database` (now only an id salt) and `catalog_directory` |
| "All routes are defined in `workspace/server/http/router.rs`" (`:164`) | `workspace/server/` does not exist; it is `workspace/driver/http/router.rs` |
| `/search` returns JSON (`:170`) | it returns NDJSON |

It is right about two things: CAS blob GC is unimplemented (`:204`) and
`/search/semantic` needs Qdrant plus embeddings (`:213`).

The ~18 root `*-PLAN.md` / `*-NOTES.md` files are **intent, not description**.
Several describe systems that were built and then removed. Their section
markers (INDEX-PLAN §9, SMOLVM-PLAN §5.1, LR-6, ID-8, §L0) are cited throughout
the source and are genuinely useful for *why* a decision was made — just never
for *what exists*.

## No symbol query returns anything

`POST /search` answers `200` with a well-formed, **empty** NDJSON body — in
**both** modes. That is indistinguishable from "your query matched nothing",
and it is not a configuration problem.

| Surface | Actual behaviour | Cause |
| --- | --- | --- |
| `POST /search`, `mode: "precise"` (the default) | `200`, zero lines | `SearchTarget::search for SourceStores` is `Ok(futures::stream::empty())` (`workspace/driver/search/mod.rs:117`) — the replica-local tantivy *symbol* index was removed |
| `POST /search`, `mode: "semantic"` and `/search/semantic` | `200`, zero lines | Qdrant is queried **successfully** and returns scored identities; then hydration calls `SourceStores::symbol_by_id`, which returns `Ok(None)` unconditionally (`workspace/driver/search/mod.rs:96-98`), so every hit is dropped (`workspace/driver/coordination/search.rs:149`) |
| `GET /symbols/:id` | **`404`, always** | same `symbol_by_id` (`coordination/search.rs:37-48`) |
| `POST /expand` | **`404`, always** | resolves the seed through `symbol_by_id` before it reaches the neighbour walk (`handlers/search.rs:125`) |
| `POST /usages` | **`503`, always** | `Driver::assemble` builds `ReverseIndexUsageBackend::empty()` (`workspace/driver/lib.rs:339`); an empty backend answers `IndexUnavailable` (`workspace/index/search/usages.rs:178-179`) |

**`POST /packages/search` is the only search surface that returns data.** The
write plane, `/healthz`, `/readyz`, and `/metrics` all work.

The gap is exactly one function wide: `workspace/driver/search/mod.rs:96`.
Do not chase a ranking bug here. Detail:
[../flows/03](../flows/03-http-query.md).

## Buck2 targets that cannot resolve

`build/rust.bzl:12-18` maps `"ir" → "//workspace/ir:ir"`. `workspace/ir/` was
deleted 2026-07-27 (`6eec3387`), twelve days after the Buck2 files were last
touched. Root `BUCK:2` still aliases it; `workspace/compiler/BUCK:18` and
`workspace/registry/BUCK:8` both call `member("ir")`.

So `buck2 build //:ir`, `//workspace/compiler:compiler`, and
`//workspace/registry:registry` cannot resolve. The `test_suite(name =
"smoke")` in root `BUCK` enumerates 54 hand-maintained labels and is stale for
the same reason.

**Cargo is authoritative.** A `BUCK` file is not evidence about the dependency
graph. Which targets *do* still build is
[OQ-10](../open-questions.md#oq-10--which-buck2-targets-if-any-still-build) —
no `buck2` command was executed during writing.

## `workspace/compiler/` is a previous generation

Large, thoroughly commented, and describes a seven-language producer pipeline
in convincing detail. **It is not what runs.** Not a Cargo member; reachable
only through the broken Buck2 path.

If you find yourself reading tree-sitter extraction, renderers, or the
`producer-worker` / `compiler-daemon` binaries, you are in the wrong tree. The
live producer contract is `crates/nudox-producer*`; the *running* producers on
the server plane are guest binaries inside OCI images this repo does not build
([OQ-14](../open-questions.md#oq-14--what-builds-the-guest-producer-binaries)).

`workspace/compiler/sandbox/` is the exception — it **is** a live Cargo member.

Also dead: `compiler/` and `ir/` at the repo root, and `workspace/server/`
(deleted, still referenced by `workspace/default.nix:37`).

## The devShell `build` commands are broken

`flake.nix:411` sets `MAIN_PACKAGE = "nudox"`, and `build`, `check`,
`build-release`, `run`, `run-release` all pass `--bin $env.MAIN_PACKAGE`:

```nu
# .config/scripts/build.nu
cargo build --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
```

**There is no `nudox` binary and no `nudox` workspace member.** The only two
are `driver` and `ingest`. `check` fails at its second line (after `cargo check
--workspace` succeeds); `build`/`run` fail immediately.

Use plain cargo: `cargo check --workspace`, `cargo nextest run`,
`cargo build -p driver --bin driver`. `.config/scripts/test.nu` is fine — it is
plain `cargo test --workspace`.

## The `ingest` binary does nothing

`workspace/index/ingest/main.rs:17` — `main` prints a message describing the
intended composition and exits. It wires no engine, no transport, and no
watermark store. It is one of only two binaries in the workspace.

## Two different enums are both called `SinkKind`

| Type | Variants | Role |
| --- | --- | --- |
| `index::coordination::SinkKind` — a **re-export of `heart::DerivedStore`** (`workspace/index/coordination/outbox.rs:78`; enum at `workspace/heart/sink.rs:37`) | `Vector`, `Graph`, `Text` | the serving-side vocabulary; what `driver` matches on |
| `index::enums::SinkKind` (`workspace/index/enums.rs:180`) | `Text`, `Vector`, `UsageIndex` | the catalog `TEXT`-column codec |

Bridged by `catalog_sink`/`serving_sink`
(`workspace/index/coordination/outbox.rs:84-97`); the mapping is
`Graph ↔ UsageIndex`. `Graph` is a real, persisted sink whose stored token is
`"usage_index"` — not a phantom variant. Its fan-out is currently a no-op
(`workspace/driver/poll.rs:199`, `:218`).

## Two different enums are both called `Language`

`heart::Language` (`workspace/heart/ecosystem.rs:35`) has **8** variants and
folds C and C++ into `Cpp`. `nudox_ir::body::Language`
(`crates/nudox-ir/src/body.rs:38`) has **10** — it splits `C` from `Cpp` and
adds `Other`.

The split is deliberate: `nudox-ir` carries its own minimal enum so it need not
depend on `heart` (`crates/nudox-producer/src/lib.rs:285-288`). There is no
`From` impl bridging them; a conversion is a match you write by hand, and
`heart::Language::Cpp` is ambiguous across the boundary.

## `registry::` inside `driver` is not the `registry` crate

`workspace/driver/lib.rs:35` declares a crate-local `pub mod registry` that
**shadows** the extern-prelude `registry` crate. Bare `registry::…` and
`crate::registry::…` inside `driver` both resolve to that facade. The real
crate is imported as `xregistry` (`workspace/driver/Cargo.toml:36`) and
reachable as `::registry`. There is a second facade `pub mod vector` at
`lib.rs:78`.

Symptom: you jump-to-definition on `registry::search::usages::Usage` and land
in `workspace/index/`, not `workspace/registry/`. That is correct — the facade
re-projects `registry::{blob, cas, catalog, coordination, queue, search, …}`
onto `::index::*` (`lib.rs:39-70`).

Also `pub type Server<M> = Driver<M>` (`lib.rs:135`) — two names, one type.

## Comments that contradict the code

The repo is mid-migration. When a comment and the code disagree, the code wins.

| Location | Says | Reality |
| --- | --- | --- |
| `workspace/driver/poll.rs:79-83` | the outbox sink is claimed via "a postgres advisory lock so exactly one replica drains it" | it is an in-process `tokio::sync::Mutex` (`workspace/index/coordination/outbox.rs:277`). Two processes will both drain. And there is no Postgres in this repo. |
| `workspace/driver/coordination/mod.rs:3` | "the durability model is **postgres-as-WAL**" | DoltLite, a versioned SQLite |
| `workspace/driver/config.rs:451-454` | `validate()` rejects default/TerminusDB credentials in production | **not implemented.** `validate()` never reads `self.deployment`; `DefaultCredentialInProduction` (`config.rs:663`) has zero constructors. Setting `deployment = "production"` changes nothing. |
| `workspace/driver/config.rs:60` | `compiler_endpoint` defaults to `:9090` | `defaults::compiler_endpoint()` returns `http://127.0.0.1:8080` (`config.rs:588`) — and nothing reads the field at all |
| `workspace/driver/main.rs:43-45` | `assemble` constructs a `CompilerClient` | no `CompilerClient` type exists anywhere. IR is produced in an ephemeral `SmolvmCage` per job. |
| `workspace/driver/main.rs:30`, `workspace/driver/http/router.rs:80` | "See `workspace/telemetry`" | no such crate. It is `heart::telemetry`. |
| `workspace/driver/coordination/indexing.rs:57-59`, `:646-647` | "the compile phase is stubbed" | it is not — it runs the cage for real (`indexing.rs:179-250`) |
| `workspace/driver/http/handlers/search.rs:88-89` | `/usages` returns a typed `501` | it returns `503`; the `501` comes from a backend `driver` never constructs |
| `crates/nudox-engine/src/lib.rs:13` | the §L0 law is "enforced by `scripts/lint-gui-no-block.sh`" | that script does not exist; there is no `scripts/` directory |
| `crates/nudox-producer/src/lib.rs:5` | "Seven language frontends (… Nix)" | no Nix producer crate; the seventh is `clang` |
| `crates/nudox-producer-go/src/producer.rs:1-30` | the trait crate is "being authored … in parallel", behind `#[cfg(feature = "producer-trait")]` guards | `nudox-producer` landed; the Go crate never adopted it and the `cfg` guards are gone from the body |
| root `Cargo.toml:10` vs `:36` | "sandbox was dropped" / "sandbox rejoins the cargo workspace" | both were true in sequence. Read that file's header as a changelog. |

## Silent-degradation paths

Places where something goes wrong and nothing tells you.

| Path | What happens |
| --- | --- |
| A producer emits a malformed or aborted IR stream | **the job succeeds** with an empty IR section. Only a `tracing::warn!` marks it (`workspace/driver/coordination/indexing.rs:1058`, doc at `:1041-1046`). The package reaches `Stored`. |
| `$NUDOX_CONFIG` names a file that does not exist | **silently ignored.** `resolve()` only merges the file layer when `path.is_file()` (`workspace/driver/config.rs:432`); boot proceeds on defaults. |
| `mirror.follow` names a language with no follower | logged and skipped, not a boot error (`workspace/driver/lib.rs:565-568`). Only `CSharp` and `Rust` have followers. |
| `mirror.follow` names an unparseable string | logged and skipped (`workspace/driver/lib.rs:555`) |
| Semantic quota exhausted (64 per 60 s) | **silently degrades to the precise arm**, which returns empty (`workspace/driver/search/planner.rs:56-58`) |
| Embeddings endpoint unreachable at boot | boot **succeeds** — the embedder is constructed, never probed (`workspace/driver/lib.rs:278-281`). Surfaces as a 503 on the first semantic query. |
| Listing-signal / download-count fetch fails during emit | non-fatal; facets stay `None`, the job succeeds (`workspace/driver/coordination/indexing.rs:268`, `:284`) |
| `POST /v1/compiled/lookup` backend error on one key | **downgraded to a miss**, status 200 (`workspace/driver/http/handlers/compiled.rs`, the `Err` arm) |
| `POST /v1/rerank` timeout or transport failure | `200 {"rerank_unavailable": true}` — check the field, not the status |
| `nudox-producer-python` built without the `pyrefly` feature | `invoke` returns an empty oracle and `produce` succeeds with an empty package (`crates/nudox-producer-python/src/producer.rs:53-57`). "Found nothing" and "compiled without its oracle" are indistinguishable. |
| Golden VM fork unsupported on the host | **not a failure** — cold-boots a fresh cage instead (`workspace/driver/coordination/indexing.rs:927-934`) |

## Config keys that are read by nothing

Resolved, defaulted, documented — and dead. Grep before you trust one.

| Key | Evidence |
| --- | --- |
| `compiler_endpoint` | `grep -rn compiler_endpoint --include=*.rs` → only `config.rs` and a stale comment at `main.rs:44` |
| `endpoints.qdrant_collection` | only `config.rs`. The live collection name comes from `CollectionConfig::for_model::<M>()` (`workspace/driver/lib.rs:416`). |
| `deployment` | never read outside `config.rs` — see the boot-guard row above |

Also dead: `ConfigValidationError::{MissingRequiredEndpoint, DimensionOutOfRange,
InvalidUrl}` (`config.rs:634`, `:638`, `:647`) are declared and never
constructed. And `Driver::reranker()` returns `None` unconditionally
(`workspace/driver/lib.rs:490-494`) — the handler builds its own client.

## `/admin/*` is not protected

The `AdminPrincipal` extractor admits every request (`workspace/driver/authz.rs:87`),
and `read_allowed`/`write_allowed`/`admin_allowed` all return `true`
(`authz.rs:167`, `:172`, `:177`). A caller sends **nothing** today — no token,
no header.

The deny arm exists and projects to `403` (`workspace/driver/error.rs:328`) but
nothing constructs it in a serving path. `workspace/driver/main.rs:47-48` states
the intent: enforcement happens at a fronting proxy.

**Do not expose this binary to an untrusted network.**
`POST /admin/packages/:id/rebuild` is as open as `GET /healthz`.

## Two Trustfall versions in one workspace

The local-first plane uses a git fork pinned by rev (root `Cargo.toml:116`),
because upstream 0.8.1 lacks a real `Adapter::Error` channel (LR-6) and the
`AsyncAdapter` stream engine — whose result stream is `!Send` and therefore
drives the engine's `LocalSet`. `workspace/registry` pins
`trustfall = "=0.8.1"` from crates.io (`workspace/registry/Cargo.toml:36`).

Likewise two axum majors: `driver` pins `0.7` (hence the `:id` path syntax),
`nudox-mcp` pins `0.8`.

## Defaults that write to the temp directory

`data_directory` defaults to `<OS tmpdir>/nudox/<source_name>/`
(`workspace/driver/config.rs:405-409`), `object_store` to
`file://<OS tmpdir>/nudox/blobs` (`config.rs:567`), and `catalog_directory` to
the relative `./data/catalog` (`config.rs:505`).

A stock deployment loses its catalog, its scratch store, and its blobs on
reboot. Set all three explicitly for anything but a scratch run.

Note also that `nudox.toml` at the repo root would itself be git-ignored — the
allowlist covers no `*.toml` except specific `Cargo.toml` paths.

## Three Nix packages do not evaluate

`server`, `default`, and `backend` all fail on the deleted `workspace/server/`
(`workspace/default.nix:37`, `:54`). Verified by executing
`nix eval --offline '.#packages.x86_64-linux.<pkg>.name'` for each.
`packages.registry` fails differently (`program 'git' failed with exit code
128`). Only `compiler-daemon` and `compilerImage` evaluate — both belong to the
dead Buck2 tree.

**There is no buildable Nix image for the `driver` binary.**

## There is no CI

No `.github/`, `.gitlab-ci.yml`, `.woodpecker`, `.drone.yml`, or `.forgejo`.
The `flake.nix` git hooks are the only enforced definition of correct:
`convco` + `nixfmt` + `rustfmt` + `markdownfmt` + `clippy` on pre-push,
`clippy` + `cargo nextest run` on pre-merge-commit.

`.config/pipeline.cue` looks like a CI pipeline and is a **goreleaser** project
definition. It names the nonexistent `nudox` binary and was last touched
2026-02-22.

## The Backend↔Web contract is desynchronized

Nudox/Web's `docs/` app calls `GET /api/packages`, `GET /search?q=`, and
`GET /terminus_search`. This repo serves no `/api` prefix, `/search` is a POST
returning NDJSON, and `terminus_search` appears in **zero** Rust source files.

**Which side is stale is unresolved.** Do not "fix" either side on the
assumption that the other is authoritative. Evidence for both:
[../api-contract.md](../api-contract.md#current-desynchronization-with-nudoxweb),
question:
[OQ-1](../open-questions.md#oq-1--which-side-of-the-backendweb-contract-is-stale).
