# Phase 4 — ForgeRuntime: Implementation Plan

Post phases 0–3 merge (HEAD ~`1d92e6d`). Replaces compile-plane globals + env
reads with an owned, injected `ForgeRuntime`. `run_producer` becomes the sole
cache client.

## 1. Globals to KILL (11 policy globals)

| # | Global | file:line | Type | Injected by today |
|---|---|---|---|---|
| 1 | `BACKEND` | `util/sandbox/lib.rs:88` | `OnceLock<Selected>` | `select()` on first `sandbox::run` |
| 2 | `OBSERVER` | `util/sandbox/observer.rs:78` | `OnceLock<Arc<dyn SandboxObserver>>` | `observer::install()` (dead) |
| 3 | `OVERRIDES` | `util/sandbox/overrides.rs:12` | `OnceLock<RwLock<HashMap<String,LimitOverride>>>` | `server/main.rs:47 install_limit_overrides` |
| 4 | `ToolchainPaths T` | `compiler/compile/isolate.rs:136` | `OnceLock<ToolchainPaths>` | `from_env()` (reads NUDOX_TOOLCHAIN_*/RUSTUP_HOME @ :87-99) |
| 5 | nix `POOL` | `compiler/compile/isolate.rs:438` | `OnceLock<Option<WorkerPool>>` | worker bin env, warmed @ main.rs:49 |
| 6 | parser `POOL` | `compiler/compile/isolate.rs:447` | `OnceLock<Option<WorkerPool>>` | same |
| 7 | parse `CAS` | `compiler/generate/parse_cache.rs:24` | `OnceLock<Option<Tiered>>` | `default_open()` + NUDOX_PARSE_CACHE_DISABLE @ :27 |
| 8 | fallback `RT` | `compiler/generate/parse_cache.rs:56` | `OnceLock<tokio Runtime>` | sync→async bridge |
| 9 | rustdoc `WRAPPER` | `compiler/compile/rust/package.rs:245` | `OnceLock<PathBuf>` | REMOVED w/ rustdoc path (P3) |
| 10 | `CUSTOM_REGISTRIES` | `server/http/dto.rs:120` | `LazyLock<ArcSwap<..>>` | server-plane (defer/move onto Server) |
| 11 | `PROMETHEUS` | `server/http/handlers/health.rs:19` | `OnceLock<PrometheusHandle>` | server-plane (defer) |

KEEP (pure process derivations, not policy): seccomp `PATH` `cage.rs:398`,
`NEVER` `cancel.rs:34`.

## 2. std::env reads in compiler → must reach ZERO
producer/mod.rs:264 (ToolchainPaths::from_env), :268/270/273 (PATH/LANG/LC_ALL);
isolate.rs:90, :325/328/331, :394/419; parse_cache.rs:27; rust/package.rs:327
(NUDOX_RUSTDOC_SYSTEM — gone w/ rustdoc); rust/ra/mod.rs:47 (NUDOX_RUST_PRODUCER — removed P3).
Allowed to remain: cas/disk.rs:40 (move to server config), server::config, cage::seal/probe.

## 3. Location
- `ForgeRuntime` struct + `assemble` typestate → NEW `workspace/server/forge.rs` (server already deps heart/registry/runtime/compiler/sandbox+cas; no new BUCK edges).
- `ForgeContext` trait + `run_producer` → NEW `workspace/compiler/compile/producer/runtime.rs`.
- `NodeId`, `ToolchainSet`, typed `OverrideTable`+`SandboxKey`, `ForgeObserver` → `sandbox` (cage vocabulary).

## 4. Types (see agent plan for full shapes)
- sandbox: `NodeId` (node.rs), `ToolchainSet` (toolchains.rs, absorbs ToolchainPaths::from_env, `.digest()->ContentHash`), rewrite overrides.rs → `SandboxKey{Profile(ProducerProfile),Package(id)}` + `OverrideTable{from_config,resolve,empty}`, observer.rs add `ForgeObserver` trait (cache_hit/miss, producer_finished/killed, worker_restart) + delete static OBSERVER/install/global. `SealedInput` gains `pub key: JobKey`.
- compiler: `ForgeContext { node,cage,cas,toolchains,overrides,observer }`.
- server: `ForgeRuntime<Cold|Ready>{node,policy,cage:Arc<dyn Cage>,cas:Arc<Tiered>,toolchains,overrides,observer}`, `assemble(policy,cfg)->Result<Ready,_>` w/ `select_cage(policy)` (Production→LinuxNamespaces asserting production_grade else Err; Development→DevPassthrough::try_new). impl ForgeContext for Ready.

## 5. run_producer (sole cache client)
key=input.key.as_hash(); cas.get→hit: observer.cache_hit + decode(postcard); miss: observer.cache_miss + execute_plan(cage.run per SealedCommand w/ CancelToken, library→injected worker) → postcard → cas.put_keyed → observer.producer_finished. block_on bridge via injected tokio Handle.

## 6. Ordered steps
1. sandbox: node.rs, toolchains.rs, rewrite overrides.rs, ForgeObserver in observer.rs, SealedInput.key.
2. compiler: producer/runtime.rs (ForgeContext+run_producer+execute_plan); rewrite producer/mod.rs seal_package(&ToolchainSet) + execute→cage; rewrite isolate.rs delete T/POOLs/env, IsolatedCommand→SealedCommand; parse_cache.rs delete CAS/RT/get/put keep key/hash_source_tree/hash_dep_lock (move to seal); generate/mod.rs cache_get_or_build uses ctx.cas; cas/disk.rs drop env.
3. server: forge.rs (ForgeRuntime+assemble+select_cage+ForgeConfig+errors); config.rs resolved Policy+node+cas root; lib.rs Server.forge + assemble builds it; main.rs delete boot_check/install_limit_overrides/warm_workers; indexing.rs compile_package threads Arc<ForgeRuntime>→run_producer.
4. tests: FakeCage; two-ForgeRuntimes-coexist unit test (separate Tiered::memory_only + NodeId, put in A not visible in B); clippy.toml disallow std::env::var* in compiler.

Gate: SERVER_TEST_BACKENDS=1 integration + escape.rs + compiler offline + coexistence test green; `rg 'std::env' workspace/compiler` == 0.

## 7. surface.rs call-site
`generate::surface::{collect,build,run_producer}` take `ctx:&dyn ForgeContext` threaded from indexing.rs compile_package. Per-arm becomes `seal_package(ctx.toolchains(),root,profile,ctx.overrides())` then `ctx.run_producer(&p, sealed)`.

## 8. Risks
- SandboxKey::Package needs identity in sandbox w/o registry dep → heart-level PackageId or SmolStr triple.
- sync→async bridge: pass tokio Handle into ForgeRuntime, keep block_on, delete RT OnceLock.
- ForgeObserver vs SandboxObserver rename touches CountingObserver tests; events only at run_producer boundary.
