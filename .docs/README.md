# Nudox Backend documentation

Nudox Backend compiles packages from eight language ecosystems into a shared
intermediate representation, indexes that IR, and serves it — over an HTTP API
and over MCP. A caller names a package by ecosystem, name, and version; the
system acquires it from its upstream registry, extracts it under a sanitizing
allowlist, compiles it to IR inside an ephemeral microVM, stores the result
content-addressed, and fans it out to a vector index and a full-text index. On
the other side, queries arrive as a single query algebra and leave as NDJSON or
paged JSON.

**Before you read anything else, know two things.** First, **there are two
planes, not one.** `workspace/*` is the hosted server plane; `crates/nudox-*`
is the local-first plane behind the desktop GUI and the MCP server. They share
exactly one crate (`nudox-ir`) and neither supersedes the other. A reader who
assumes a single pipeline will spend a day looking for a connection that does
not exist. Second, **this repository is mid-migration and parts of it are
rubble.** TerminusDB and Postgres were removed; the symbol text index was
removed; the compiler daemon was removed; `workspace/server` and `workspace/ir`
were deleted. Comments, plan files, and the README describe states one or two
refactors old. Where a document and the code disagree, the code wins — and
where this documentation found such a disagreement, it records it rather than
smoothing it over.

## Status of this documentation

Written against `main` at **`b388f36d`** on **2026-08-13**, working tree clean.

Every non-obvious claim carries a `path/to/file.rs:line` citation, derived at
write time. Where a fact came from executing a command, the document says so;
where it did not, it says that instead — see
[building-and-testing.md's verification summary](building-and-testing.md#verification-summary).

Line numbers move. Treat a citation as "this was here at `b388f36d`" and
re-grep if it does not match.

The ~18 root-level `*-PLAN.md` and `*-NOTES.md` files are **intent, not
description**. Their section markers (INDEX-PLAN §9, SMOLVM-PLAN §5.1, LR-6,
ID-8, §L0) are cited throughout the source and are genuinely useful for
understanding *why* — but several describe systems that were built and then
removed. `README.md` at the repo root is substantially wrong; see
[architecture.md](architecture.md#the-readme-is-fiction) for a claim-by-claim
table.

## Reading order

| If you are… | Read |
| --- | --- |
| new to the repo | [architecture.md](architecture.md) → [flows/03](flows/03-http-query.md) → [flows/01](flows/01-package-ingest.md) |
| building or testing | [building-and-testing.md](building-and-testing.md) — start at the top, it opens with the reason your build will fail |
| integrating a client | [api-contract.md](api-contract.md), then its [surfaces that return empty](api-contract.md#surfaces-that-return-empty) section |
| adding a language producer | [flows/02](flows/02-producer-to-ir.md) |
| operating a deployment | [flows/05](flows/05-boot-and-config.md), then [OQ-2](open-questions.md#oq-2--how-is-the-backend-actually-deployed) |
| working on the GUI / MCP plane | [flows/04](flows/04-local-first-mcp.md) |
| trying to work out why a query returns nothing | [flows/03](flows/03-http-query.md) |

## Document index

| Document | What it covers |
| --- | --- |
| [architecture.md](architecture.md) | The two planes, the crate map with LOC and roles, the data stores, identity and hashing, federation, the `registry` facade inside `driver`, what is dead, and a dated migration changelog |
| [building-and-testing.md](building-and-testing.md) | Cargo vs Buck2, the dev shell, the authoritative test command, the git hooks (there is no CI), running locally, and every known-broken build path with its exact error |
| [api-contract.md](api-contract.md) | **The canonical wire contract.** Every route, request type, response type, content type, error shape, and body limit; the query algebra with its exact serde spelling; NDJSON framing; what breaks in Web if each surface changes; and the open Backend↔Web discrepancy |
| [flows/01-package-ingest.md](flows/01-package-ingest.md) | `POST /packages` → queue → four phases → SmolvmCage → NdIrF1 → blob → outbox → Qdrant/Tantivy. The phase state machine, idempotency, and the two `SinkKind` enums |
| [flows/02-producer-to-ir.md](flows/02-producer-to-ir.md) | The `Producer` trait verbatim, `produce()`, a worked implementor, the `ProducerId` version-bump rule, and the two disjoint producer planes |
| [flows/03-http-query.md](flows/03-http-query.md) | `POST /search` → NDJSON, the planner and its semantic budget, both arms, and exactly which hop is dead |
| [flows/04-local-first-mcp.md](flows/04-local-first-mcp.md) | `McpEndpoint::start` → six tools → `EngineHandle` → Trustfall → `Corpus` → `nudox-ir`. The §L0 dependency law and the standing decisions LR-1…LR-12 |
| [flows/05-boot-and-config.md](flows/05-boot-and-config.md) | `main` → figment layering → `validate()` → `assemble` → `serve`. Every config key with its default and env var, `Role` gating, shutdown, and the stale comments in this area |
| [open-questions.md](open-questions.md) | OQ-1 … OQ-16. What could not be determined, why it matters, and what would resolve it |

## Known-broken things

Hit these on page one rather than page forty.

| What | Impact | Where |
| --- | --- | --- |
| **The repo does not build from a clean checkout.** Seven `include_str!`'d assets are absent from disk and untracked, because `.gitignore` is deny-by-default and its allowlist omits `*.graphql`, `*.trustfall`, and `*.ron`. `nudox-graph` and `index` fail to compile; `nudox-engine`, `nudox-mcp`, and `lindsey` fall with them. | blocks everything on the local-first plane | [building-and-testing.md](building-and-testing.md#the-repo-does-not-build-from-a-clean-checkout) · [OQ-3](open-questions.md#oq-3--are-the-missing-graphql--trustfall--ron-files-an-accident) |
| **No symbol query returns anything.** `POST /search` is empty in *both* modes, `GET /symbols/:id` is always 404, `POST /expand` is always 404, `POST /usages` is always 503. The precise arm is `stream::empty()`; the semantic arm queries Qdrant successfully and then drops every hit because `symbol_by_id` returns `Ok(None)`. | the primary read surface answers nothing | [api-contract.md](api-contract.md#surfaces-that-return-empty) · [flows/03](flows/03-http-query.md) |
| **`POST /packages/search` is the only search surface that returns data.** | | [flows/03](flows/03-http-query.md#what-still-works-on-the-read-plane) |
| **The Backend↔Web contract is desynchronized, and which side is stale is unresolved.** Web calls `GET /api/packages`, `GET /search?q=`, and `GET /terminus_search`; this repo serves no `/api` prefix, `/search` is a POST, and `terminus_search` appears in zero Rust files. | Web's `docs/` app cannot talk to this backend as built | [api-contract.md](api-contract.md#current-desynchronization-with-nudoxweb) · [OQ-1](open-questions.md#oq-1--which-side-of-the-backendweb-contract-is-stale) |
| **`/admin/*` is not protected.** The `AdminPrincipal` extractor admits every request; every authorization predicate returns `true`. Do not expose this binary to an untrusted network. | | [api-contract.md](api-contract.md#authentication) |
| **The production credential boot guard is documented but not implemented.** Setting `deployment = "production"` changes nothing — `validate()` never reads the field. | an operator gets a guarantee that does not exist | [flows/05](flows/05-boot-and-config.md#what-is-a-stub) · [OQ-16](open-questions.md#oq-16--was-the-production-credential-boot-guard-removed-or-never-written) |
| **Three Nix packages do not evaluate** — `server`, `default`, `backend` — because `workspace/default.nix` still references the deleted `workspace/server/`. There is no buildable image for `driver`. | | [building-and-testing.md](building-and-testing.md#three-nix-packages-do-not-evaluate) |
| **The devShell's `build` / `check` / `run` commands are broken.** They pass `--bin nudox`; no such binary exists. | | [building-and-testing.md](building-and-testing.md#main_package-names-a-binary-that-does-not-exist) |
| **Buck2 is stale.** `//:ir`, `//workspace/compiler:compiler`, and `//workspace/registry:registry` cannot resolve — `workspace/ir/` was deleted twelve days after the Buck2 files were last touched. Cargo is authoritative. | | [building-and-testing.md](building-and-testing.md#the-buck2-break-precisely) · [OQ-10](open-questions.md#oq-10--which-buck2-targets-if-any-still-build) |
| **The outbox sink lock is in-process only**, despite a comment claiming a cross-replica advisory lock. Two `driver` processes will both drain the same sink. | idempotent, so wasteful rather than corrupting | [flows/01](flows/01-package-ingest.md#the-sink-lock-is-in-process-only) · [OQ-12](open-questions.md#oq-12--is-single-replica-outbox-drain-a-requirement) |
| **A producer that emits a malformed or aborted IR stream still succeeds**, with an empty IR section. | the one silent-degradation path in ingest | [flows/01](flows/01-package-ingest.md#failure-modes) |
| **The `ingest` binary is a stub** that prints a message and exits. One of only two binaries in the workspace. | | [building-and-testing.md](building-and-testing.md#the-ingest-binary-is-a-stub) |
| **There is no CI.** The `flake.nix` git hooks are the only enforced definition of correct. | | [building-and-testing.md](building-and-testing.md#git-hooks-flakenix-checksprecommitgithooks-via-git-hooksnix) |

## What is NOT documented here

- **`workspace/compiler/`** (except `sandbox/`) — the Buck2-only, unbuildable
  previous generation of the producer pipeline. It is large and convincing and
  it is not what runs. See
  [architecture.md](architecture.md#dead-and-vestigial-code) and
  [OQ-5](open-questions.md#oq-5--is-workspacecompiler-being-revived-or-scheduled-for-deletion).
- **`compiler/` and `ir/` at the repo root** — dead, no build path.
- **`lindsey` (`workspace/gui`)** beyond its place in the dependency law. The
  GPUI app is 28 k lines and its internals were out of scope for this pass.
- **`ir-vcs` internals** — the libpijul-backed patch engine, diff, and semver
  machinery. It appears here only where the ingest flow touches its NdIrF1
  stream decoder.
- **`index` internals below the module level** — schema v4's tables, the
  ObjectPack container format, the ecosystem grammars. 45 k lines; this pass
  documented its role and its seams, not its interior.
- **The IR data model itself** (`nudox-ir`) — entries, kinds, views, the change
  model. It is the shared bottom of both planes and deserves its own document.
- **Deployment topology** — hosts, delivery, systemd vs container. It lives in
  a separate repository; see
  [OQ-2](open-questions.md#oq-2--how-is-the-backend-actually-deployed).

## A note on vocabulary

Use the code's names; readers grep. `Driver`, `Lowering`, `SmolvmCage`,
`SinkKind`, `heart::query::Query`, `PristineIntroTable`, `ReadCap`,
`SourceStores`, `Federation`.

One alias to know up front: **`pub type Server<M> = Driver<M>`**
(`workspace/driver/lib.rs:135`). `Driver` is the composition struct; `Server`
is what the moved modules call it. Both appear freely in the source and they
are the same type.
