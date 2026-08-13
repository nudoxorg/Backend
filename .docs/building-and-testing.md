# Building, testing, running

## Read this first

**The repository does not build from a clean checkout.** Seven files that
committed source `include_str!`s are absent from disk *and* untracked in git.
`include_str!` resolves at compile time, so this is a hard compile error in
`nudox-graph` and `index`, and it takes `nudox-engine`, `nudox-mcp`, and
`lindsey` down with them.

You will hit this within about ninety seconds of `cargo check --workspace`.
Full detail and root cause: [below](#the-repo-does-not-build-from-a-clean-checkout).

## TL;DR

Commands that are correct as written. Where a command was **not** executed
during writing, that is said explicitly.

```sh
# Enter the dev shell (Lix/Nix). Provides the fenix Rust toolchain, buck2,
# cargo-nextest, go, jdk21, dotnet SDK 10, and the formatters.
nix develop

# Inspect the workspace without compiling anything. VERIFIED — run during writing.
cargo metadata --no-deps --locked --offline

# Build / check / test. NOT verified during writing (no warm dependency cache;
# running one would have written target/ into the repo). Expect the missing-
# asset failure described below.
cargo check --workspace
cargo nextest run          # the authoritative test command — the pre-merge hook runs exactly this
cargo build -p driver --bin driver

# Run the server. Needs a reachable Qdrant or assembly aborts.
cargo run -p driver --bin driver

# Formatting and lints.
cargo fmt --all
cargo clippy --workspace
```

**Do not use the devShell's `build`, `check`, `run`, `build-release`, or
`run-release` commands.** They pass `--bin nudox`, and no such binary exists —
see [`MAIN_PACKAGE` names a binary that does not exist](#main_package-names-a-binary-that-does-not-exist).

## Which build system

**Cargo is authoritative. Buck2 is stale and, for at least three targets,
broken.**

| | Cargo | Buck2 |
| --- | --- | --- |
| Config | root `Cargo.toml` — a virtual manifest, 21 members | `BUCK`, `.buckconfig`, `.buckroot`, `build/rust.bzl`, `build/third-party/` |
| Covers | every live crate on both planes | `heart`, `ir`, `registry`, `compiler`, `sandbox` only |
| Last touched | `Cargo.toml` 2026-07-27 (`6eec3387`) | `BUCK` / `.buckconfig` 2026-07-15 (`25061c36`); `build/rust.bzl` 2026-07-15 (`0e3a3fde`) |
| Status | `cargo metadata --locked --offline` succeeds (verified) | `//:ir` and every `member("ir")` dangle |

### The Buck2 break, precisely

1. `build/rust.bzl:12-18` defines `_MEMBERS` with `"ir": "//workspace/ir:ir"`.
2. `workspace/ir/` was **deleted** on 2026-07-27 in `6eec3387`, twelve days
   after the Buck2 files were last touched.
3. Root `BUCK:2` still declares `alias(name = "ir", actual = "//workspace/ir:ir")`.
4. `workspace/compiler/BUCK:18` and `workspace/registry/BUCK:8` both call
   `member("ir")`.

Therefore `buck2 build //:ir`, `//workspace/compiler:compiler`, and
`//workspace/registry:registry` cannot resolve. `heart` and `sandbox` do not
depend on `ir` and may still build.

**Not confirmed by execution** — `buck2` is not installed outside the Nix
devShell, and no `buck2` command was run during writing. See
[OQ-10](open-questions.md#oq-10--which-buck2-targets-if-any-still-build).

Root `BUCK` also declares `test_suite(name = "smoke")` enumerating 54 concrete
`//workspace/{compiler,heart,registry,compiler/sandbox}:test-*` labels. It
carries a hand-maintenance note (`BUCK:15-19`) and is stale for the same reason.

The README's Buck2 alias list (`//:server`, `//:runtime`, `//:cas`,
`//:caching`, `README.md:82-84`) names targets that **do not exist**. Root
`BUCK` declares exactly five: `heart`, `ir`, `registry`, `compiler`, `sandbox`.

Third-party Buck2 dependencies are managed by reindeer
(`build/third-party/reindeer.toml`, driven by the `sync-deps` devShell
command).

## Prerequisites

The repo assumes **Lix** (a Nix implementation) and a dev shell. `README.md:228`
puts it directly: do not install compilers or runtimes globally — add them to
`flake.nix`.

`nix develop` provides (read from `flake.nix`): the fenix Rust toolchain,
`buck2` (via `buck2.nix`), `reindeer`, `go`, `jdk21_headless`,
`dotnetCorePackages.sdk_10_0`, `cargo-nextest`, `rust-analyzer`, `typos`,
`hongdown`, `nixfmt-rfc-style`, `taplo`, `tombi`, `b3sum`, `goreleaser`,
`cuelsp`, `nil`, `marksman`.

Environment set by the shell (`flake.nix:409-412`): `RUSTC_BOOTSTRAP=1`,
`MAIN_PACKAGE=nudox`, `OUTPUT_DIRECTORY=dist`, plus `OPENSSL_*` and `DOTNET_*`.

`RUSTC_BOOTSTRAP=1` is load-bearing: `workspace/driver/lib.rs:15` is
`#![feature(return_type_notation)]`, a nightly feature, and the crate is built
with a stable toolchain.

## Building

```sh
cargo build --workspace              # everything (will fail — see below)
cargo build -p driver --bin driver   # the server binary
cargo build -p index --bin ingest    # the (stub) ingestor binary
```

`[profile.release] lto = "thin"` (root `Cargo.toml`). Workspace lints:
`unused_imports = "warn"`, `dead_code = "warn"`; clippy's `result_large_err`
and `too_many_arguments` are allowed.

The workspace is `resolver = "3"`, `edition = "2024"`, version `0.1.0`, MIT.

`lindsey` (`workspace/gui`) is **not** a workspace member and has its own
lockfile. Build it from inside its own directory, not from the root.

## Testing

**`cargo nextest run` is the authoritative test command**, because it is
literally what the `testrust` pre-merge-commit hook runs (`flake.nix:285-292`,
`entry = "cargo nextest run"`).

There is **no CI in this repository.** No `.github/`, no `.gitlab-ci.yml`, no
`.woodpecker`, no `.drone.yml`, no `.forgejo` — verified by `ls`. The git hooks
configured in `flake.nix` are the only enforced definition of "correct".

`.config/pipeline.cue` looks like a pipeline and is not one: despite
`.config/concourse-schema.cue` sitting beside it, it is a **goreleaser**
project definition (`goreleaser.#Project`, `builder: "rust"`, `binary:
"nudox"`, `release: disable: true`). It also names the nonexistent `nudox`
binary, and it was last touched 2026-02-22.

### Git hooks (`flake.nix`, `checks.preCommitGitHooks` via `git-hooks.nix`)

`default_stages = ["pre-push"]` (`flake.nix:256`).

| Hook | Stage | Runs |
| --- | --- | --- |
| `convco` | pre-push | `convco check "$remote_sha..$local_sha"` — **Conventional Commits are enforced** (`flake.nix:258-266`) |
| `nixfmt` | pre-push | nix formatting (`flake.nix:270`) |
| `rustfmt` | pre-push | via the fenix toolchain (`flake.nix:271`) |
| `markdownfmt` | pre-push | `hongdown --write` on `*.md` (`flake.nix:278-284`) |
| `clippy` | pre-merge-commit, pre-push | via the fenix toolchain (`flake.nix:293-301`) |
| `testrust` | pre-merge-commit | **`cargo nextest run`** (`flake.nix:285-292`) |

Note `convco`: commit messages must be Conventional Commits or the push is
rejected.

### Integration test counts

From `cargo metadata --no-deps --locked --offline`, executed during writing:

`driver` 18 · `index` 8 · `nudox-engine` 9 · `heart` 7 · `nudox-graph` 6 ·
`nudox-mcp` 5 · `sandbox` 3 · `nudox-ir` 2 · `nudox-store` 2 ·
`nudox-producer-{csharp,go,typescript}` 1 each · `rusqdoltlite` 1 ·
`registry`, `ir-vcs`, `transport`, `nudox-producer`,
`nudox-producer-{clang,java,python,rust}` 0.

## Running locally

`cargo run -p driver --bin driver` with no configuration uses the built-in
localhost defaults (`workspace/driver/config.rs:369`). What must actually be up:

| Dependency | Required to boot? | What happens without it |
| --- | --- | --- |
| **Qdrant** at `http://127.0.0.1:6334` | **yes** | `ensure_collection` does a real round trip during `connect_source`; assembly aborts with `ConnectError(BackendKind::Qdrant)` (`workspace/driver/lib.rs:417-419`) |
| A writable `./data/catalog` | **yes** | the DoltLite catalog is created and migrated to v4 there |
| A writable `<tmpdir>/nudox/definitive/` | **yes** | `scratch.sqlite`, the package tantivy index |
| A writable `<tmpdir>/nudox/blobs/` | **yes** | the default `file://` object store; created automatically |
| **Embeddings endpoint** at `http://127.0.0.1:11434/v1/embeddings` | no | boot succeeds. The embedder is constructed, never probed. The first semantic query 503s. |
| A rerank endpoint | no | `POST /v1/rerank` answers `{"rerank_unavailable": true}` with status 200 |
| `NUDOX_GUEST_ROOTFS` + a provisioned golden image | only for ingest | every indexing job fails at the compile phase with `InternalError::CageCompile`. The HTTP surface still serves. |

Once it is up:

```sh
curl localhost:8080/healthz                       # 200, empty
curl localhost:8080/readyz                        # {"ready":true,"degraded":[]}
curl localhost:8080/metrics                       # prometheus text

curl -X POST localhost:8080/packages \
  -H 'content-type: application/json' \
  -d '{"ecosystem":"rust","name":"serde","version":"1.0.219"}'
# → {"package":"…uuid…","state":{"Unindexed":{"needed":true}},"enqueued":true}

curl -X POST localhost:8080/packages/search \
  -H 'content-type: application/json' \
  -d '{"target":"Packages","text":"serde"}'
```

**What silently returns empty:** `POST /search` in both modes, `GET
/symbols/:id` (always 404), `POST /expand` (always 404), and `POST /usages`
(always 503). None of these are configuration problems — they are unwired code
paths. See
[api-contract.md](api-contract.md#surfaces-that-return-empty) for the exact
line-level evidence, and [flows/03](flows/03-http-query.md) for why.

**For a production-shaped deployment**, set `data_directory`,
`catalog_directory`, and `object_store` explicitly. All three default under the
OS temp directory or the current working directory, so a stock deployment loses
its catalog on reboot. See
[flows/05-boot-and-config.md](flows/05-boot-and-config.md#configuration-reference).

## Formatting and lints

`rustfmt.toml` at the repo root: edition and style-edition 2024,
**`imports_granularity = "Crate"`**, `format_macro_matchers` and
`format_macro_bodies` on, `normalize_comments` on (but `normalize_doc_attributes`
off), `wrap_comments` on, `overflow_delimited_expr` on,
`use_field_init_shorthand` on, Unix newlines, lowercase hex literals, and
`color = "Never"`.

Note that `imports_granularity` and several of the others are **nightly-only**
rustfmt options, which is the other reason `RUSTC_BOOTSTRAP=1` is in the shell.

Files under `workspace/driver/` use **tabs**; `crates/nudox-*` uses **spaces**.
Both are rustfmt-clean; it is a per-tree convention.

Other tools: `typos` (spelling, part of the `check` script), `hongdown` for
markdown, `nixfmt-rfc-style` for Nix, `taplo`/`tombi` for TOML.

---

## Known-broken build paths

### The repo does not build from a clean checkout

**`.gitignore` is deny-by-default and has eaten source assets.**

`.gitignore:2` is `/**/*` — ignore everything — followed by an allowlist of `!`
patterns. The allowlist covers `*.rs`, `*.nix`, `*.md`, `*.snap`, `*.cue`,
`*.nu`, `*.patch`, producer fixtures (`*.go`, `*.java`, `*.ts`, `go.mod`, …),
and a handful of named JSON files. **It does not cover `*.graphql`,
`*.trustfall`, or `*.ron`.**

Consequence — these seven files are `include_str!`'d by committed source but
are **absent from disk and untracked in git**. Verified by `ls`, `git ls-files`,
and `git check-ignore -v`, all executed during writing:

| Missing file | `include_str!` site |
| --- | --- |
| `crates/nudox-graph/schema.graphql` | `crates/nudox-graph/src/lib.rs:31` and `:51` |
| `crates/nudox-graph/src/queries/find_symbol_by_key.trustfall` | `crates/nudox-graph/src/queries/mod.rs:23` |
| `crates/nudox-graph/src/queries/list_package_functions.trustfall` | `crates/nudox-graph/src/queries/mod.rs:27` |
| `crates/nudox-graph/src/queries/find_usages.trustfall` | `crates/nudox-graph/src/queries/mod.rs:31` |
| `crates/nudox-graph/src/queries/find_implementors.trustfall` | `crates/nudox-graph/src/queries/mod.rs:35` |
| `crates/nudox-graph/src/queries/symbols_mentioning_type.trustfall` | `crates/nudox-graph/src/queries/mod.rs:39` |
| `workspace/index/ecosystem/assets/cpp_alias_seed.ron` | `workspace/index/ecosystem/cpp/alias.rs:65` |

The evidence, verbatim:

```
$ ls crates/nudox-graph/src/queries/
mod.rs

$ ls crates/nudox-graph/
Cargo.toml  src  tests

$ ls workspace/index/ecosystem/assets/
ls: cannot access 'workspace/index/ecosystem/assets/': No such file or directory

$ git ls-files | grep -E '\.(graphql|trustfall|ron)$'
                                    # no output — none are tracked

$ git check-ignore -v crates/nudox-graph/schema.graphql
.gitignore:2:/**/*      crates/nudox-graph/schema.graphql
```

Four `nudox-graph` **test** files also `include_str!` `schema.graphql`
(`tests/error_channel.rs:16`, `tests/pushdown.rs:32`,
`tests/fixture_queries.rs:21`, `tests/schema_parses.rs:7`), and
`crates/nudox-mcp/tests/schema_source.rs` asserts `nudox_mcp::SCHEMA_SDL` is
byte-identical to it.

**`include_str!` resolves at compile time**, so this is not a runtime
degradation — `nudox-graph` and `index` fail to compile, and
`nudox-engine` → `nudox-mcp` → `lindsey` fail transitively. A new contributor
hits this on their first build.

The precedent is already recorded inside `.gitignore` itself
(`.gitignore:76-79`): a comment block explains that the Go and Java oracle
sources had to be explicitly un-ignored for exactly this reason. The same fix
shape applies here.

**Do not fix this as part of a documentation change.** It is recorded as
[OQ-3](open-questions.md#oq-3--are-the-missing-graphql--trustfall--ron-files-an-accident).

**Verification status:** the missing files and their ignore status were
confirmed by execution. A full `cargo check --workspace` was **not** run — this
machine has no warm dependency cache and the run would have written `target/`
into the repository. The compile failure is therefore inferred from
`include_str!`'s compile-time semantics, which are not in doubt, rather than
observed. Confirming it is
[OQ-4](open-questions.md#oq-4--does-cargo-check---workspace--cargo-nextest-run-pass-today).

### Three Nix packages do not evaluate

`workspace/default.nix:37` does `callPackage ./server/package.nix` and `:54`
does `callPackage ./server/image.nix`. **`workspace/server/` does not exist** —
it was dissolved into `driver`.

Verified by execution during writing:

```
$ nix eval --offline '.#packages.x86_64-linux.server.name'
error: getting status of '/nix/store/…-source/workspace/server/package.nix':
       No such file or directory

$ nix eval --offline '.#packages.x86_64-linux.backend.name'
error: getting status of '/nix/store/…-source/workspace/server/image.nix':
       No such file or directory

$ nix eval --offline '.#packages.x86_64-linux.default.name'
error: … workspace/server/package.nix: No such file or directory
```

`packages.default` is an alias of `server` (`workspace/default.nix:72`), so it
fails identically. `nix flake check` inherits the failure: its only integration
check, `backendImage` (`tests/default.nix:35`), takes `packages.server` and
`packages.backend` as inputs (`tests/default.nix:44-45`).

**There is currently no buildable Nix image for the `driver` binary.**

### `MAIN_PACKAGE` names a binary that does not exist

`flake.nix:411` sets `MAIN_PACKAGE = "nudox"`. The devShell commands `build`,
`check`, `build-release`, `run`, and `run-release` all pass
`--bin $env.MAIN_PACKAGE` to cargo:

```nu
# .config/scripts/check.nu
cargo check --workspace
cargo clippy --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
typos
```

```nu
# .config/scripts/build.nu
cargo build --workspace --bin $env.MAIN_PACKAGE --target $env.RUST_TARGET
```

**There is no `nudox` binary and no `nudox` workspace member.** The only two
binaries are `driver` and `ingest` (verified via `cargo metadata`). The root
`compiler/Cargo.toml` *is* named `nudox`, but it is dead and not a member.

So `check` fails at its second line (after `cargo check --workspace` runs), and
`build`/`run` fail immediately. `.config/scripts/test.nu` is unaffected — it is
plain `cargo test --workspace`.

**Not confirmed by execution** — running cargo would have written `target/`
into the repo. The chain (`MAIN_PACKAGE` → `--bin nudox` → no such bin) is read
directly from the two files above.

### The `ingest` binary is a stub

`workspace/index/ingest/main.rs:17` — `main` prints a message describing the
intended composition (open the DoltLite engine, wire a `reqwest` transport,
construct the followers and `GitMonitor`, drive them via `FollowerDriver`) and
exits cleanly. It wires nothing. It builds and links; that is its stated
purpose.

## Nix packages

`nix eval --offline '.#packages.x86_64-linux' --apply builtins.attrNames`
lists `[ backend clippy compiler-daemon compilerImage cxx default registry
ripgrep rustc server ]`. Attribute names evaluate lazily, so a successful
listing proves nothing about buildability.

Each was evaluated individually during writing:

| Attribute | Evaluates? | Result |
| --- | --- | --- |
| `server` | **no** | `workspace/server/package.nix`: No such file or directory |
| `default` | **no** | alias of `server`; identical failure |
| `backend` | **no** | `workspace/server/image.nix`: No such file or directory |
| `registry` | **no** | `error: program 'git' failed with exit code 128` — a git-context artifact of evaluating from this checkout, not necessarily a repo bug |
| `compiler-daemon` | yes | `nudox-compiler-0.1.0` |
| `compilerImage` | yes | `image-compiler-daemon.json` |
| `rustc`, `clippy`, `cxx`, `ripgrep` | not tested | Buck2 toolchain re-exports |

Evaluation is not building. None of these were `nix build`-ed.

`compilerImage` (`workspace/compiler/image.nix`) builds
`nudox/compiler-daemon:latest` with entrypoint `/bin/compiler-daemon` and env
`NUDOX_COMPILER_ADDR=0.0.0.0:8080`, `NUDOX_ROLE=forge`, `RUST_LOG=info`. It
belongs to the dead Buck2 `workspace/compiler/` tree — it is the one image that
still evaluates, and it is not the server.

## Devshell commands

Every one is a wrapper around `.config/scripts/<name>.nu`:

`build`, `build-release`, `check`, `clean`, `create-notes`, `doc`, `doc-open`,
`fmt`, `fmt-check`, `install`, `install-force`, `lint`, `lint-fix`, `patch`,
`release`, `run`, `run-release`, `sync-deps`, `test`, `test-with`, `test-all`,
`update`, `buck-build`, `buck-test`, `ra-index`, `snowydeer-import`,
`build-compiler-image`.

`build`, `check`, `build-release`, `run`, and `run-release` are broken by
`MAIN_PACKAGE`. `buck-build` and `buck-test` inherit the Buck2 breakage.

## Verification summary

| Claim | How verified |
| --- | --- |
| workspace members, targets, binary count, test counts | **executed** — `cargo metadata --no-deps --format-version 1 --locked --offline` |
| missing `*.graphql` / `*.trustfall` / `*.ron`; ignore rules | **executed** — `ls`, `git ls-files`, `git check-ignore -v` |
| Nix package evaluation results | **executed** — `nix eval --offline '.#packages.x86_64-linux.<pkg>.name'` for each |
| absence of CI config | **executed** — `ls .github .gitlab-ci.yml .woodpecker .drone.yml .forgejo` |
| `linkml` submodule declared but absent | **executed** — `cat .gitmodules`, `ls linkml` |
| commit dates for Buck2 files and the `workspace/ir` deletion | **executed** — `git log`, `git show --stat` |
| LOC per crate | **executed** — `find … -name '*.rs' \| xargs wc -l` |
| git hooks, rustfmt settings, devshell contents, script contents | **read** from `flake.nix`, `rustfmt.toml`, `.config/scripts/*.nu` |
| Buck2 `member("ir")` breakage | **read** from `BUCK`, `build/rust.bzl`, `workspace/*/BUCK` |
| `cargo check` / `cargo build` / `cargo nextest run` outcomes | **not run** — no warm dependency cache, and a run would have written `target/` into the repository |
| any `buck2` command | **not run** — buck2 is not installed outside the devShell |
| any `nix build` | **not run** |
