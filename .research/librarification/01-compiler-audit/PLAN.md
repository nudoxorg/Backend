# Compiler Audit: Librarification Analysis

**Date:** 2026-07-16 (re-verified against live tree)  
**Scope:** `workspace/compiler` (live tree) · nested `workspace/compiler/sandbox` · `workspace/heart`  
**Out of scope:** root-level `compiler/` (legacy) · building / fixing compile failures  
**Purpose:** Feed the dual-form librarification restructure: **stateless sealed server binary** (untrusted k8s fleet) + **trusted-code embedded library** (desktop / local registry)

> **Path note:** There is no top-level `workspace/sandbox`. The sandbox crate lives at
> `workspace/compiler/sandbox/` (Buck member `sandbox`, also has its own `Cargo.toml`).
> All sandbox citations below use that path.

---

## 0. Mission Checklist Coverage

| # | Question | Section |
|---|---|---|
| 1 | Full pipeline map (stages, types, serialization) | §1 |
| 2 | State inventory (embed vs sealed-server blockers) | §2 |
| 3 | Daemon / protocol: implemented vs stubbed | §3 |
| 4 | Producer-by-producer (in-process vs subprocess) | §4 |
| 5 | Sandbox Cage/Policy/WorkerPool; macOS; trusted mode | §5 |
| 6 | Graph/TerminusDB model; IR-blob cold path | §6 |
| 7 | Treesitter CST production/storage | §7 |
| 8 | Dual-form crate split + concrete API sketches | §8 |
| 9 | Incrementality / hashes / determinism | §9 |
| — | `render/`, `heart` vocabulary, risks | §10–12 |

---

## 1. Full Pipeline Map

### 1.1 Entry Points

| Entry | File:line | Context |
|---|---|---|
| `generate(input)` | `workspace/compiler/generate/mod.rs:85–90` | Owns a per-call `LocalForgeContext::new()` (memory CAS + DevPassthrough + `ToolchainSet::from_env`) |
| `generate_with(ctx, input)` | `generate/mod.rs:96–138` | Injected `ForgeContext` — **the real library API** |
| Daemon `POST /compile` | `bin/compiler_daemon.rs:107–140` | postcard body → `spawn_blocking` → `compile_package` |
| Daemon compile path | `bin/compiler_daemon.rs:181–241` | materialize tempdir → peel tarball root → `generate_with` → wire map |

```
PackageInput {
  coordinates: heart::package::Coordinates,
  toolchain:   heart::Toolchain,
  root:        PathBuf,            // already-materialized source tree
}
  │
  ▼  generate/mod.rs:100–111
hash_source_tree(root)  →  ContentHash   // parse_cache.rs:13 — sorted path‖file-hash
hash_dep_lock(root)     →  ContentHash   // parse_cache.rs:50 — Cargo.lock / go.sum / …
JobKey::derive(
    PRODUCER_VERSION,                // "nudox-producer/2"  producer/mod.rs:281
    toolchains.digest().as_bytes(),  // sandbox/toolchains.rs:123
    source_hash.as_bytes(),
    dep_lock.as_bytes(),
)  →  JobKey                         // heart/content.rs:62 — length-prefixed BLAKE3
  │
  ├─► surface::build(ctx, input)     CAS key = job.as_hash()
  │     generate/surface.rs:37–39, 55–107
  │     → seal_package → run_producer → ProducerOutput { index, aux }
  │     → Ir::from_entries → Index
  │
  ├─► cst::extract(input)            CAS key = job.with_tag(b"cst")
  │     generate/cst.rs:64
  │     → arborium parse → walk_references → CstSet  (trees dropped)
  │
  ├─► occurrences::build(input, surface)
  │     CAS key = job.with_tag(RESOLVER_VERSION.as_bytes())  // "occ-v1" resolve.rs:33
  │     generate/occurrences.rs:64–125
  │     → LanguageSpec.extract + resolve → OccurrenceSet
  │
  └─► source_archive::build(input)   CAS key = job.with_tag(b"archive")
        generate/source_archive.rs:42
        → walk + BLAKE3 per file → SourceArchive { files: Vec<FileDigest> }
  │
  ▼
BlobInfo::assemble(surface, cst, archive)   generate/blob_info.rs:30
  → snapshot: ContentHash (sorted-fold of (path_len, path, file_hash))
  │
  ▼
GeneratedPackage {
    coordinates, toolchain,
    surface: Index,               // ir::entry::Index
    cst: CstSet,
    occurrences: OccurrenceSet,   // ir::syntax::OccurrenceSet
    archive: SourceArchive,
    blob_info: BlobInfo,
    snapshot: ContentHash,
}
```

**CAS serialization:** all stage values go through `cache_get_or_build` (`compile/producer/runtime.rs:125–153`) as **postcard** (`Serialize + Deserialize`).

### 1.2 Surface Production (language dispatch)

File: `generate/surface.rs:55–107`

`run_producer` (private, surface.rs:55) seals the package then dispatches by `input.coordinates.ecosystem()`:

| Language | Producer type | Constructed as |
|---|---|---|
| Rust | `RustProducer { name, version, direct_repo }` | surface.rs:87–91 |
| Go | `GoProducer` | surface.rs:93 |
| Java | `JavaProducer` | surface.rs:94 |
| C# | `CSharpProducer` | surface.rs:95 |
| Python | `PythonProducer` | surface.rs:96 |
| TypeScript | `TypescriptProducer { name }` | surface.rs:101–103 |
| Nix | `NixProducer` | surface.rs:105 |

Seal path (`surface.rs:67–76` → `producer::seal_package` at `compile/producer/mod.rs:291–365`):
1. `Scratch::temp("producer")` — RAII temp dir (`scratch.rs:34–40`)
2. Hermetic `Env` (fixed PATH + toolchain env; `project_hermetic_env` strips secret-shaped keys, mod.rs:372–401)
3. RO binds for package tree + toolchains + optional `/nix/store`; RW scratch
4. `Job::acquiring(...).seal(tier).into_input()` — **type-enforced net-off** (mod.rs:350–359)

Then `producer::run_producer(ctx, &p, sealed.input())` (`runtime.rs:160–188`):
- CAS get by `input.key.as_hash()` → postcard decode `ProducerOutput`
- miss → `p.produce(ctx, input)` → postcard put

### 1.3 Producer Lifecycle: plan → execute → decode

File: `compile/producer/mod.rs:404–474` (`execute`)

```
Producer::plan(ctx, input) → ExecPlan
    ExecPlan::Commands(Vec<SealedCommand>)   # Go fully; Java/C#/Rust adaptive-stub plan
  OR
    ExecPlan::Library(WorkerLang)            # Nix / TypeScript / Python
  OR
    ProducerError::adaptive(...)             # Rust, Java, C# — override produce()

execute:
  Library  → execute_library (mod.rs:416–443)
               worker_pool(lang).lower(...)  OR  lower_in_process (dev)
  Commands → execute_commands (mod.rs:446–474)
               for cmd: ctx.cage().run(cmd, CancelToken::never())

decode(input, last_captured) → ProducerOutput { index: Index, aux: AuxOutputs }
```

Default `produce` = `execute` (`mod.rs:248–254`). Adaptive producers override `produce` and call `lower_in_process` directly.

### 1.4 Stage: CST Production

File: `generate/cst.rs:64–151` (approx)

- Walk source tree; skip `.git` / `target` / etc.
- Per file: `arborium::get_language` + `arborium_tree_sitter::Parser` + `walk_references`
- Live `Tree` is **dropped** — only serializable spans kept
- Output: `CstSet { files: Vec<Cst { path, references: Vec<ResolvedReference> }> }`
- Sorted by path; `Serialize`/`Deserialize` (serde)

### 1.5 Stage: Occurrence Resolution

File: `generate/occurrences.rs:64–125` + `generate/resolve.rs:33+`

- Second tree-sitter pass via `treesitter::spec_for(lang)` → `LanguageSpec::extract`
- `resolve(lang, extractions, surface)` → `OccurrenceSet` with confidence ladder
- CAS key domain: `RESOLVER_VERSION = "occ-v1"` (`resolve.rs:33`)

### 1.6 Stage: Source Archive + Blob Info

| Stage | File:line | Output |
|---|---|---|
| `source_archive::build` | `source_archive.rs:42` | `SourceArchive { files: Vec<FileDigest { path, hash, size }> }` sorted |
| `BlobInfo::assemble` | `blob_info.rs:30` | folds archive digests; surface/CST intentionally unused for identity (`let _ = (surface, cst)`) |

### 1.7 Stage: Linked Data / Graph Emission (optional fan-out)

File: `generate/linked_data/emit.rs`

```
emit(index, occurrences, ctx: PackageCtx, sink: &mut dyn DocumentSink):
    corpus = project(index, occurrences, ctx)   # graph/from_ir.rs:903
    Wave 1: packages
    Wave 2: symbols bare (EDGE_FIELDS stripped — emit.rs:26–27)
    Wave 3: symbols full
    Wave 4: Implementation + Reference reified relations
    Wave 5: PackageVersion (declares list)
```

Documents are `serde_json::Value` JSON-LD via `terminusdb_schema::ToJson`.  
**Not** called from `generate()` / `generate_with()` — graph emit is a separate caller concern (server/registry today).

### 1.8 Daemon Wire Path (subset of full pipeline)

`bin/compiler_daemon.rs:205–240` runs full `generate_with` but **returns only**:

| Wire field | Source | Format |
|---|---|---|
| `surface: Vec<u8>` | `generated.surface` | postcard-encoded `ir::entry::Index` |
| `references: Vec<WireFile>` | `generated.cst` | wire mirror of `ResolvedReference` (`protocol.rs:117–127`) |
| `identifiers: Vec<String>` | public names from Index | UTF-8 strings |

**Not on the wire today:** `OccurrenceSet`, `SourceArchive`, `BlobInfo` / snapshot, full `GeneratedPackage`. The protocol comment (`protocol.rs:10–13`) states the daemon returns IR surface bytes and the *server* writes blobs — consistent with current mapping, but incomplete for the planned dual-store model that also wants occurrences + archive digests.

### 1.9 Serialization Formats Summary

| Data | Format | Where |
|---|---|---|
| `ProducerOutput` | postcard | CAS (L1 memory / L2 disk / L3 object-store) |
| `CstSet`, `OccurrenceSet`, `SourceArchive`, `Index` (stage cache) | postcard | CAS via `cache_get_or_build` |
| Individual source file blobs | raw bytes + BLAKE3 name | registry / local disk (outside compiler crate) |
| Graph documents | JSON-LD (`serde_json::Value`) | TerminusDB via `DocumentSink` |
| Daemon ↔ server | postcard (`CompileRequest`/`CompileResponse`) | HTTP `application/x-postcard` |
| WorkerPool jobs | line-oriented JSON (`JobRequest`/`JobResponse`) | stdin/stdout of `producer-worker` |
| Oracle stdout (Go/Java/C#) | JSON → language schema → IR | subprocess capture |

### 1.10 Types Crossing Stage Boundaries

```
PackageInput
  → SealedInput (sandbox::seal) + JobKey
  → ProducerOutput { index: ir::entry::Index, aux: AuxOutputs }
  → Index (surface)
  → CstSet (generate::cst)
  → OccurrenceSet (ir::syntax)
  → SourceArchive / FileDigest (generate::source_archive)
  → BlobInfo { archive, snapshot: ContentHash }
  → GeneratedPackage
  → (optional) GraphCorpus → JSON-LD documents
  → (daemon) CompileResponse { surface bytes, WireFile[], identifiers }
```

---

## 2. State Inventory

### 2.1 Filesystem Paths (read at runtime)

| State | Where accessed | Library-embed blocker? | Server-binary blocker? |
|---|---|---|---|
| Package source root (`input.root`) | All producers, `generate/` | No — injected | No |
| Scratch dirs (`Scratch::temp`) | `compile/producer/scratch.rs:34` | No — transient | No |
| `std::env::temp_dir()` | `compile/isolate.rs:274`, java/csharp/go oracle paths | Mild — reads TMPDIR | No |
| Buck2 resource files (oracle binaries) | `compile/producer/resource.rs:21–52` via `current_exe()` + `.resources.json` | **Yes** — Buck-only | Partial (works under Buck image) |
| Source walk skip dirs | `generate/cst.rs`, `source_archive.rs`, `parse_cache.rs:32` | No | No |
| Cargo/rustup home | `sandbox/toolchains.rs:65–66` | No if injected | No if injected |
| `/nix/store` RO bind if present | `producer/mod.rs:318–320` | No — optional | No |
| CAS disk root | `NUDOX_CAS_ROOT` → `daemon/forge.rs:142–145` | N/A (library uses memory CAS) | Optional L2 |
| `bwrap` on PATH | `sandbox/cage.rs:105` | macOS: unavailable → DevPassthrough | Production needs bwrap |

### 2.2 Environment Variables Read at Runtime

| Variable | Read by | Library-embed issue? |
|---|---|---|
| `NUDOX_COMPILER_ADDR` | `bin/compiler_daemon.rs:59` | Binary only |
| `NUDOX_CAS_ROOT` | `bin/compiler_daemon.rs:69` | Binary only |
| `NUDOX_PRODUCER_WORKER` | `daemon/forge.rs:263`, `sandbox/probe.rs:81` | **Boundary** — must become construction param |
| `NUDOX_SANDBOX_REQUIRE` | `sandbox/probe.rs:57–60` | Binary assemble only |
| `NUDOX_ENV` | `sandbox/probe.rs:62–64` | Binary assemble only |
| `NUDOX_TOOLCHAIN_*`, `RUSTUP_HOME`, `CARGO_HOME`, `JAVA_HOME`, `GOROOT`, `GOPATH` | `sandbox/toolchains.rs:55–71` | **Boundary** — `ToolchainSet::from_env` is the sanctioned sealer read |
| `NUDOX_TOOLCHAIN_PATH` | `sandbox/toolchains.rs:61–63` | Same |
| `NUDOX_TYPESCRIPT_ORACLE` | `compile/typescript/oracle/tsz.rs:57–61` | Optional opt-in; still an ambient read inside library path |
| `NUDOX_RUST_PRODUCER` | comments/tests only (`rust/producer.rs:73`); rustdoc path removed | Document only — RA is default |

**Critical finding:** `IsolationPolicy::require_worker()` at `sandbox/probe.rs:80–82` reads env. `ForgeRuntime::assemble` copies that into a field (`daemon/forge.rs:156`) and implements `ForgeContext::require_worker` (`forge.rs:218–220`). Trait injection is correct; the env read must stay **outside** any embedded library path. `LocalForgeContext` correctly defaults `require_worker` to `false` (trait default, `runtime.rs:71–73`).

**Secondary ambient read:** `NUDOX_TYPESCRIPT_ORACLE` is checked inside the TS producer path, not via `ForgeContext` — a librarification gap.

### 2.3 Process Globals / OnceLocks

| Global | Location | Implication |
|---|---|---|
| Seccomp BPF path `OnceLock` | `sandbox/cage.rs:362` | Process-wide compile-once; fine for single-process embed |
| `CancelToken::never` static | `sandbox/cancel.rs:34` | Fine |
| `arborium::get_language` | grammar registry | Compiled-in grammars; thread-safe if arborium is |
| `ra_ap_*` RootDatabase | `compile/rust/ra/load.rs` | Per-call, not process-global — OK for server; keep-alive needed for incremental library form |

### 2.4 Network Access

- Seal path forces `NetGrant::Off` (`producer/mod.rs:356–358`; `ThreatTier::net_default` always `Off` at `budget.rs:40–42`)
- Go oracle sets `GOPROXY=off` (`compile/go/producer.rs:46`)
- **No production compiler path grants network** — type-enforced at `Job::seal`
- Acquisition / registry fetch is outside this crate

### 2.5 Toolchain Discovery

- `ToolchainSet::from_env()` — sole sanctioned multi-var read (`toolchains.rs:55`)
- `which::which("bwrap")` — runtime PATH scan in `LinuxNamespaces::new` (`cage.rs:105`)
- `producer_worker_bin()` — env → exe-dir → PATH (`daemon/forge.rs:262–280`)
- Oracle binaries via `buck_resource` — **not** PATH; Buck resources manifest (`resource.rs:21–52`)
- Java/C# still require host `javadoc` / `dotnet` on hermetic PATH (via `NUDOX_TOOLCHAIN_PATH` or binds)

### 2.6 Database / Object Store Handles

- **None** in the compiler crate
- CAS injected via `ForgeContext::cas()` (`runtime.rs:48`)
- TerminusDB only as schema/types + `DocumentSink` abstraction — no live client connection

### 2.7 Blocker Summary

| Goal | Blockers |
|---|---|
| Pure library embed (trusted) | `buck_resource` / Buck-only oracles; ambient `NUDOX_TYPESCRIPT_ORACLE`; `LocalForgeContext::new` always `from_env`; heavy RA/OXC/pyrefly/snix binary size; macOS no real cage (acceptable for trusted) |
| Stateless sealed server | Adaptive producers (Rust fully in-process; Java/C# multi-step not fully sealed-command staged); daemon L3 always `Unsupported`; wire omits occurrences/archive; needs bwrap for production policy |

---

## 3. Daemon / Protocol: Implemented vs Stubbed

### 3.1 Fully Implemented

**Protocol** (`workspace/compiler/protocol.rs`):

```rust
// protocol.rs:52–57, 61–69, 73–93
FileBytes { path, bytes }
CompileRequest { coordinates, toolchain, files: Vec<FileBytes> }
CompileResponse::Ok { surface, references, identifiers }
CompileResponse::Err { kind, message }
WireFile / WireReference / WireTarget   // postcard mirrors of IR reference types
```

Duplicated intentionally with `registry::protocol` for byte-identical postcard layout (`protocol.rs:1–6`).

**Daemon binary** (`bin/compiler_daemon.rs`):
- `GET /health` (liveness) — :101–104
- `POST /compile` postcard, 256 MiB body ceiling — :80–86, :107–140
- Materialize + peel single top-level dir (crates.io/npm layout) — :186–196, :251–274
- `ForgeRuntime::assemble(Policy::Development, …)` — :67–74  
  ⚠ Comment mentions `NUDOX_ISOLATION_POLICY=production` but actual gate is `NUDOX_SANDBOX_REQUIRE` / `NUDOX_ENV` via `IsolationPolicy` (`probe.rs:57–74`). Daemon **hardcodes** `Policy::Development` today — production policy is not wired from env into `assemble`.

**ForgeRuntime** (`daemon/forge.rs`):
- Cold/Ready typestate — :56–59, :131–178
- `select_cage` — Production requires LinuxNamespaces+bwrap; Development falls back to DevPassthrough — :231–251
- `DaemonL3::None` always `Unsupported` — :34–48
- `Tiered<DaemonL3>` L1 memory + optional L2 disk — :141–146
- nix + parser `WorkerPool`s from `producer_worker_bin()` — :158–162
- Implements `ForgeContext` — :188–221

**Worker binary** (`bin/producer_worker.rs`):
- `lower <lang> <root>` one-shot JSON Index on stdout
- `serve` line-oriented JSON for `WorkerPool`
- Langs: nix | typescript | python only

**WorkerPool** (`sandbox/worker.rs`): free-list + condvar, rlimits, cgroup best-effort, `JobRequest`/`JobResponse`.

### 3.2 Adaptive / Not Fully Sealed

| Producer | plan() | produce() | Cage status |
|---|---|---|---|
| Rust | `adaptive("rustdoc multi-crate")` `rust/producer.rs:46` | `lower_in_process` → RA HIR (`:59–84`) | **No cage** — pure in-process |
| Java | `adaptive("javadoc multi-step")` | `lower_in_process` → `oracle::extract` | Oracle uses `IsolatedCommand` + `run_isolated` → **cage for javadoc only** |
| C# | `adaptive("roslyn oracle multi-step")` | `lower_in_process` → package/oracle | Oracle uses `IsolatedCommand` → **cage for `dotnet` only** |
| Go | real `ExecPlan::Commands` | default execute | **Fully sealed command path** |
| TS/Py/Nix | `ExecPlan::Library` | default execute | WorkerPool or in-process fallback |

Phase 4 comments claim sequential sealed commands for multi-step toolchains — not landed.

### 3.3 DAEMON-PLAN Status Matrix

| Concept | Status | Evidence |
|---|---|---|
| `SealedInput` / `CapabilityBudget` | Done | `sandbox/seal.rs`, `budget.rs` |
| `Job` acquire→seal typestate | Done | `sandbox/job.rs`; used at `producer/mod.rs:350–359` |
| `Cage` trait | Done | `sandbox/cage.rs:76–85` |
| `LinuxNamespaces` | Done | `cage.rs:97+` |
| `DevPassthrough` | Done | `cage.rs:468–553`; Production construction denied |
| `WorkerPool` | Done | `sandbox/worker.rs` |
| `ForgeContext` | Done | `runtime.rs:27–74` |
| `ForgeRuntime` | Done | `daemon/forge.rs` |
| `run_producer` CAS | Done | `runtime.rs:160–188` |
| `ExecPlan::Library` | Done | Nix/TS/Python |
| `ExecPlan::Commands` | Partial | Go full; Java/C# adaptive partial; Rust none |
| Distributed L3 at daemon | Stub | `DaemonL3::None` |
| Daemon returns full blob set | Incomplete | surface+refs+ids only |

---

## 4. Producer-by-Producer Analysis

### 4.1 Rust — `RustProducer`

| | |
|---|---|
| **ID** | `ProducerId("rustdoc/3")` — `compile/rust/producer.rs:31` (name historical; RA is the impl) |
| **Tier** | `ThreatTier::Untrusted` — :38 |
| **Mode** | Adaptive in-process library |
| **Stack** | `ra_ap_load_cargo`, `ra_ap_hir`, `ra_ap_ide_db`, … (BUCK:35–45) |
| **Entry** | `compile/rust/mod.rs:40` → `ra::generate_ir` |
| **Runtime needs** | `RUSTUP_HOME`/`CARGO_HOME` for cargo metadata; proc-macro srv optional |
| **Sandbox** | Bypasses cage entirely |
| **Desktop embed** | **Very high** cost (+ tens of MB). Works on macOS natively |
| **Incrementality** | Reloads workspace every call; keeping `RootDatabase` alive is the library-form win |

### 4.2 TypeScript — `TypescriptProducer`

| | |
|---|---|
| **ID** | (see producer const) Library plan `WorkerLang::Typescript` — `typescript/producer.rs:36` |
| **Tier** | `Hostile` — :28 |
| **Mode** | WorkerPool or in-process OXC (`oxc_*`) |
| **Tier C** | Optional tsz oracle via `NUDOX_TYPESCRIPT_ORACLE` (`oracle/tsz.rs:57`) |
| **Runtime binary** | None (pure Rust) |
| **Desktop embed** | Moderate; tsz vendored stack is large if linked |
| **macOS** | Native |

### 4.3 Python — `PythonProducer`

| | |
|---|---|
| **Mode** | `ExecPlan::Library(WorkerLang::Python)` — `python/producer.rs:33` |
| **Tier** | `Hostile` — :25 |
| **Stack** | pyrefly + ruff_python_ast (BUCK:53–59) |
| **Runtime binary** | None |
| **Desktop embed** | Moderate pure-Rust cost |
| **macOS** | Native |

### 4.4 Go — `GoProducer`

| | |
|---|---|
| **ID** | `ProducerId("go-oracle/1")` — `go/producer.rs:23` |
| **Mode** | `ExecPlan::Commands` — fully sealed — :52 |
| **Oracle** | Buck resource `go-oracle` |
| **Env** | `GOWORK=off`, `GOFLAGS=-mod=mod`, `GOPROXY=off` — :44–46 |
| **Desktop embed** | Ship platform-native oracle binary; inject path (not `buck_resource`) |
| **macOS** | Works under DevPassthrough |

### 4.5 Java — `JavaProducer`

| | |
|---|---|
| **ID** | `ProducerId("javadoc/1")` — `java/producer.rs:24` |
| **Mode** | Adaptive; `oracle::extract` runs `javadoc -doclet` via cage |
| **Oracle JAR** | Buck resource `java-oracle.jar` |
| **Runtime** | Host `javadoc` on PATH |
| **Desktop embed** | Needs JDK; inject jar path |
| **macOS** | DevPassthrough (unsandboxed host process) |

### 4.6 C# — `CSharpProducer`

| | |
|---|---|
| **Mode** | Adaptive; `dotnet oracle.dll` via cage (`csharp/oracle.rs:95–116`) |
| **Oracle** | Buck resource `csharp-oracle` (published directory) |
| **Runtime** | Host `dotnet` |
| **Desktop embed** | Needs .NET SDK; inject oracle dir |
| **macOS** | DevPassthrough |

### 4.7 Nix — `NixProducer`

| | |
|---|---|
| **Mode** | `ExecPlan::Library(WorkerLang::Nix)` — `nix/producer.rs:37` |
| **Tier** | `Hostile` — :29 (executes package Nix code) |
| **Stack** | snix_eval + rnix + rowan |
| **Runtime binary** | None |
| **Desktop embed** | Moderate; **GPL licensing of snix** is a distribution risk |
| **macOS** | Native |

### 4.8 Summary Table

| Producer | ThreatTier | Runtime binary | Exec path | Cage? | macOS |
|---|---|---|---|---|---|
| Rust | Untrusted | (optional) proc-macro-srv | Adaptive in-process | No | Yes |
| TypeScript | Hostile | None | Library worker/in-proc | Worker isolation | Yes |
| Python | Hostile | None | Library worker/in-proc | Worker isolation | Yes |
| Go | Untrusted | go-oracle | Commands | Yes (cmd) | DevPassthrough |
| Java | Untrusted | javadoc + jar | Adaptive + IsolatedCommand | Partial | DevPassthrough |
| C# | Untrusted | dotnet + dll | Adaptive + IsolatedCommand | Partial | DevPassthrough |
| Nix | Hostile | None | Library worker/in-proc | Worker isolation | Yes |

### 4.9 Trusted vs Untrusted Suitability (librarification)

**Trusted local (developer project):** All seven can run if toolchains present. Prefer in-process for TS/Py/Nix/Rust; ship oracles for Go/Java/C# or feature-gate languages.

**Untrusted remote (k8s fleet):** Prefer worker/cage for Hostile; finish Phase-4 sealed commands for Java/C#; move Rust into worker subprocess (RA is huge and currently unsandboxed). Production policy requires bwrap (`Policy::Production` + `LinuxNamespaces`).

---

## 5. Sandbox: Cage / Policy / WorkerPool

### 5.1 Cage Hierarchy

```
Cage (trait)  sandbox/cage.rs:76
  ├── LinuxNamespaces   production: bwrap + cgroup v2 + seccomp + rlimits
  │     new() → which("bwrap")   cage.rs:103–106
  └── DevPassthrough    development only (Policy::Development token)
        try_new          cage.rs:477–484
        run: rlimits + best-effort cgroup; no namespaces  :508–552
```

Linux-only modules:
- `sandbox/landlock.rs` — `#[cfg(target_os = "linux")]` (`lib.rs:55–56`)
- `sandbox/seccomp.rs` — same (`lib.rs:57–59`)

### 5.2 Policy Resolution

```rust
// sandbox/cage.rs:64–72
Policy::from_env() // Production if IsolationPolicy::env_requires_production()

// sandbox/probe.rs:57–82
env_requires_production: NUDOX_SANDBOX_REQUIRE=1|true || NUDOX_ENV=prod|production
require_worker: env_requires_production || NUDOX_PRODUCER_WORKER set
```

`DevPassthrough` **cannot** be constructed under `Policy::Production` (`cage.rs:480–482`).

### 5.3 macOS Behavior Today

1. `LinuxNamespaces::new()` finds no `bwrap` → `probe_available() = false`
2. `select_cage(Development)` → `DevPassthrough` (`forge.rs:243–249`)
3. `select_cage(Production)` → hard error `NoProductionCage` (`forge.rs:233–241`)
4. Workers: rlimits via `pre_exec` (unix); cgroups fail gracefully
5. GUI target machines are **always Development/trusted** in practice

### 5.4 Trusted Mode = No Sandbox

Already almost exists:

```rust
// Current trusted-ish path (library default):
LocalForgeContext::new()
// → DevPassthrough::try_new(Development)   runtime.rs:240–241
// → require_worker = false (trait default)
// → no worker pools
// → ToolchainSet::from_env()  // should become explicit for embed
```

Recommended addition for fully trusted desktop:

```rust
/// Zero-overhead cage for first-party code (no rlimits, no pre_exec).
pub struct TrustedPassthrough;
impl Cage for TrustedPassthrough {
    fn id(&self) -> CageId { CageId("trusted-passthrough") }
    fn capabilities(&self) -> CageCaps { CageCaps::default() }
    fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
        // plain std::process::Command with budget env only
    }
}
```

`ThreatTier::Trusted` already exists (`budget.rs:31`) with `ceiling() = None` (`budget.rs:74`). No producer uses it yet.

### 5.5 WorkerPool Mechanics

- Config per language profile (`WorkerPoolConfig::nix` / `static_parser`)
- Parent does `env_clear`; child only gets sealed env
- Protocol: JSON lines (`JobRequest::Lower { lang, root }` → `JobResponse::Ok { body }`)
- `producer-worker` binary built at `workspace/compiler/BUCK:100–111`

---

## 6. Graph / TerminusDB Lowering

### 6.1 Module Layout

| Module | Role |
|---|---|
| `graph/model.rs` | `#[derive(TerminusDBModel)]` document types |
| `graph/link.rs` | Name → IRI linker; stubs; `PackageCtx` |
| `graph/from_ir.rs` | `project` → `GraphCorpus` (:890–957) |
| `graph/symtab.rs` | FQ/suffix lookup tables |
| `generate/linked_data/emit.rs` | Wave-ordered JSON-LD stream |
| `generate/linked_data/schema.rs` | Schema upload helpers |

### 6.2 Document Model (high level)

- **`Package`** — version-agnostic (`Package/{language}%2F{name}`)
- **`PackageVersion`** — `declares: Vec<TdbLazy<Symbol>>`
- **`Symbol`** — id/uri/fq_name/kind/visibility/docs/resolved + edges (`member_of`, `implements`, `extends`, `mentions`, `takes`, `returns`) + inline `shape`
- **`Shape`** — Record | Function | TraitDef | TraitImpl | Sum | Union | Alias | Info
- **`Implementation`** / **`Reference`** — reified, content-addressed (`value_hash`)

Linker resolution (`link.rs`): exact fq → alias → unique last-segment → `~extern` stub. Total: edges never dangle.

### 6.3 Cold Path Without TerminusDB

**Yes — already pure functions:**

```rust
// graph/from_ir.rs:903
pub fn project(index: &Index, occurrences: &OccurrenceSet, ctx: PackageCtx) -> GraphCorpus;

// generate/linked_data/emit.rs
pub fn emit(..., sink: &mut dyn DocumentSink) -> Result<(), GenerateError>;
```

Cold-package path for librarification:

1. Load postcard `Index` (+ optional `OccurrenceSet`) from blob store  
2. `project(...)` → in-memory `GraphCorpus`  
3. Answer adjacency / signature / call-graph queries in-process  
4. Optionally `emit` to NDJSON / sqlite / never to TerminusDB  

**What you lose without TerminusDB:** server-side WOQL/GraphQL multi-hop, shared reverse indexes, durable branch/diff history. Acceptable for cold long-tail packages under leaky-bucket admission.

---

## 7. Tree-Sitter Integration

### 7.1 CST Pipeline Storage

Trees are **transient**. Stored form is span lists only (`generate/cst.rs` docs:3–7).

```rust
// generate/cst.rs
Cst { path: PathBuf, references: Vec<ResolvedReference> }
CstSet { files: Vec<Cst> }  // sorted by path
```

No tree-sitter tree serialization in the indexing pipeline.

### 7.2 Occurrence Extractors

`treesitter/mod.rs:34–44` — `spec_for(lang)` → static `LanguageSpec` per language (all 7 langs).  
`treesitter/spec.rs:153` — `LanguageSpec::extract` → definitions/references/imports.  
Trees dropped after extract.

### 7.3 Snippet / Embedding Path

`treesitter/mod.rs:517` — `parse_and_extract`:
- Find enclosing function, extract snippet, s-expression, references
- Serialize to `TreesitterRepr(Vec<u8>)` = JSON `TreesitterPayload`
- **Rendering/embedding feature**, not the main generate pipeline storage

### 7.4 Grammar Registry

`arborium::get_language(name)` — compiled-in grammars (feature-flagged crates). No dynamic `.so` loading.

### 7.5 Answer

| Question | Answer |
|---|---|
| How are CSTs produced? | Per-file arborium parse in `cst::extract` + occurrence `LanguageSpec` pass |
| How stored? | `CstSet` / `OccurrenceSet` as postcard CAS blobs; not live trees |
| Tree serialization? | **No** for pipeline; only `TreesitterRepr` JSON for snippets |
| Reparse on demand? | **Yes** — always reparse when stage cache misses |

---

## 8. Proposed Dual-Form Crate Split

### 8.1 Target Layout

```
compiler-core (lib)                 # pure pipeline; no ambient policy reads
  compile/producer/                 # ForgeContext, Producer, seal, execute, CAS helpers
  compile/{rust,typescript,python,go,java,csharp,nix}/
  compile/isolate.rs
  compile/vcs.rs                    # acquisition helpers (may move to client later)
  generate/                         # generate_with, surface, cst, occurrences, archive, blob_info
  generate/linked_data/             # emit + DocumentSink (graph fan-out)
  graph/                            # pure IR → GraphCorpus
  render/                           # IR → surface syntax (Wadler docs)
  treesitter/                       # LanguageSpec + snippet path
  error.rs

compiler-wire (lib, tiny)           # shared postcard types
  protocol.rs                       # CompileRequest/Response, Wire*  (single source of truth)

compiler-daemon (bin crate)
  bin/compiler_daemon.rs
  daemon/forge.rs                   # ForgeRuntime, DaemonL3, select_cage, producer_worker_bin
  # depends on compiler-core + compiler-wire + sandbox

producer-worker (bin)               # already exists
  bin/producer_worker.rs

sandbox (lib, existing path)        # workspace/compiler/sandbox
  cage, worker, toolchains, budget, seal, probe, …

compiler-client / trusted-forge (lib, new or part of future client)
  TrustedForgeContext { cage, cas, toolchains, oracles, require_worker: false }
  OracleSet { go, java_jar, csharp_dir }
  re-export generate_with + GeneratedPackage
```

### 8.2 What Stays / Moves / Changes

| Item | Action |
|---|---|
| `lib.rs` `pub mod daemon` | **Remove from core** — binary-only (`lib.rs:21` currently exports it) |
| `protocol.rs` | Move to `compiler-wire`; both daemon and server/registry depend on it |
| `buck_resource` | Replace with `ForgeContext::oracles()` / `OracleSet` for non-Buck embeds |
| `IsolationPolicy::require_worker` env | Call only from binary `assemble`; never from core |
| `LocalForgeContext::new` | Add `new_with(toolchains, cage, cas_config)`; keep `new()` for tests |
| `NUDOX_TYPESCRIPT_ORACLE` | Move to `ForgeContext` flag / `AuxConfig` |
| `ToolchainSet::from_env` | Remain in sandbox; only called at binary/client assemble |
| `render/` | Stays in core — pure, no ambient state |
| `graph/` | Stays in core — pure cold-path graph ops |

### 8.3 Concrete Library-Facing API Sketch

```rust
// ── compiler-core public surface ──────────────────────────────────────────

// Pipeline
pub use generate::{
    PackageInput, GeneratedPackage, BlobInfo, SourceArchive, FileDigest, CstSet,
    generate_with, // generate() convenience optional / test-only
};
pub use generate::linked_data::{DocumentSink, emit as emit_graph};
pub use graph::from_ir::{GraphCorpus, project as project_graph};
pub use graph::link::PackageCtx;

// Context injection (the heart of dual-form)
pub use compile::producer::{
    ForgeContext, LocalForgeContext,
    Producer, ProducerOutput, ExecPlan, ProducerError, AuxOutputs,
    PRODUCER_VERSION, seal_package, run_producer, cache_get_or_build,
};

// Extended trait methods to add:
// trait ForgeContext {
//     type Cas: Cas + EvictableCas;
//     fn node(&self) -> &NodeId;
//     fn cage(&self) -> &dyn Cage;
//     fn cas(&self) -> &Self::Cas;
//     fn toolchains(&self) -> &ToolchainSet;
//     fn overrides(&self) -> &OverrideTable;
//     fn observer(&self) -> &dyn ForgeObserver;
//     fn handle(&self) -> &tokio::runtime::Handle;
//     fn worker_pool(&self, lang: WorkerLang) -> Option<&WorkerPool>;
//     fn require_worker(&self) -> bool;
//     fn oracles(&self) -> &OracleSet;           // NEW
//     fn typescript_oracle_enabled(&self) -> bool; // NEW (replaces env)
// }

pub struct OracleSet {
    pub go_oracle: Option<PathBuf>,
    pub java_oracle_jar: Option<PathBuf>,
    pub csharp_oracle_dir: Option<PathBuf>,
}

// Trusted client forge
pub struct TrustedForgeContext { /* owns Dev/Trusted cage + memory/disk CAS */ }
impl TrustedForgeContext {
    pub fn assemble(toolchains: ToolchainSet, oracles: OracleSet, cas_root: Option<PathBuf>) -> Self;
}
impl ForgeContext for TrustedForgeContext { /* … */ }

// Wire (compiler-wire)
pub use protocol::{CompileRequest, CompileResponse, FileBytes, WireFile, WireReference};
```

**Existing names to keep:** `ForgeContext`, `SealedInput`, `JobKey`, `ContentHash`, `PackageInput`, `GeneratedPackage`, `Producer`, `run_producer`, `generate_with`, `ThreatTier`, `Cage`, `WorkerPool`. These already form a coherent DAEMON-PLAN vocabulary — do not rename in the split.

### 8.4 Refactor Sequence (ordered)

1. **Extract `compiler-wire`** from duplicated `compiler::protocol` / `registry::protocol`.  
2. **Stop exporting `daemon` from lib** — binary depends on `compiler_daemon` internals crate or `#[cfg]` bin-only modules.  
3. **`OracleSet` on `ForgeContext`** — rewrite `buck_resource` call sites in go/java/csharp.  
4. **`LocalForgeContext::new_with_toolchains`** — stop implicit env in embed path.  
5. **`TrustedPassthrough` + `ThreatTier::Trusted` wiring** for GUI.  
6. **Expand daemon response** (or add `/compile/v2`) to include postcard occurrences + archive digests when INDEX/REGISTRY both need them.  
7. **Phase-4 sealed multi-step** for Java/C#; **Rust-in-worker** for untrusted fleet.  
8. **Feature-gated language deps** so desktop builds can omit snix/RA/Java if unused.

### 8.5 Modules / Functions That Change Signature

| Function | File:line | Change |
|---|---|---|
| `LocalForgeContext::new` | `runtime.rs:224` | Split into env vs explicit constructors |
| `buck_resource` | `resource.rs:21` | Deprecate; oracles from context |
| `IsolationPolicy::require_worker` | `probe.rs:80` | Document binary-only; never call from core |
| `producer_worker_bin` | `forge.rs:262` | Stays daemon-side |
| `select_cage` | `forge.rs:231` | Stays daemon-side; client has own selector |
| `generate` | `generate/mod.rs:85` | Optional: `#[cfg(test)]` or keep as thin wrapper |
| `tsz::enabled` | `typescript/oracle/tsz.rs:61` | Read context flag, not env |
| Go `oracle_binary` | `compile/go/package.rs` | Use `ctx.oracles().go_oracle` |
| Java/C# resource resolution | `oracle.rs` files | Same |

---

## 9. Incrementality Readiness

### 9.1 Content Hashes Already Present

| Hash | Covers | File:line |
|---|---|---|
| `JobKey::derive` | producer_version ‖ toolchain ‖ source ‖ dep_lock (len-prefixed) | `heart/content.rs:62–74` |
| `JobKey::with_tag` | child keys (`cst`, `archive`, resolver version) | `heart/content.rs:86–92` |
| `hash_source_tree` | sorted rel-path ‖ per-file BLAKE3 | `generate/parse_cache.rs:13–24` |
| `hash_dep_lock` | named lockfiles | `parse_cache.rs:50–77` |
| `FileDigest.hash` | per-file bytes | `source_archive.rs` |
| `BlobInfo.snapshot` | sorted path_len + path + file_hash fold | `blob_info.rs:30+` |
| `ToolchainSet.digest` | labeled toolchain path strings | `toolchains.rs:123–146` |
| `PRODUCER_VERSION` | `"nudox-producer/2"` domain separator | `producer/mod.rs:281` |
| `RESOLVER_VERSION` | `"occ-v1"` | `resolve.rs:33` |

### 9.2 Determinism

**Holds:**
- Source tree hash / archive / CST sort by path  
- Occurrence files sorted by path (`resolve.rs` sorts)  
- Graph projection sorts entries by IRI (`from_ir.rs:909–911`)  
- Emit wave order fixed  
- Seal net-off type-enforced  

**Breaks / soft risks:**
- Scratch dir names use PID + time (`scratch.rs:21–28`) — not in content hash, OK  
- Absolute paths inside RA Vfs — must continue projecting to relative `NudoxPath`  
- Proc-macro expansion nondeterminism edge cases  
- snix impure builtins (mitigated by DocsIO hermeticity tests)  
- `ToolchainSet.digest` hashes **path strings**, not toolchain binary contents — two machines with different fenix store paths get different JobKeys for same compiler version  
- HashMap iteration without sort would be nondeterministic — surface producers must emit sorted IR paths (graph path already sorts)

### 9.3 Symbol-Level Change Detection (gap analysis)

**Today:** package-granularity CAS only. One file change → full JobKey miss → full re-lower of all stages.

**Needed for librarification incrementality:**

1. **Per-file invalidation keys** — already have `FileDigest`; attach source hash to each `Cst` / occurrence file entry.  
2. **Per-symbol content hash** — postcard hash of `ir::kind::Entry` for graph/embed skip.  
3. **Index diff** — `NudoxPath` key set difference → `{added, removed, changed}`.  
4. **Reference fan-in** — walk `OccurrenceSet` targets to find dependents of changed symbols.  
5. **RA keep-alive** (library form only) — retain `RootDatabase` across file saves; incompatible with stateless k8s pod model (use package-level CAS there).  
6. **Downstream stores** — Tantivy/Qdrant/Terminus only re-write changed symbol ids (orchestration concern; compiler must emit stable ids + hashes).

---

## 10. Render Module & Ancillary Surfaces

### 10.1 Render (`workspace/compiler/render/`)

- **Purpose:** IR → human-readable surface syntax (reverse of producers)  
- **Architecture:** Wadler–Lindig doc algebra (`render/doc.rs`) + `Backend` trait + per-lang `emit/{rust,go,java,typescript,python,csharp,nix}.rs`  
- **Entry:** `render_entry` / `render_entry_doc` (`render/backend.rs` via `render/mod.rs:28–30`)  
- **Ambient state:** none — pure library; **belongs in compiler-core**  
- **Librarification use:** local GUI symbol preview without round-tripping to server

### 10.2 VCS / Traversal (`compile/vcs.rs`, per-lang `traversal.rs`)

- Shared `gix` plumbing for clone/fetch/materialize  
- Used more by acquisition/version resolution than by sealed compile of already-materialized roots  
- For dual-form: **acquisition belongs with client/index**, not sealed compiler pods that receive `FileBytes` / extracted roots

### 10.3 Heart Vocabulary (shared types the compiler already depends on)

| Type | File | Role |
|---|---|---|
| `ContentHash`, `JobKey` | `heart/content.rs` | CAS identity |
| `Cold` / `Live` / `Connect` | `heart/connection.rs` | Typestate pattern precedent for client stores |
| `Language`, `Toolchain` | `heart/ecosystem.rs` | Ecosystem tagging |
| `Coordinates` / package identity | `heart/package/`, `heart/identity/` | Package keys |
| `Cas`, `EvictableCas`, `Tiered`, `DiskCas`, `NoL3` | `heart/cache/` | Injected store |
| `DerivedStore` | `heart/sink.rs` | Fan-out trait for derived indexes |

Precedent for client library: **`Connect` typestate** (Cold→Live) mirrors `ForgeRuntime` Cold→Ready — keep this pattern for local REGISTRY open and remote INDEX connect.

---

## 11. Concrete Recommendations (Actionable)

1. **Split crates as in §8.1** without renaming the core vocabulary (`ForgeContext`, `generate_with`, `SealedInput`).  
2. **Stop exporting `daemon` from the library** (`lib.rs:21`).  
3. **Unify protocol** into one `compiler-wire` crate; delete postcard twin drift risk.  
4. **Inject `OracleSet` + explicit `ToolchainSet`** — kill `buck_resource` and embed-time `from_env`.  
5. **Feature-gate language backends** for desktop binary size (especially RA + snix + tsz).  
6. **Trusted path:** `TrustedForgeContext` + optional `TrustedPassthrough`; set `ThreatTier::Trusted` for first-party packages.  
7. **Untrusted path:** force `require_worker` + `Policy::Production` + bwrap; finish Java/C# sealed multi-step; put Rust in a worker.  
8. **Wire completeness:** extend compile response (or side-channel CAS put) for occurrences + archive digests so INDEX/REGISTRY do not re-walk trees.  
9. **Cold graph:** call `project` over IR blobs; reserve TerminusDB for hot packages only (aligns with terminus tiering research).  
10. **Incrementality v1:** per-file CST/occurrence invalidation using existing `FileDigest`; symbol hash later.  
11. **Fix daemon policy wiring:** currently hardcodes `Policy::Development` (`compiler_daemon.rs:67`) despite comments about production — production fleet must pass `Policy::Production` explicitly.  
12. **Hash toolchains by version content**, not only path strings, for cross-node CAS sharing.

---

## 12. Open Questions / Risks

| Risk | Severity | Notes |
|---|---|---|
| snix GPL in desktop binary | High | May force Nix producer out of GUI process or behind subprocess/feature |
| RA binary size + no cage | High | Untrusted pods currently run RA in-process without sandbox |
| `buck_resource` vs Cargo/GUI packaging | High | Oracles must be redistributed somehow for local Go/Java/C# |
| Toolchain digest = paths not content | Medium | Breaks multi-node CAS sharing |
| Daemon wire incomplete | Medium | Occurrences/archive not returned; server may recompute or drop |
| Protocol duplication drift | Medium | compiler vs registry copies |
| Adaptive producers | Medium | Phase 4 not done; hard to reason about seal guarantees |
| macOS isolation | Accepted for trusted | Production untrusted on macOS is non-goal |
| tsz / pyrefly pre-release churn | Medium | Pin carefully; feature-flag Tier C |
| Absolute path leaks into IR | Medium | Audit RA source map / NudoxPath projection continuously |
| Keeping RootDatabase alive | Design | Library incremental win vs server statelessness — **split by form** |

---

## Appendix A: File Reference Map

| Concern | Absolute path |
|---|---|
| Crate root | `/Users/philocalyst/Projects/Backend/workspace/compiler/lib.rs` |
| Pipeline entry | `.../generate/mod.rs` |
| Surface dispatch | `.../generate/surface.rs` |
| Producer trait + seal | `.../compile/producer/mod.rs` |
| ForgeContext / CAS | `.../compile/producer/runtime.rs` |
| Buck resources | `.../compile/producer/resource.rs` |
| Isolate seam | `.../compile/isolate.rs` |
| Wire protocol | `.../protocol.rs` |
| Daemon binary | `.../bin/compiler_daemon.rs` |
| Worker binary | `.../bin/producer_worker.rs` |
| ForgeRuntime | `.../daemon/forge.rs` |
| Sandbox lib | `.../sandbox/lib.rs` |
| Cage | `.../sandbox/cage.rs` |
| WorkerPool | `.../sandbox/worker.rs` |
| Toolchains | `.../sandbox/toolchains.rs` |
| Isolation policy | `.../sandbox/probe.rs` |
| ThreatTier / budget | `.../sandbox/budget.rs` |
| Graph model | `.../graph/model.rs` |
| Graph project | `.../graph/from_ir.rs` |
| Linker | `.../graph/link.rs` |
| Linked-data emit | `.../generate/linked_data/emit.rs` |
| CST | `.../generate/cst.rs` |
| Occurrences | `.../generate/occurrences.rs` |
| Resolver version | `.../generate/resolve.rs` |
| Parse/hash cache | `.../generate/parse_cache.rs` |
| Treesitter | `.../treesitter/mod.rs` |
| Render | `.../render/mod.rs` |
| Buck build | `.../BUCK` |
| ContentHash / JobKey | `/Users/philocalyst/Projects/Backend/workspace/heart/content.rs` |
| Connect typestate | `.../heart/connection.rs` |
| CAS tiers | `.../heart/cache/mod.rs` |

---

## Appendix B: Pipeline Serialization Cheat-Sheet

```
JobKey  = BLAKE3( len‖producer_version ‖ len‖toolchain_digest ‖ len‖source_hash ‖ len‖dep_lock )
CAS[job]              = postcard(ProducerOutput)   // via run_producer
CAS[job‖"cst"]        = postcard(CstSet)
CAS[job‖"occ-v1"]     = postcard(OccurrenceSet)
CAS[job‖"archive"]    = postcard(SourceArchive)
snapshot              = BLAKE3( for file in sorted: len‖path ‖ file_hash )
HTTP /compile body    = postcard(CompileRequest) → postcard(CompileResponse)
worker line           = JSON JobRequest → JSON JobResponse { body: Index JSON }
```

---

## Executive Summary

The live compiler at `workspace/compiler` is already halfway to the planned dual-form architecture. The critical abstractions exist and are wired: `ForgeContext` injects cage, CAS, toolchains, overrides, workers, and observer without process globals in the compile plane; `generate_with` is a pure(ish) function of `(ctx, PackageInput) → GeneratedPackage`; sealing goes through a typestate `Job::acquiring.seal(tier)` that **type-enforces network off**; stage outputs are postcard-encoded under content-addressed `JobKey`s derived from producer version, toolchain digest, source tree hash, and lockfile hash. The daemon binary (`compiler-daemon`) implements postcard `POST /compile`, Cold→Ready `ForgeRuntime`, and optional disk CAS — a real sealed-server skeleton, not a stub.

Gaps for librarification are specific, not architectural. **(1) Ambient edges remain:** `ToolchainSet::from_env` and `IsolationPolicy::require_worker` env reads, `buck_resource` via `current_exe` for Go/Java/C# oracles, and `NUDOX_TYPESCRIPT_ORACLE`. **(2) Sandbox is nested** at `workspace/compiler/sandbox` (not `workspace/sandbox`); production isolation is Linux+bwrap only — macOS correctly falls back to `DevPassthrough`, which is the trusted desktop path. **(3) Producers are uneven:** Go is the only fully `ExecPlan::Commands` sealed producer; TS/Python/Nix use WorkerPool with in-process fallback; Rust/Java/C# are “adaptive” and bypass the plan→execute template (Rust runs fully in-process with no cage). **(4) Daemon wire is incomplete:** full pipeline produces surface + CST + occurrences + archive + snapshot, but the HTTP response only returns postcard surface, CST-derived wire references, and identifier facets. **(5) Graph lowering is already pure** (`project` / `emit`) and can evaluate over IR blobs without TerminusDB for cold packages. **(6) Treesitter trees are never stored** — only `ResolvedReference` spans and occurrence corpora; reparse-on-demand is intentional and cheap. **(7) Incrementality is package-scoped** today; symbol-level change detection needs per-entry hashes and index diffs layered on existing digests. **(8) `render/` is pure and should ship in the embedded library** for local previews.

Recommended dual-form split: `compiler-core` (pipeline + graph + render + treesitter + producers), `compiler-wire` (protocol), `compiler-daemon` + `producer-worker` binaries, existing `sandbox`, and a thin `TrustedForgeContext` for the GUI/client. Keep existing names (`ForgeContext`, `generate_with`, `SealedInput`, `JobKey`). Do not rename the DAEMON-PLAN vocabulary — complete its injection surface (`OracleSet`, explicit toolchains, trusted cage) and seal the remaining adaptive producers for the untrusted fleet.

---

*Report re-verified 2026-07-16 against live sources under `workspace/compiler`, `workspace/compiler/sandbox`, and `workspace/heart`. No build attempted.*

---

## Edge-tech: fleet stage CAS (pointer)

Compiler pods should share **immutable** JobKey stage blobs (surface/CST/occurrences/archive) via remote CAS L3, not language `target/` caches across untrusted tenants. Full design: [edge-tech/06-compiler-fleet-cache](../../edge-tech/06-compiler-fleet-cache/PLAN.md). Orch addendum: [12-orchestration § Compiler fleet cache](./12-orchestration/PLAN.md).
