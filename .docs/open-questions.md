# Open questions

Each entry is something that could not be determined from the code at write
time. **None of these are guesses.** Where another document says "unknown", it
links here.

Numbers are stable across regenerations — other documents link to them. A
question that gets resolved keeps its number and is marked **RESOLVED** with
the answer and the date, rather than being renumbered and reused.

Written against `main` at `b388f36d`, 2026-08-13.

---

## OQ-1 — Which side of the Backend/Web contract is stale?

**Question:** Is Nudox/Web's `docs/` app written against a stale version of
this backend, does it target a separate legacy service, or does an adapter
exist outside both repositories?

**Why it matters:** [api-contract.md](api-contract.md) is the canonical
contract and Nudox/Web's documentation links to it. If Web is stale, the fix is
in Web. If Web targets a live legacy service, this backend's contract is *not*
the one Web's users are hitting, and calling it canonical is misleading. If an
adapter exists, both documents are describing halves of a three-part system and
neither is complete. The three answers imply three different pieces of work.

**What we know:**

- Web's `docs/src/lib/server/backend.ts` calls `GET /api/packages` (`:87`),
  `GET /api/packages/{id}` with a **numeric** id (`:96`), `POST /api/packages`
  with `{language, name, version, source?, entry_point?, branch?, sync_on_add?}`
  (`:100`), `GET /search?q=…&limit=…` (`:110`), and
  `GET /terminus_search?q=…` (`:114`).
- This repo serves **no `/api` prefix at all**, has **no package-list route**,
  takes **UUIDs** not numbers, spells the add-package body
  `{ecosystem, name, version, origin?}`, serves `/search` as a **POST** taking
  a `heart::query::Query` and returning NDJSON, and contains
  **`terminus_search` in zero Rust source files** — `grep -rn 'terminus_search'
  --include=*.rs` returns 0 matches. The string appears only in four stale
  planning markdown files.
- Web's default backend URL is a hard-coded production IP,
  `http://87.99.136.215:3001`
  (`docs/src/lib/server/terminus-config.ts:36-41`), with the same port
  suggested in `docs/.env.example`. This backend defaults to
  `127.0.0.1:8080`.
- Web also talks TerminusDB GraphQL directly at `{serverUrl}/api/graphql/…`,
  port 6363, with basic auth (`terminus-config.ts:28`), and carries
  `@graphql-codegen/*` devDependencies. This backend has no GraphQL surface and
  no TerminusDB.
- The port and IP being non-default and concrete is consistent with **all
  three** readings — a stale client still pointed at a still-running old
  service would look exactly like this.

**What would resolve it:** ask the maintainer directly. Failing that: find out
what, if anything, serves `/api/packages` on `87.99.136.215:3001`, and whether
Web's `docs/` app is currently deployed at all.

**Blocked on:** a maintainer, or access to that host.

---

## OQ-2 — How is the Backend actually deployed?

**Question:** What hosts run `driver`, how are images delivered, does it run as
a NixOS systemd service or a container, and where does production `nudox.toml`
come from?

**Why it matters:** every configuration default in this repo is a localhost dev
default, including three that write to the OS temp directory. A reader
following [flows/05](flows/05-boot-and-config.md) can configure a correct
deployment, but cannot tell what the *actual* deployment does — and therefore
cannot tell whether a config key is dead in general or merely dead here.

**What we know:**

- `flake.nix` takes an input `nixos.url =
  "git+https://dev.nudox.org/git/Nudox/MachineConfigurations.git"`, locked in
  `flake.lock` to rev `4741259a241fbca430b2410078cb9a0dbde3a28b` (revCount 428,
  `refs/heads/main`). **The machine and service definitions live in that
  separate repository, which is not present in this working tree.**
- This repo produces OCI images with `nix2container` via `mkServiceImage`
  (`build/nix/lib.nix:72`), default port `8080/tcp`.
- `packages.backend` — the one that would build a `driver` image — **does not
  evaluate** (see
  [building-and-testing.md](building-and-testing.md#three-nix-packages-do-not-evaluate)).
  The only image that evaluates is `compilerImage`, for the dead Buck2
  compiler-daemon tree.
- `workspace/driver/http/router.rs:78-88` names a concrete observability stack:
  Prometheus exposition scraped by **VictoriaMetrics** as `job="backend"`,
  **vmalert** rules, and a **nixos-side Grafana dashboard** — all defined
  outside this repo. The metric and label names are described there as "the
  contract" those dashboards query.
- `build/nix/snowydeer/` is a Buck2 BXL integration that imports Buck2-built
  binaries into the Nix store (`snowydeer-import`, `NUDOX_*_PATH` env vars
  threaded through `flake.nix` with `--impure`).

**What would resolve it:** read `Nudox/MachineConfigurations` at
`https://dev.nudox.org/git/Nudox/MachineConfigurations.git`, rev
`4741259a241fbca430b2410078cb9a0dbde3a28b`.

**Blocked on:** access to that repository.

---

## OQ-3 — Are the missing `*.graphql` / `*.trustfall` / `*.ron` files an accident?

**Question:** Were the seven `include_str!`'d assets lost to the deny-by-default
`.gitignore`, or do they exist in someone's working tree or on another branch?

**Why it matters:** it determines whether the fix is "regenerate them" or
"un-ignore and commit them", and it is the single highest-value thing to
resolve — the repository does not build without it, and every new contributor
hits it immediately.

**What we know:**

- `.gitignore:2` is `/**/*`. The allowlist covers `*.rs`, `*.nix`, `*.md`,
  `*.snap`, `*.cue`, `*.nu`, `*.patch`, producer fixtures, and named JSON
  files — **not** `*.graphql`, `*.trustfall`, or `*.ron`.
- All seven files are absent from disk *and* untracked. `git ls-files | grep -E
  '\.(graphql|trustfall|ron)$'` returns nothing; `git check-ignore -v
  crates/nudox-graph/schema.graphql` returns `.gitignore:2:/**/*`. Both
  executed during writing.
- `.gitignore:76-79` records the exact same failure happening before, for the
  Go and Java oracle sources, with a comment explaining why they had to be
  un-ignored: "fresh checkouts / git worktrees build an empty target". This
  looks like a recurrence of a known problem, not a novel one.
- `crates/nudox-mcp/tests/schema_source.rs` asserts `nudox_mcp::SCHEMA_SDL` is
  byte-identical to `crates/nudox-graph/schema.graphql` — so a *reconstructed*
  schema would still need to match whatever the engine expects.

**What would resolve it:** `git log --all --diff-filter=A -- '*.graphql'
'*.trustfall' '*.ron'` to see whether they were ever committed on any branch;
then ask the maintainer whether a working copy exists. Then `cargo check -p
nudox-graph`.

**Blocked on:** a maintainer, or another checkout.

---

## OQ-4 — Does `cargo check --workspace` / `cargo nextest run` pass today?

**Question:** Beyond the missing-asset failure, does the workspace compile and
do the tests pass on a fresh clone?

**Why it matters:** the missing assets are a *known* failure. What is not known
is whether they are the *only* failure, or the first of several. Documenting
"it builds once you add seven files" is a claim this documentation cannot
currently make.

**What we know:** `cargo metadata --no-deps --locked --offline` succeeds, so
the manifest graph and the lockfile are consistent. No compilation was
attempted: this machine has no warm dependency cache, and a build would have
written `target/` into the repository, which the writing brief forbade.

**What would resolve it:** run `cargo check --workspace` and `cargo nextest
run` with a warm cache, ideally with `CARGO_TARGET_DIR` pointed outside the
repo. Do it after OQ-3 is resolved, or the answer is just OQ-3 again.

**Blocked on:** a machine with a warm dependency cache; OQ-3.

---

## OQ-5 — Is `workspace/compiler/` being revived, or scheduled for deletion?

**Question:** `workspace/compiler/` (excluding `sandbox/`) holds a
seven-language oracle pipeline, tree-sitter extraction, renderers, and the
`producer-worker` / `compiler-daemon` binaries — with no Cargo path to any of
it and a broken Buck2 path. Is it coming back or going away?

**Why it matters:** it is the largest trap in the repository. It is big,
well-commented, and describes the producer pipeline in convincing detail — and
a reader who studies it learns a previous generation of the system. If it is
scheduled for deletion, this documentation should say so; if it is being
revived, the two producer planes are about to become three.

**What we know:**

- Not a Cargo workspace member. Its `BUCK` file calls `member("ir")` →
  `//workspace/ir:ir`, deleted 2026-07-27.
- `workspace/ir/` was deleted with the message "the migration is complete",
  which is precedent for how this repo removes a superseded tree.
- `packages.compiler-daemon` and `packages.compilerImage` are the **only two**
  Nix packages that still evaluate — someone is keeping that path alive.
- The guest producer binaries the live ingest pipeline execs may well be built
  from here; see OQ-14.

**What would resolve it:** ask the maintainer. The `compilerImage` evaluation
success and OQ-14 are the same thread.

**Blocked on:** a maintainer.

---

## OQ-6 — RESOLVED (2026-08-13): what does `POST /usages` actually return?

**Question:** 501, 503, or a page?

**Answer: always `503`.** `Driver::assemble` constructs
`ReverseIndexUsageBackend::empty()` (`workspace/driver/lib.rs:339`), and an
empty backend returns `UsageQueryError::IndexUnavailable` for every query
(`workspace/index/search/usages.rs:178-179`), which projects to
`503 SERVICE_UNAVAILABLE` (`workspace/driver/error.rs:346`).

The 501 the handler's doc comment mentions
(`workspace/driver/http/handlers/search.rs:88-89`) comes from
`UsageQueryError::UnsupportedTarget`, which only the `Unsupported` backend
produces (`workspace/index/search/usages.rs:95-102`) — and `driver` never
constructs that backend. The handler's doc comment is stale.

A malformed `StableReference` in `target.of` gives `400`
(`UnresolvableTarget` → `error.rs:347`).

The distinction is deliberate and worth preserving: an empty `Page` from this
route would mean "this symbol genuinely has no uses", which is different from
"this package's IR is not materialized here" (`usages.rs:119-128`). Once a
scope is loaded via `ReverseIndexUsageBackend::loaded(view, reverse)`
(`usages.rs:159`), the route serves live results with no other change.

---

## OQ-7 — RESOLVED (2026-08-13): do the two `SinkKind` enums differ?

**Question:** `index::enums::SinkKind` declares `Text`/`Vector`/`UsageIndex`,
but `driver` matches on a `Graph` variant. Is `Graph` real?

**Answer: yes, and there are genuinely two types.**

- `index::coordination::SinkKind` is a **re-export of `heart::DerivedStore`**
  (`workspace/index/coordination/outbox.rs:78`; the enum itself at
  `workspace/heart/sink.rs:37`), with variants `Vector`, `Graph`, `Text`. This
  is the serving-side vocabulary, and it is what `driver` imports
  (`workspace/driver/poll.rs:17`) and what `serve()` iterates
  (`workspace/driver/lib.rs:537`).
- `index::enums::SinkKind` (`workspace/index/enums.rs:180`) is the catalog
  `TEXT`-column codec for `outbox.sink_kind` / `sink_watermarks.sink_kind`,
  with variants `Text`, `Vector`, `UsageIndex`.
- They are bridged by `catalog_sink` and `serving_sink`
  (`workspace/index/coordination/outbox.rs:84-97`). The mapping is
  `Graph ↔ UsageIndex`.

So `Graph` is a real, persisted sink whose stored token is `"usage_index"`. Its
fan-out is currently a no-op (`workspace/driver/poll.rs:199`, `:218`).

---

## OQ-8 — Is Radicle still used for hosting alongside Forgejo?

**Question:** `README.md:247` says "We use Radicle for hosting. After `rad
auth`:". Is that still true?

**Why it matters:** it affects contribution instructions, and it is one of the
few README claims this documentation could not simply disprove.

**What we know:** `.gitmodules` points at `github.com` for `linkml`, and the
flake inputs point at Forgejo (`dev.nudox.org`). There is a
`.config/scripts/rad-sync.nu` script in the devShell script directory, which is
weak evidence that Radicle is at least still wired. `git remote -v` was not
checked during writing.

**What would resolve it:** ask the maintainer; check `git remote -v` and
whether a Radicle remote is configured.

**Blocked on:** a maintainer.

---

## OQ-9 — Why is the `linkml` submodule declared but absent?

**Question:** `.gitmodules` declares `linkml` → `https://github.com/philocalyst/linkml`
at path `linkml`, and `.gitignore:131` explicitly un-ignores `/linkml` with the
comment "TODO: Remove when upstreamed" (`.gitignore:130`). But `ls linkml` → no
such file. Is it still needed?

**Why it matters:** a declared-but-absent submodule makes `git submodule
update --init` behave surprisingly and makes it unclear whether a fresh clone
is complete.

**What we know:** the declaration and the `.gitignore` exemption both exist;
the directory does not. Both verified during writing. Nothing in the Rust
source references `linkml`.

**What would resolve it:** `git submodule status`; ask the maintainer whether
the LinkML dependency was dropped.

**Blocked on:** a maintainer.

---

## OQ-10 — Which Buck2 targets, if any, still build?

**Question:** `//:ir`, `//workspace/compiler:compiler`, and
`//workspace/registry:registry` provably cannot resolve. Can `//workspace/heart:heart`
and `//workspace/compiler/sandbox:sandbox`?

**Why it matters:** it decides whether Buck2 is "partly stale" or "entirely
dead", and therefore whether the `BUCK` files are worth maintaining at all.

**What we know:** the dangling `member("ir")` chain is read directly from
`build/rust.bzl:12-18`, root `BUCK:2`, `workspace/compiler/BUCK:18`, and
`workspace/registry/BUCK:8`. `heart` and `sandbox` do not depend on `ir`.
**No `buck2` command was executed** — buck2 is not installed outside the Nix
devShell.

**What would resolve it:** `buck2 build //...` inside `nix develop`.

**Blocked on:** nothing but running it.

---

## OQ-11 — What is meant to start the MCP endpoint?

**Question:** `McpEndpoint::start` is called from
`crates/nudox-mcp/tests/endpoint.rs` and nowhere else. No binary in this
repository starts the MCP server. What is supposed to?

**Why it matters:** [flows/04](flows/04-local-first-mcp.md) documents a surface
that cannot currently be reached by running anything in this repo. A reader who
wants to point a coding agent at their corpus has no command to run.

**What we know:**

- `crates/nudox-mcp/src/lib.rs:1` says "the MCP server `lindsey` hosts
  (GUI-LOCAL-PLAN §L6)", and the quick-start example
  (`lib.rs:24-31`) shows `endpoint.url()` going to a "status bar" and
  `client_config_snippet()` to "Settings → Connection" — clearly a GUI host.
- **`workspace/gui/Cargo.toml` does not depend on `nudox-mcp`.** Its only
  backend dependency is `nudox-engine`.
- `grep -rn 'McpEndpoint' --include=*.rs .` outside the crate itself finds only
  its own tests.

The most economical reading is that the `lindsey` side of §L6 has not landed
yet, but that is an inference and this document does not assert it.

**What would resolve it:** ask the maintainer whether `lindsey` is meant to
gain a `nudox-mcp` dependency, or whether a separate MCP binary is planned.

**Blocked on:** a maintainer.

---

## OQ-12 — Is single-replica outbox drain a requirement?

**Question:** `workspace/driver/poll.rs:79-83` says the outbox sink is claimed
via "a postgres advisory lock so exactly one replica drains it". The actual
implementation is an in-process `tokio::sync::Mutex`
(`workspace/index/coordination/outbox.rs:277`), which excludes concurrent tasks
in one process and nothing at all across processes. Was cross-replica exclusion
a requirement that regressed, or was the comment always aspirational?

**Why it matters:** two `driver` processes pointed at the same catalog will
both drain the same sink. Because every sink write is idempotent and the
watermark advance is monotone, this is wasteful rather than corrupting — but
"wasteful" means duplicate embedding calls against a paid or rate-limited
embeddings endpoint, per replica, forever. And if a future sink write is *not*
idempotent, the comment will have promised a guarantee that never existed.

**What we know:**

- `try_lock_sink` is `Arc::clone(&self.sink_guards[slot]).try_lock_owned()`
  over a `tokio::sync::Mutex<()>` held in the `Outbox` struct. `SinkLockGuard`
  wraps an `OwnedMutexGuard`; `release()` is a no-op that exists "for call-site
  clarity" (`outbox.rs:313-316`).
- There is no Postgres anywhere in this repo — the comment names a database
  that was removed.
- The `Role` split exists specifically to support horizontal scaling
  (`workspace/driver/config.rs:122-124`, "DAEMON-PLAN §Phase 5"), so
  multi-replica is an intended deployment shape.

**What would resolve it:** ask the maintainer whether multi-replica gateway
nodes are deployed. Related to OQ-2.

**Blocked on:** a maintainer, or `Nudox/MachineConfigurations`.

---

## OQ-13 — Are the Go, Java, and C# producers meant to implement `Producer`?

**Question:** Only four of the seven producer crates implement the `Producer`
trait. Go, Java, and C# do not, and do not even depend on `nudox-producer`. Is
that migration pending, abandoned, or deliberate?

**Why it matters:** [flows/02](flows/02-producer-to-ir.md) documents `produce()`
as "the contract every language frontend implements", which is what the crate
says about itself — and it is currently true of four of seven. Anyone adding an
eighth language needs to know which pattern to follow.

**What we know:**

- `grep -rn 'impl Producer for' --include=*.rs crates/` returns exactly five
  hits: `nudox-producer-clang`, `-rust`, `-python`, `-typescript`, plus two
  test fakes in `nudox-producer` itself.
- `crates/nudox-producer-{go,java,csharp}/Cargo.toml` carry no `nudox-producer`
  dependency.
- `crates/nudox-producer-go/src/producer.rs:1-30` still describes the trait
  crate as "being authored by the prodcore agent in parallel" and mentions
  `#[cfg(feature = "producer-trait")]` guards — but no such `cfg` appears in
  the file's current body. `GoProducer` exposes `lower_bytes` and
  `invoke_oracle` instead.
- The Go doc comment also points at `workspace/compiler/compile/go/oracle/` for
  the oracle binary — the dead Buck2 tree. Related to OQ-5 and OQ-14.

**What would resolve it:** ask the maintainer. The Go module doc reads like a
migration that stalled, but that is an inference.

**Blocked on:** a maintainer.

---

## OQ-14 — What builds the guest producer binaries?

**Question:** The ingest pipeline execs binaries at
`/opt/nudox/<lang>/bin/nudox-<lang>-producer` inside a golden OCI image
(`workspace/driver/coordination/indexing.rs:719-813`). What builds those
binaries and those images?

**Why it matters:** this is the actual production code path for IR generation.
Without knowing what produces those binaries, nobody can tell whether a change
to `crates/nudox-producer-rust` affects a running deployment, or whether the
two producer planes are drifting.

**What we know:**

- `indexing.rs:685-687` says explicitly: "The golden toolchain OCI images
  (built by the Buck2 compiler tree) fulfill this contract by shipping the
  producer binary at `entrypoint`… the content of the image is a deployment
  concern, not a code concern."
- The Buck2 compiler tree is `workspace/compiler/`, which cannot build (OQ-5,
  and [building-and-testing.md](building-and-testing.md#the-buck2-break-precisely)).
- Two signals that the guest binaries are **not** these crates: the table has a
  `Language::Nix` arm naming `/opt/nudox/nix/bin/nudox-nix-producer`
  (`indexing.rs:775`) and there is no `crates/nudox-producer-nix`; and the C/C++
  arm calls its producer a tree-sitter static parse (`indexing.rs:803`) while
  `crates/nudox-producer-clang` is a libclang oracle.
- `RootfsStore::from_env()` reads `NUDOX_GUEST_ROOTFS` (`indexing.rs:914`), so
  the images are provisioned out-of-band on each forge node.

**What would resolve it:** ask the maintainer; inspect a provisioned forge node
or the image build in `Nudox/MachineConfigurations`. Related to OQ-2 and OQ-5.

**Blocked on:** a maintainer, or a forge node.

---

## OQ-15 — What populates the local-first corpus?

**Question:** `EngineHandle::resolve_project` and `EngineHandle::sync` are both
marked stubs (`crates/nudox-engine/src/lib.rs:41-43`). What path is meant to
load a project's IR into the `Corpus` that `nudox-graph` queries?

**Why it matters:** [flows/04](flows/04-local-first-mcp.md) traces queries
*out* of the corpus. It does not trace anything *into* it, because no non-test
path was found. An MCP tool over an empty corpus answers nothing, the same way
`POST /search` does.

**What we know:**

- `Corpus::insert` (`crates/nudox-store/src/corpus.rs:78`) exists and is the
  obvious entry.
- `nudox-store`'s `ProducerSource`
  (`crates/nudox-store/src/source/producer.rs:85`) drives
  `nudox_producer::produce`, so there *is* a producer → corpus path in the
  crate.
- `nudox-engine` has a `fixtures` feature, which `lindsey` enables
  (`workspace/gui/Cargo.toml:37`) — suggesting the GUI currently runs on
  fixture data.
- `resolve_project` and `sync` — the two calls that would wire a real project —
  are stubs.

**What would resolve it:** read `crates/nudox-engine/src/runtime.rs` and
`crates/nudox-store/src/source/` end to end and follow `Corpus::insert`'s
callers; then confirm with the maintainer what the intended `lindsey` startup
path is.

**Blocked on:** nothing but the reading — this one is answerable from the
source with more time than this pass had.

---

## OQ-16 — Was the production credential boot guard removed, or never written?

**Question:** `ServerConfiguration::validate`'s doc comment
(`workspace/driver/config.rs:451-454`) promises a production boot guard on
default credentials. The function body does not implement it, and
`ConfigValidationError::DefaultCredentialInProduction` (`config.rs:663`) has
zero constructors. Was this removed with TerminusDB, or was it never written?

**Why it matters:** an operator reading that doc comment — or the config field's
name — reasonably believes that `deployment = "production"` makes the server
refuse to boot with development defaults. **It does not.** Setting
`deployment = "production"` today changes nothing at all; `self.deployment` is
never read outside `config.rs`. The guard's whole point was to prevent exactly
the mistake its absence now permits.

**What we know:**

- `validate()` (`config.rs:455-480`) checks only source-name uniqueness and the
  rerank license deny-list. It never reads `self.deployment`.
- `grep -rn 'DefaultCredentialInProduction' --include=*.rs .` returns one hit:
  the variant declaration.
- `grep -rn 'deployment' --include=*.rs workspace/ crates/` finds no read of
  `ServerConfiguration::deployment` outside `config.rs`.
- The doc comment specifically mentions a "TerminusDB password", which no
  longer exists as a config key — so the guard predates the TerminusDB removal
  and plausibly went with it.

**What would resolve it:** `git log -S DefaultCredentialInProduction` to see
whether a body ever existed; then decide with the maintainer whether to restore
it or delete the field, the variant, and the comment.

**Blocked on:** nothing but the git archaeology, then a maintainer decision.
