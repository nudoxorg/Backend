# Desktop Toolchain / Oracle Packaging

**Research date:** 2026-07-16  
**Scope:** How the trusted desktop GUI ships, discovers, and injects language producers and host SDKs **without Buck**, under size/offline constraints, while preserving `JobKey` integrity.  
**Feeds:** dual-form librarification — **trusted local embed** (desktop registry) vs **untrusted k8s fleet** (sealed daemon).  
**Depends on:** `01-compiler-audit` (producer map, ambient edges, `ToolchainSet`, `buck_resource`, snix GPL note, worker isolation), `03-gui-audit` (GPUI shell, `nudox-indexer` PATH launch), `08-incremental` (`JobKey` / producer version domains).  
**Out of scope:** k8s image baking details beyond contrast; building the app; legal opinion as counsel (options only).

> **Path note:** Live code is `workspace/compiler` (+ nested `sandbox`). Oracles currently resolve via Buck `resources` + `<exe>.resources.json` (`compile/producer/resource.rs`). Desktop has **no** packaging plan today — this document fills that gap.

---

## 0. Mission Checklist

| # | Question | Section |
|---|---|---|
| 1 | Producer inventory: in-proc vs subprocess; absolute deps; size; license | §1 |
| 2 | Packaging options (ship / Nix / system / on-demand / flags) | §2 |
| 3 | Feature-gating / language packs so base GUI is not multi-GB | §3 |
| 4 | Explicit `ToolchainSet` / `OracleSet` injection (no ambient `from_env` in library) | §4 |
| 5 | snix GPL contamination risk for desktop distribution | §5 |
| 6 | Desktop worker process model (RA crash isolation, etc.) | §6 |
| 7 | Versioning: `producer_version` + toolchain digest ↔ `JobKey` | §7 |
| 8 | MVP languages desktop v1 vs remote-only | §8 |
| 9 | Prior art (RA, VS Code LS, JetBrains, sccache, Nix apps) | §9 |
| — | Concrete recommendations + open questions | §10–11 |

---

## 0.1 Problem Statement

Today the compiler assumes a **Buck-built, Nix-devshell host**:

1. **Oracles** (Go/Java/C#) resolve only through `buck_resource(name)` reading `<current_exe>.resources.json` next to the binary (`workspace/compiler/compile/producer/resource.rs:21–52`). Comment states explicitly: *“no cargo fallback… Buck2 is the only way these binaries get built.”*
2. **Host SDKs** enter via `ToolchainSet::from_env()` (`sandbox/toolchains.rs:55–71`) — `NUDOX_TOOLCHAIN_*`, `RUSTUP_HOME`/`CARGO_HOME`, `JAVA_HOME`, `GOROOT`/`GOPATH`, `NUDOX_TOOLCHAIN_PATH`.
3. **Library convenience path** `LocalForgeContext::new()` always calls `ToolchainSet::from_env()` (`compile/producer/runtime.rs:243`) — ambient for tests, **illegal** for a pure embedded library.
4. **Daemon assemble** likewise (`daemon/forge.rs:153`) — fine for server binary, wrong pattern for GUI if GUI just reuses `new()`.
5. **GUI** (`workspace/gui`) today shells `nudox-indexer` from PATH (`local_index_panel.rs:80–86`); it does **not** embed the compiler, ship oracles, or configure toolchains.
6. **Size:** linking RA + OXC + pyrefly + tsz + snix + arborium into one GPUI process without feature gates yields a multi-hundred-MB binary and multi-GB *effective* footprint once host SDKs are considered (full rustup ~1.5–2.3 GiB; Go ~240 MiB; JDK ~340 MiB; .NET SDK ~700 MiB on this host).

**Trusted packages compile locally inside the desktop app. Untrusted packages compile on the k8s fleet.** This research answers how local compilation is *resourced*.

---

## 1. Producer Inventory

Source of truth: `01-compiler-audit` §4 re-verified 2026-07-16 against live producers. IDs from `const ID: ProducerId`.

### 1.1 Summary Matrix

| Language | ProducerId | Mode | Exec path | Cage today | Absolute runtime deps | Ship artifact(s) | Link-in size OOM | Host SDK OOM (this machine) | License (producer stack) |
|---|---|---|---|---|---|---|---|---|---|
| **Rust** | `rustdoc/3` (RA impl) | Adaptive **in-process** | `lower_in_process` → `ra_ap_*` | **None** | cargo metadata; **sysroot** via `RustLibSource::Discover`; **proc-macro-srv** optional | *none* (library) | **High:** tens of MB code; RA bin alone ~40–44 MiB | rustup toolchain **1.4–2.3 GiB** (not all needed) | MIT/Apache (ra_ap, rustc libs) |
| **TypeScript** | `oxc/1` | Library worker / in-proc | OXC; optional **tsz** Tier C | Worker if pooled | *none* (pure Rust) | *none* | **Medium** (OXC); **High** if tsz linked | none | MIT (oxc); tsz TBD/vendored |
| **Python** | `pyrefly/1` | Library worker / in-proc | pyrefly + ruff_python_ast | Worker if pooled | *none* | *none* | **Medium** | none (typeshed may be desirable later) | MIT (pyrefly) |
| **Go** | `go-oracle/1` | `ExecPlan::Commands` | go-oracle binary | Sealed command | **go-oracle** path + host **`go`** (GOROOT via toolchain) for `golang.org/x/tools` types | **go-oracle** (~5–15 MiB static-ish) | tiny (Rust driver) | **Go 1.x ~240 MiB** full GOROOT | BSD (Go); our oracle BSD/MIT-ish |
| **Java** | `javadoc/1` | Adaptive + IsolatedCommand | `javadoc -doclet` | Partial (javadoc only) | **java-oracle.jar** + host **`javadoc`/`java`** (JAVA_HOME) | **java-oracle.jar** (~tens of KB–few MB) | tiny | **JDK 21 ~340 MiB** (headless similar) | Oracle/OpenJDK terms; doclet own code |
| **C#** | `roslyn-oracle/1` | Adaptive + IsolatedCommand | `dotnet oracle.dll` | Partial (dotnet only) | **csharp-oracle/** publish dir + host **`dotnet`** | **oracle.dll** + deps (publish dir, ~10s MiB) | tiny | **.NET SDK 10 ~700 MiB** | MIT (.NET / Roslyn); our oracle own |
| **Nix** | `snix/1` | Library worker / in-proc | snix_eval + rnix | Worker if pooled | *none* | *none* (or **GPL worker bin**) | **Medium** + **GPL** | none | **GPL-3.0 (snix)**; rnix permissive |

Producer profiles / memory ceilings: `sandbox/profiles.rs` — Rust 6 GiB, C# 4 GiB, Go/Java 2 GiB, Nix/StaticParser 1 GiB.

### 1.2 Rust — in-process RA library

| | |
|---|---|
| **Files** | `compile/rust/producer.rs`, `compile/rust/ra/load.rs` |
| **Stack** | `ra_ap_load_cargo`, `ra_ap_hir`, `ra_ap_ide_db`, `ra_ap_vfs`, … pin **0.0.341** (`BUCK:35–45`) |
| **Mode** | Adaptive: `plan` errors with multi-crate; `produce` → `lower_in_process` (`producer.rs:41–65`) |
| **Sysroot** | `CargoConfig { sysroot: Some(RustLibSource::Discover), … }` (`ra/load.rs:140`) — discovers rustc sysroot from env/PATH, **not** from `ToolchainSet` fields alone |
| **Proc macros** | Default `ProcMacroPolicy::Sysroot` → `rust-analyzer-proc-macro-srv` from sysroot (`load.rs:41, 50–58`); failure degrades (warn, missing macro items) |
| **Offline** | `CARGO_NET_OFFLINE=true` default (`load.rs:149`); still needs local `CARGO_HOME` registry for deps already present |
| **Env consumed** | Indirectly via cargo: `RUSTUP_HOME`, `CARGO_HOME`, `PATH` to rustc/cargo; applied if `ToolchainSet` sets them (`toolchains.rs:86–91`) |
| **Size** | Vendor sources for `ra_ap_*` ~17 MiB raw; **linked** release contribution estimated **30–80 MiB** (order-of-magnitude; RA standalone ~40 MiB). Runtime RSS on medium workspaces **multi-GiB** (profile allows 6 GiB) |
| **License** | rust-analyzer / ra_ap: Apache-2.0 OR MIT |
| **Desktop implication** | Must either (a) link RA into app/worker and require a **discovered or shipped** rustc sysroot, or (b) remote-only for Rust. Cannot “just ship RA” without cargo+sysroot for real crates |

### 1.3 TypeScript — OXC (+ optional tsz)

| | |
|---|---|
| **ID** | `oxc/1` (`typescript/producer.rs:21`) |
| **Mode** | `ExecPlan::Library(WorkerLang::Typescript)` — worker or in-process |
| **Tier C** | `NUDOX_TYPESCRIPT_ORACLE` ambient read in `tsz::enabled()` (`oracle/tsz.rs:57–64`) — **librarification gap** |
| **Deps** | Pure Rust: oxc_* , optional tsz_* git-vendored (`BUCK:61–83`) |
| **Size** | OXC: moderate. tsz monorepo: **large** if always linked — must be feature-gated |
| **License** | OXC: MIT. tsz: verify at pin; treat as separate feature |
| **Desktop** | Best pure-Rust ship candidate after feature-split from tsz |

### 1.4 Python — pyrefly

| | |
|---|---|
| **ID** | `pyrefly/1` |
| **Mode** | `ExecPlan::Library(WorkerLang::Python)` |
| **Stack** | `pyrefly`, `pyrefly_*`, `ruff_python_ast` (`BUCK:53–59`); context in `compile/python/context.rs` |
| **Host** | None required for baseline lower |
| **License** | Pyrefly: **MIT** ([facebook/pyrefly](https://github.com/facebook/pyrefly)) |
| **Desktop** | Strong MVP candidate; size medium; isolate in worker for crash safety |

### 1.5 Go — sealed command + oracle binary

| | |
|---|---|
| **ID** | `go-oracle/1` |
| **Oracle** | `//workspace/compiler/compile/go/oracle:oracle` — Go module `nudox.org/compiler/languages/go/oracle`, deps `golang.org/x/tools` (`go.mod`) |
| **Resolve** | `package::oracle_binary()` → `buck_resource("go-oracle")` (`compile/go/package.rs:29`) |
| **Host** | `go` on hermetic PATH; `GOROOT` via `ToolchainSet`; oracle env: `GOWORK=off`, `GOFLAGS=-mod=mod`, `GOPROXY=off` (`go/producer.rs:44–46`) |
| **Oracle source size** | ~1k LOC Go — binary typically **~5–15 MiB** stripped (estimate; not measured prebuilt here) |
| **GOROOT** | Live nix go-1.26.4 store path **~240 MiB** |
| **Desktop** | Ship platform-triple `go-oracle`; inject path via `OracleSet`. Prefer **system/Nix go** discovery over shipping full GOROOT in base app |

### 1.6 Java — javadoc doclet jar

| | |
|---|---|
| **ID** | `javadoc/1` |
| **Oracle** | `java-oracle.jar` from `compile/java/oracle` (`Extractor.java` ~900 LOC); JDK 17+ APIs (records, pattern matching, permitted subclasses) |
| **Invoke** | `javadoc -doclet nudox.oracle.Extractor -docletpath <jar> …` (`java/oracle.rs:10–12`) |
| **Resolve** | `buck_resource("java-oracle.jar")` (`oracle.rs:35`) |
| **Host** | `javadoc` + JDK; Markdown docs need JDK ≥ 23 for full parse (`oracle.rs:19–21`) — recommend **JDK 21+** baseline, accept doc null for `///` on older |
| **JDK size** | Zulu CA JDK 21 on this host **~342 MiB** |
| **Desktop** | Ship jar always with Java language pack; **never** ship full JDK in base — system discovery or optional download pack |

### 1.7 C# — Roslyn oracle publish directory

| | |
|---|---|
| **ID** | `roslyn-oracle/1` |
| **Oracle** | `dotnet publish` output dir (`csharp/oracle/BUCK`); resource key `csharp-oracle` |
| **Invoke** | `dotnet <publish>/oracle.dll --mode source --root …` (`csharp/oracle.rs`) |
| **Host** | .NET **SDK** (not just runtime) for Roslyn surface analysis; live SDK 10 **~707 MiB** |
| **Desktop** | Heaviest optional language pack; remote-only for v1 is rational |

### 1.8 Nix — snix eval (GPL)

| | |
|---|---|
| **ID** | `snix/1` |
| **Mode** | `ExecPlan::Library(WorkerLang::Nix)` |
| **Stack** | `snix_eval` (git fragment @ `50b41ae…`), `rnix`, `rowan` (`BUCK:89–92`) |
| **Hermeticity** | DocsIO + pure builtins; tests `nix_env_leak.rs` |
| **License** | Snix crates: **GPL-3.0** ([cachix/snix README — License structure](https://github.com/cachix/snix): *“linking to it, embedding the evaluator… fall under the terms of the GPL3”*). Protocol buffers MIT only. |
| **Desktop** | **Must not link into proprietary GUI process** without accepting GPL on the whole app. Options in §5 |

### 1.9 Shared non-producer deps (always in pipeline)

| Component | Role | Size / note |
|---|---|---|
| arborium + tree-sitter grammars | CST + occurrences for all langs | Feature-flag per grammar; multi-MB if all enabled |
| `render/` | IR → syntax preview | Pure, small — ship in core |
| `graph/` | IR → GraphCorpus | Pure — ship in core |
| `producer-worker` bin | Isolate TS/Py/Nix (and future) | Separate process; can carry GPL snix without infecting GUI |

### 1.10 Absolute dependency classes

```
Class A — pure Rust libraries (link or worker)
  RA (ra_ap_*), OXC, pyrefly, tsz?, snix?, arborium

Class B — small ship-with-app native oracles (no full SDK)
  go-oracle, java-oracle.jar, csharp-oracle/ publish dir

Class C — large host SDKs (discover, optional pack, or remote-only)
  rustc/cargo/sysroot (+ rustup), GOROOT/go, JDK/javadoc, dotnet SDK

Class D — process isolation infrastructure
  producer-worker binary, optional TrustedPassthrough / DevPassthrough cage
```

Desktop packaging is almost entirely about **B + C policy** and **A feature gates**, with **D** for crash isolation and GPL quarantine.

---

## 2. Packaging Options (macOS / Linux / Windows)

Platform matrix from flake: `aarch64-darwin`, `x86_64-darwin`, `aarch64-linux`, `x86_64-linux` (`flake.nix:55–60`). Windows is **not** in the supported systems list today — treat as future; still note packaging patterns.

### 2.1 Option matrix

| Strategy | What ships in app bundle | Who provides Class C SDKs | Offline | Offline quality | Complexity | Fits trusted desktop? |
|---|---|---|---|---|---|---|
| **A. Ship-in-app** | GUI + worker + oracles + *subset* SDKs | App vendor | Yes if SDK shipped | High if complete | High size; notarization | Only for tiny Class B; SDKs too big |
| **B. Nix-provided** | GUI thin; toolchains from `/nix/store` or profile | Nix on machine / Determinate / devshell | Yes if store populated | Highest hermeticity | Requires Nix install | Excellent for power users / internal |
| **C. System discovery** | GUI + worker + oracles only | User-installed rustup/go/jdk/dotnet | Partial | Matches user toolchain | Lowest ship size | **Default for MVP** |
| **D. Download-on-demand** | GUI shell; language packs + pinned SDK tarballs from CDN | App downloader into `Application Support` | After download | High if packs pinned | CDN, verify digests, updates | **v1.5+ language packs** |
| **E. Language feature flags (build-time)** | Only compiled-in producers | any of B–D | n/a | n/a | Cargo features / Buck selects | **Mandatory** regardless of A–D |
| **F. Hybrid (recommended)** | Base: GUI + core + TS/Py (+ optional Go oracle) | System discover for Rust/Go/Java; packs optional | Base offline for pure-Rust langs | Good | Medium | **Yes — §8** |

### 2.2 Platform-specific notes

#### macOS (primary desktop target)

| Concern | Guidance |
|---|---|
| Bundle layout | `Nudox.app/Contents/MacOS/{nudox,producer-worker}` + `Contents/Resources/oracles/{go-oracle,java-oracle.jar,csharp-oracle/}` |
| Code signing / notarization | All Mach-O oracles + worker must be signed with same team ID; hardened runtime may need entitlements for JIT (dotnet) / DYLD |
| Sandbox (App Store) | **Out of scope for v1** — local indexing needs broad FS + subprocess; distribute outside MAS or with temporary exceptions |
| Isolation | No bwrap; `DevPassthrough` / `TrustedPassthrough` only (`01` §5.3) — acceptable for trusted packages |
| Discovery | `/opt/homebrew`, `/usr/local`, `~/.cargo`, `~/.rustup`, `/Library/Java/JavaVirtualMachines`, Nix `/nix/store` |

#### Linux

| Concern | Guidance |
|---|---|
| Bundle | AppImage / flatpak / bare tarball with `bin/` + `share/nudox/oracles/` |
| Optional production cage | If ever running untrusted local, bwrap available — **not required for trusted desktop** |
| Distro JDKs / go | Prefer explicit path injection over PATH-only (reproducible digests) |

#### Windows (future)

| Concern | Guidance |
|---|---|
| Oracles | `.exe` go-oracle; jar; `oracle.dll` under `dotnet` |
| Paths | `ToolchainSet` must use proper path encoding (already `OsStr::as_encoded_bytes` in digest — OK) |
| Not in flake matrix | No CI until product decision |

### 2.3 Ship-in-app details (Class B only)

Existing **server** packaging already copies oracles next to the daemon (`workspace/compiler/package.nix:88–151`) and writes `compiler-daemon.resources.json` for `buck_resource`. Desktop must replace Buck resolution with **explicit paths**, but can **reuse the same layout**:

```
$APP_ROOT/
  bin/nudox                  # GPUI GUI
  bin/producer-worker        # optional language isolates
  share/nudox/
    oracles/
      go-oracle
      java-oracle.jar
      csharp-oracle/         # publish dir
    manifests/
      oracles.v1.json        # versions + blake3 digests
```

`package.nix` is the **reference implementation** of oracle materialization; port to a desktop `package-desktop.nix` / cargo-bundle step that does not depend on `current_exe().resources.json`.

### 2.4 Nix-provided toolchains

Devshell already wires (`flake.nix:347–356`):

- `go.go_binary` → nixpkgs go  
- `java.java_home` → `jdk21_headless`  
- `csharp.dotnet` → `dotnetCorePackages.sdk_10_0`  
- fenix rust toolchain  

Desktop assembly options:

1. **Internal builds:** GUI launched from `nix develop` with `NUDOX_TOOLCHAIN_*` exported — good for developers, not end users.  
2. **End-user Nix:** optional “use Nix profile” setting pointing at a flake-provided `nudox-toolchains` package that exposes a single root with `rust|go|jdk|dotnet` symlinks; `ToolchainSet` filled from that root.  
3. **Deterministic digests:** prefer hashing **content** of `rustc -vV` / `go env GOROOT` version files over raw store path strings (§7).

### 2.5 System discovery (MVP default)

Discovery order (recommended library API — **assemble-time only**, never inside `produce`):

```
for each toolchain field:
  1. Explicit user settings (JSON / GUI settings)
  2. NUDOX_TOOLCHAIN_* (dev override; binary/host only)
  3. Well-known installers:
       rust:  rustup which rustc → sysroot; RUSTUP_HOME/CARGO_HOME
       go:    `go env GOROOT` if go on PATH
       java:  JAVA_HOME, /usr/libexec/java_home (macOS), update-alternatives
       dotnet: DOTNET_ROOT, `dotnet --info`
  4. Empty → language unavailable (clear UI error, offer remote)
```

**Never** ambient-scan PATH inside sealed producers. Discovery produces a `ToolchainSet` + `OracleSet` once at `TrustedForgeContext::assemble`.

### 2.6 Download-on-demand language packs

JetBrains/VS Code pattern (see §9): base IDE small; language support as packs.

Suggested pack IDs:

| Pack ID | Contents | Approx download |
|---|---|---|
| `core` | GUI, compiler-core (no RA/snix), arborium rust+ts+py grammars, producer-worker base | 50–150 MiB |
| `lang-rust` | RA linked in `producer-worker-rust` **or** dylib; no full sysroot | +40–100 MiB |
| `lang-go` | go-oracle + optional minimal GOROOT **or** require system go | +10–250 MiB |
| `lang-java` | java-oracle.jar; optional Temurin 21 JRE/JDK pack | +5 MiB / +300 MiB |
| `lang-csharp` | csharp-oracle publish; optional .NET SDK pack | +20 MiB / +700 MiB |
| `lang-nix` | **GPL** `producer-worker-nix` only | +20–50 MiB |
| `sdk-rust-sysroot` | pinned fenix/rustup minimal profile | +300–800 MiB |

Each pack ships `manifest.json`:

```json
{
  "pack": "lang-go",
  "version": "1.0.0",
  "producer_ids": ["go-oracle/1"],
  "artifacts": [
    {"path": "oracles/go-oracle", "blake3": "…", "os": "aarch64-darwin"}
  ],
  "requires_host": ["go>=1.22"],
  "toolchain_fingerprint_keys": ["go_root", "go_version"]
}
```

Install root: macOS `~/Library/Application Support/nudox/packs/`, Linux `$XDG_DATA_HOME/nudox/packs/`.

### 2.7 Offline use policy

| Mode | Behavior |
|---|---|
| **Strict offline** | Only packs already on disk + system toolchains; no CDN; pure-Rust langs (TS/Py) work; Rust needs local sysroot; no untrusted remote |
| **Online optional** | Download language packs; still no compile-time network inside producers (`NetGrant::Off`, `GOPROXY=off`, `CARGO_NET_OFFLINE`) |
| **Hybrid trust** | Trusted local packages offline; untrusted → remote fleet (needs network for API, not for local toolchains) |

Seal path already forbids compile network (`01` §2.4). Offline packaging must not reintroduce acquisition into the producer.

---

## 3. Feature-Gating Strategy (Language Packs at Build Time)

### 3.1 Why build-time gates are mandatory

Linking everything in `workspace/compiler/BUCK` deps into the GUI process:

- RA alone dominates compile time and binary size  
- snix pulls **GPL** into the link graph  
- tsz is pre-release and heavy  
- Most users need 1–3 languages  

### 3.2 Proposed Cargo / Buck features

```
compiler-core (always)
  + feature "lang-rust"     → ra_ap_* , rust producer
  + feature "lang-typescript" → oxc_* , ts producer
  + feature "lang-python"   → pyrefly* , python producer
  + feature "lang-go"       → go producer (oracle path only; no go SDK)
  + feature "lang-java"     → java producer
  + feature "lang-csharp"   → csharp producer
  + feature "lang-nix"      → snix_eval (prefer worker-only crate)
  + feature "oracle-tsz"    → tsz_* (typescript enrichment)
  + feature "grammars-all"  → all arborium grammars
```

**GUI default feature set (MVP):**  
`lang-typescript, lang-python, lang-rust, lang-go` + core grammars.  
**Exclude:** `lang-nix` (GPL), `lang-java`, `lang-csharp`, `oracle-tsz`.

### 3.3 Runtime language packs vs compile features

| Layer | Mechanism | User experience |
|---|---|---|
| Compile-time | Cargo features / separate worker binaries | Determines *possible* languages in this build channel |
| Install-time | Download packs into Application Support | Enables language after install without redownload of whole app |
| Run-time | `OracleSet` / `ToolchainSet` presence + feature registry | UI greys out languages missing SDK or pack |

### 3.4 Size budgets (targets — order-of-magnitude)

| Build | Target compressed download | Rationale |
|---|---|---|
| GUI base (no RA) | **≤ 80 MiB** | GPUI + core + OXC/pyrefly optional |
| GUI + RA (rust pack merged) | **≤ 150 MiB** | Competitive with small IDEs' language extensions |
| + go-oracle | **+10 MiB** | Class B |
| Full kitchen-sink + SDKs | **multi-GB** | Never the default channel |

If base exceeds 200 MiB compressed, **split RA into `producer-worker-rust`** so GUI stays thin.

### 3.5 Surface dispatch gating

`generate/surface.rs` ecosystem match must become:

```rust
// Pseudocode — missing feature → ProducerError::unsupported(lang)
match ecosystem {
  Rust if cfg!(feature = "lang-rust") => …
  … 
  other => Err(unsupported)
}
```

Remote fleet builds enable **all** non-GPL-sensitive features (or enable snix only in worker image).

---

## 4. Explicit ToolchainSet / OracleSet Injection API

### 4.1 Status today (blockers)

| API | Location | Problem for desktop |
|---|---|---|
| `ToolchainSet::from_env` | `sandbox/toolchains.rs:55` | Sanctioned sealer read — OK for **binary assemble**, not for library mid-call |
| `LocalForgeContext::new` | `runtime.rs:224–249` | Always `from_env` — embeds ambient into library path |
| `ForgeRuntime::assemble` | `daemon/forge.rs:153` | Correct for daemon; pattern to mirror with **explicit** args |
| `buck_resource` | `resource.rs:21` | Buck-only; breaks GUI/Cargo packaging |
| `tsz::enabled` | `tsz.rs:60` | Ambient `NUDOX_TYPESCRIPT_ORACLE` |
| `ForgeContext` trait | `runtime.rs` | **No** `oracles()` method yet (`01` §8.3 sketch) |

### 4.2 Target types

```rust
/// Class B — paths to prebuilt language oracles (desktop or server).
#[derive(Debug, Clone, Default)]
pub struct OracleSet {
    pub go_oracle: Option<PathBuf>,
    pub java_oracle_jar: Option<PathBuf>,
    pub csharp_oracle_dir: Option<PathBuf>,
    /// Optional: explicit producer-worker binary (TS/Py/Nix/Rust workers).
    pub producer_worker: Option<PathBuf>,
    /// Optional: rust-specific worker (if RA not in GUI process).
    pub rust_worker: Option<PathBuf>,
    /// Optional: GPL-isolated nix worker (never linked into GUI).
    pub nix_worker: Option<PathBuf>,
}

impl OracleSet {
    pub fn empty() -> Self { Self::default() }

    /// Load from a desktop layout root (`share/nudox/oracles`, manifests).
    pub fn from_install_root(root: &Path) -> io::Result<Self> { /* … */ }

    /// Dev-only: Buck resources next to current_exe (tests/daemon image).
    #[cfg(feature = "buck-resources")]
    pub fn from_buck_resources() -> io::Result<Self> { /* wrap buck_resource */ }

    pub fn digest(&self) -> ContentHash {
        // path strings OR better: content hashes of oracle binaries (§7)
    }
}

/// Class C — host SDKs / sysroots (already exists; extend).
// ToolchainSet { rustup_home, cargo_home, java_home, go_root, go_path, path_dirs }
// ADD:
//   rustc_sysroot: Option<PathBuf>,  // explicit; stop relying only on Discover
//   dotnet_root: Option<PathBuf>,
//   versions: ToolchainVersions,     // for content-ish fingerprinting
```

### 4.3 ForgeContext extensions

```rust
pub trait ForgeContext {
    // … existing: node, cage, cas, toolchains, overrides, observer, handle,
    //             worker_pool, require_worker …
    fn oracles(&self) -> &OracleSet;
    fn typescript_oracle_enabled(&self) -> bool; // replaces NUDOX_TYPESCRIPT_ORACLE
}

pub struct TrustedForgeContext { /* owns cage + cas + toolchains + oracles */ }

impl TrustedForgeContext {
    /// Library-safe: no env reads.
    pub fn assemble(opts: TrustedForgeOptions) -> Result<Self, ForgeError>;
}

pub struct TrustedForgeOptions {
    pub toolchains: ToolchainSet,      // caller-filled
    pub oracles: OracleSet,
    pub cas_root: Option<PathBuf>,
    pub cage: TrustedCageKind,         // TrustedPassthrough | DevPassthrough
    pub require_worker: bool,          // default false for trusted local
    pub typescript_oracle: bool,
    pub worker_pools: WorkerPoolPolicy,
}

/// Host/binary only — may read env + discover.
pub fn discover_toolchains(policy: DiscoverPolicy) -> ToolchainSet;
pub fn discover_oracles(app_root: &Path) -> OracleSet;
```

### 4.4 Call-site rewrites

| Current | New |
|---|---|
| `buck_resource("go-oracle")` | `ctx.oracles().go_oracle.clone().ok_or(…)` |
| `buck_resource("java-oracle.jar")` | `ctx.oracles().java_oracle_jar` |
| `buck_resource("csharp-oracle")` | `ctx.oracles().csharp_oracle_dir` |
| `LocalForgeContext::new()` | Keep for tests with `from_env`; add `new_with(opts)` |
| GUI indexer | `TrustedForgeContext::assemble(discover_*(…))` then `generate_with` |

### 4.5 Trusted vs production policy

| | Trusted desktop | Production fleet |
|---|---|---|
| Cage | `TrustedPassthrough` / `DevPassthrough` | `LinuxNamespaces` + bwrap |
| `require_worker` | false by default; **true** for Hostile (TS/Py) optional; **always** for Nix GPL worker | true for Hostile; RA should move to worker |
| Toolchains | discovered / packs | image-pinned store paths |
| Oracles | install root / packs | `package.nix` resources manifest |
| Env reads | **only** in GUI/bootstrap discover | only in `ForgeRuntime::assemble` |
| Network in produce | Off | Off |

`ThreatTier::Trusted` exists (`budget.rs`) but no producer uses it yet — first-party user projects should map to Trusted + TrustedPassthrough.

### 4.6 Settings surface (GUI)

Extend `NudoxSettings` (`03-gui-audit` / `settings.rs`) with:

```json
{
  "toolchains": {
    "rustup_home": null,
    "cargo_home": null,
    "rustc_sysroot": null,
    "java_home": null,
    "go_root": null,
    "dotnet_root": null,
    "extra_path": []
  },
  "oracles": {
    "root": null
  },
  "languages": {
    "rust": "local",
    "typescript": "local",
    "python": "local",
    "go": "local",
    "java": "remote",
    "csharp": "remote",
    "nix": "remote"
  },
  "typescript_oracle": false
}
```

`languages.*.local|remote|off` is the product policy lever for §8.

---

## 5. snix GPL Contamination — Options

### 5.1 Legal fact pattern (not legal advice)

- Snix evaluator crates are **GPL-3.0**; upstream states embedding/linking falls under GPL3 ([snix license structure](https://github.com/cachix/snix)).  
- GUI (`lindsey` / future product) is presumed **proprietary or non-GPL**.  
- Static or dynamic linking of `snix_eval` into the GUI binary creates a combined work under GPL obligations (source offer, etc.).  
- Separately distributed GPL programs that communicate at arm's length (pipes, HTTP) are a common **isolation** pattern; strength depends on coupling (counsel review required).

### 5.2 Options

| Option | Description | Product impact | Risk |
|---|---|---|---|
| **A. Exclude from desktop** | No `lang-nix` feature in GUI channel; Nix packages always **remote** fleet | Simple; Nix users need network | **Recommended default** |
| **B. GPL worker subprocess** | `producer-worker-nix` binary linked with snix; GUI speaks JSON line protocol only; ship under GPL with source offer for **that binary** | Local Nix indexing works; GUI stays non-GPL *if* counsel accepts process boundary | Medium — review needed |
| **C. GPL the desktop** | Dual-license or GPL entire app | Unlikely product choice | Low legal risk, high business risk |
| **D. Replace snix** | rnix-only static parse (no eval) for desktop; snix on server | Weaker Nix IR locally | Engineering cost |
| **E. Lazy side-load GPL .so** | dlopen snix plugin | Still often considered linking; worse UX | High risk / fragile |

### 5.3 Recommendation

1. **MVP:** Option **A** — Nix remote-only.  
2. **v1.x optional:** Option **B** as optional download pack `lang-nix` with clear UI (“GPL-3.0 component”) + source tarball on CDN.  
3. **Never** enable `lang-nix` feature in the main GUI link.  
4. Fleet images may link snix into `producer-worker` (already library form) under controlled license compliance for server distribution.

### 5.4 Worker quarantine sketch

```
GUI (proprietary)
  └─ spawn: producer-worker-nix (GPL-3.0)
        stdin/stdout: JobRequest/JobResponse JSON (existing protocol)
        no shared memory, no plugins into GUI
```

`OracleSet.nix_worker` points at that binary; `WorkerLang::Nix` uses it exclusively; `require_worker` forced true for Nix on desktop.

---

## 6. Desktop Worker Process Model

### 6.1 Why workers on a trusted machine?

Trusted ≠ immortal. RA OOM (profile **6 GiB**), pyrefly panics, OXC bugs, snix eval bombs must **not** take down the GPUI UI thread/process.

| Producer | Today | Desktop target |
|---|---|---|
| TS / Python / Nix | WorkerPool optional (`require_worker` false in LocalForge) | **Prefer worker** even when trusted |
| Rust | In-process only | **Phase 1:** in-process OK with catch_unwind + memory watchdog; **Phase 2:** `producer-worker-rust` |
| Go / Java / C# | Already subprocess (oracle) | Keep; ensure GUI doesn't share address space |

### 6.2 Existing machinery

- `bin/producer_worker.rs` — `lower` / `serve`; langs **nix|typescript|python** only  
- `sandbox/worker.rs` — free-list pool, rlimits, JSON protocol, RSS watermark 768 MiB  
- `ForgeRuntime` warms nix + parser pools when `NUDOX_PRODUCER_WORKER` / discovery finds binary (`forge.rs:156–162`)

### 6.3 Desktop architecture

```
┌─────────────────────────────────────────────┐
│  Nudox GUI (GPUI)                           │
│   TrustedForgeContext                       │
│   CAS (disk under Application Support)      │
│   ToolchainSet + OracleSet (injected)       │
└───────────┬─────────────────────────────────┘
            │ generate_with / run_producer
            │
    ┌───────┴────────┬──────────────┬──────────────┐
    ▼                ▼              ▼              ▼
 worker-ts/py    worker-rust*   go-oracle      javadoc/dotnet
 (optional)      (phase 2)      (cmd)          (cmd)
    │
 worker-nix (GPL pack only)
```

\* Phase 2 rust worker needs protocol extension: today worker only returns JSON `Index`; Rust also wants `AuxOutputs.source_map` — either extend protocol or accept source_map only in-process.

### 6.4 Lifecycle policies

| Policy | Value |
|---|---|
| Pool size | 1–2 per language on laptop (vs server 2+) |
| Restart | On crash, non-zero exit, RSS watermark |
| Cancel | `CancelToken` already in cage path; wire GUI cancel button |
| Priority | Below UI; use `nice` / QoS user-initiated on macOS |
| Scratch | Per-job temp under `~/Library/Caches/nudox/scratch` |

### 6.5 GUI integration gap

`local_index_panel.rs` currently spawns external `nudox-indexer` via PATH — a **proto** isolation model. Librarification should:

1. Replace `nudox-indexer` with in-tree `TrustedForgeContext` + optional workers, **or**  
2. Keep a thin `nudox-indexer` CLI that is the same binary as `producer-worker` / compile driver for crash isolation from GPUI.

Recommendation: **prefer separate indexer helper process** for v1 (crash isolation + simpler GPUI), sharing code with library via crate; not PATH-scraped forever — resolve via app bundle path.

---

## 7. Versioning: producer_version + toolchain digest ↔ JobKey

### 7.1 Current formula

From `heart/content.rs:62–74` and `generate/mod.rs:106–111`:

```
JobKey = BLAKE3(
  len‖ PRODUCER_VERSION        // "nudox-producer/2"  producer/mod.rs:281
  len‖ toolchains.digest()     // path-string digest  toolchains.rs:123–146
  len‖ source_tree_hash
  len‖ dep_lock_hash
)
```

Per-producer IDs (`rustdoc/3`, `oxc/1`, …) are **not** in the JobKey today — only global `PRODUCER_VERSION`. Oracle binary contents are **not** hashed.

### 7.2 Problems for desktop multi-machine CAS sharing

1. **Path-string digest:** `/nix/store/aaa-…` vs `/Users/x/.rustup/…` → different JobKeys for identical compiler versions (`01` §9.2).  
2. **Oracle drift:** go-oracle rebuild with same path layout but different codegen → **silent IR change** with CAS hit if paths equal.  
3. **Producer feature matrix:** desktop without Java vs server with Java still same `PRODUCER_VERSION` — OK if Java jobs never run cross-channel; bump global version carefully.  
4. **Sysroot Discover:** two machines, same rustc version, different paths → miss.

### 7.3 Recommended fingerprint model

```rust
pub struct ToolchainFingerprint {
    // Semantic versions / release ids — stable across machines
    pub rustc_version: Option<String>,    // `rustc -vV` release + commit
    pub cargo_version: Option<String>,
    pub go_version: Option<String>,       // go env GOVERSION
    pub java_version: Option<String>,     // java -version parse
    pub dotnet_version: Option<String>,
    // Content digests of Class B oracles (blake3 file)
    pub go_oracle_b3: Option<[u8; 32]>,
    pub java_oracle_b3: Option<[u8; 32]>,
    pub csharp_oracle_b3: Option<[u8; 32]>, // merkle of publish dir
    // Optional: sysroot libstd hash (expensive; cache)
    pub rust_libstd_b3: Option<[u8; 32]>,
}

// JobKey component becomes fingerprint.digest() instead of path-only ToolchainSet::digest
```

**Migration:** bump `PRODUCER_VERSION` to `nudox-producer/3` when switching digest semantics (invalidates all CAS — acceptable once).

### 7.4 Alignment with per-producer IDs

Optionally include active producer id in surface key:

```
JobKey = H(producer_version ‖ producer_id ‖ toolchain_fp ‖ source ‖ lock)
```

Today surface is multi-lang only via ecosystem dispatch one lang per package — package coordinates already encode language, so global version + toolchain fp may suffice if oracle digests are in fp.

### 7.5 Desktop CAS locality

- Local disk CAS under Application Support uses same JobKey space as server **only if** fingerprint domain matches.  
- Cross-upload of local IR to INDEX should re-verify or tag `toolchain_fp` in blob metadata so remote never serves mismatched IR as universal truth without policy.

### 7.6 Producer version bump policy

| Change | Bump |
|---|---|
| IR shape / lowering semantics | `PRODUCER_VERSION` |
| Single-lang bugfix, IR-compatible | optional per-ProducerId (if added to key) |
| Oracle binary rebuild, same schema | oracle b3 in fingerprint (auto invalidation) |
| Resolver only | `RESOLVER_VERSION` (`occ-v1`) via `with_tag` |

---

## 8. Recommended MVP Language Set

### 8.1 Desktop v1 — local trusted compile

| Language | Local v1? | Rationale |
|---|---|---|
| **TypeScript** | **Yes** | Pure Rust OXC; no SDK; Hostile but worker-isolable; huge ecosystem |
| **Python** | **Yes** | Pure Rust pyrefly; MIT; worker-isolable |
| **Rust** | **Yes (conditional)** | Flagship; needs system rustup/cargo/sysroot; ship RA in worker or app; heavy but expected |
| **Go** | **Yes (conditional)** | Ship go-oracle (~10 MiB); require system `go`; sealed command path is clean |
| **Java** | **No (remote)** | JDK size + javadoc variability; pack later |
| **C#** | **No (remote)** | .NET SDK ~700 MiB; pack later |
| **Nix** | **No (remote)** | GPL + niche for general desktop |

### 8.2 Remote-only (k8s fleet) for desktop users

All seven languages remain available for **untrusted** packages and for languages not installed locally. GUI policy:

```
if package.trust == Untrusted → always remote
if package.trust == Trusted && language.enabled_local && toolchains_ready → local
else if language.remote_allowed → remote
else → error with install instructions
```

### 8.3 v1 feature matrix

| Feature | Desktop GUI build | Fleet image |
|---|---|---|
| lang-typescript | on | on |
| lang-python | on | on |
| lang-rust | on | on |
| lang-go | on | on |
| lang-java | off | on |
| lang-csharp | off | on |
| lang-nix | off | on (worker) |
| oracle-tsz | off | optional |
| buck-resources | off | on for image build |

### 8.4 Minimum host requirements (desktop local)

| Lang | Requirement |
|---|---|
| TS / Py | None |
| Rust | rustc + cargo + sysroot (rustup stable OK); `CARGO_HOME` with deps vendored or already downloaded |
| Go | go ≥ 1.22 on PATH or `go_root` set; modules present offline |

### 8.5 Success metrics

- Fresh install base app **< 150 MiB** compressed without JDK/dotnet  
- Index a medium TS or Py project offline after install  
- Index a Rust project when rustup present, without Buck  
- Crash in RA/worker does not kill GUI  
- JobKeys stable across restarts on same machine  

---

## 9. Prior Art

### 9.1 rust-analyzer binary distribution

- Prebuilt LS binaries for Windows/Linux/macOS on [GitHub releases](https://rust-analyzer.github.io/) / rustup component.  
- Standalone binary **~40 MiB** (nix unwrapped RA ~44 MiB on this host).  
- Editors **do not embed** RA as a library; they **spawn** a process — crash isolation and upgrade independence.  
- **Nudox lesson:** Prefer `producer-worker-rust` over linking `ra_ap_*` into GPUI for production desktop; library link is acceptable only for tightly versioned internal builds.

### 9.2 VS Code language servers / extensions

- Each language extension ships or downloads its server; base VS Code stays lean.  
- [Language Server Extension Guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide): client/server split, multi-root, restart strategies.  
- Node-based servers often **bundle** `node_modules`; native servers ship platform binaries in VSIX.  
- **Nudox lesson:** Language packs = VS Code extensions; `OracleSet` + worker bins = server path configuration; restart-on-crash like `vscode-languageclient`.

### 9.3 JetBrains

- Massive base IDE; additional languages via **plugins**; localization “language packs” are UI translation plugins ([IntelliJ plugin language packs](https://plugins.jetbrains.com/docs/intellij/providing-translations.html)).  
- Bundled plugins can be disabled but not always removed; third-party plugins downloaded.  
- Toolchains (JDK) often **discovered** or downloaded via UI (Project SDK).  
- **Nudox lesson:** Distinguish *product language packs* (our producers) from *UI locale packs*; SDK download UX is familiar to users — reuse for JDK/dotnet optional packs.

### 9.4 sccache / distributed compile toolchains

- sccache caches compile artifacts keyed by **compiler hash + command line + inputs**, not merely path.  
- Multi-machine cache hits require **content-identical toolchains**.  
- **Nudox lesson:** JobKey must migrate toward **version/content fingerprint** (§7), not path strings — same lesson as sccache distributed mode.

### 9.5 Nix-based apps

- `nix-bundle`, `nix-appimage`, macOS nix-darwin packaged apps, Determinate Nix.  
- Hermetic `/nix/store` paths; desktop apps either embed a subset store or require Nix.  
- NuDox already packages daemon via `package.nix` + snowydeer oracle imports.  
- **Nudox lesson:** Two channels — **Nix power users** get perfect hermeticity; **consumer channel** uses system discovery + optional packs without requiring Nix.

### 9.6 Other: LSP “download server if missing”

- Many extensions on first activation download pinned server tarball with checksum (e.g. rust-analyzer VS Code extension historically).  
- **Nudox:** first-run wizard: detect languages in workspace → propose pack downloads.

---

## 10. Concrete Recommendations

### 10.1 API / core (block librarification)

1. Add `OracleSet` + `ForgeContext::oracles()`; rewrite Go/Java/C# `buck_resource` call sites.  
2. Add `LocalForgeContext::new_with(toolchains, oracles, …)`; keep `new()` test-only with `from_env`.  
3. Add `TrustedForgeContext::assemble(TrustedForgeOptions)` for GUI — **zero ambient reads**.  
4. Move `NUDOX_TYPESCRIPT_ORACLE` to context flag.  
5. Extend `ToolchainSet` with `dotnet_root`, optional `rustc_sysroot`, and `ToolchainFingerprint` for JobKey.  
6. Bump to `nudox-producer/3` when fingerprint semantics change.

### 10.2 Packaging

7. Define desktop install layout (§2.3) and write `oracles.v1.json` manifests with blake3.  
8. Build pipeline: produce platform-triple oracle artifacts **without** requiring Buck at GUI runtime (Buck/Nix CI still *builds* them).  
9. Feature-gate languages; default desktop features = TS + Py + Rust + Go.  
10. Never link snix into GUI; Nix remote-only until GPL worker pack.

### 10.3 Process model

11. Ship `producer-worker` next to GUI; default `require_worker=true` for Hostile langs on desktop.  
12. Plan `producer-worker-rust` before marketing “stable” local Rust on large workspaces.  
13. Replace PATH-based `nudox-indexer` with bundle-relative helper or in-process forge behind a subprocess boundary.

### 10.4 Product policy

14. Settings: per-language `local | remote | off`.  
15. Untrusted packages always remote (orchestration concern; desktop must not “helpfully” run Hostile untrusted in-process).  
16. Offline: pure-Rust langs work; Rust/Go need preinstalled SDKs or packs.

### 10.5 Size

17. Enforce CI budget on desktop artifact size (fail > 150 MiB compressed base).  
18. tsz and full SDK packs never in base channel.

---

## 11. Open Questions / Risks

| # | Question | Severity | Notes |
|---|---|---|---|
| Q1 | Is process-isolated GPL snix acceptable for proprietary GUI? | High | Needs counsel; default remote until then |
| Q2 | Content-hash sysroots affordably? | Medium | `rustc -vV` may be enough without hashing all of libstd |
| Q3 | Windows support timeline? | Medium | Affects oracle triple matrix |
| Q4 | App Store / hardened runtime vs dotnet/java subprocesses | Medium | May force non-MAS distribution |
| Q5 | Single universal `PRODUCER_VERSION` vs per-lang keys | Medium | Multi-channel CAS sharing |
| Q6 | Should desktop rehost fleet-compatible CAS for hybrid? | Medium | Fingerprint alignment required |
| Q7 | RA in-process vs worker for v1 | High | UX latency vs stability |
| Q8 | Ship minimal GOROOT vs require system go | Low | Size vs reliability |
| Q9 | JDK 21 vs 23+ for Markdown doc comments | Low | Document in Java pack |
| Q10 | tsz readiness for optional Tier C pack | Medium | Pre-release |

---

## 12. Worked Examples

### 12.1 First launch (macOS, Rust project)

1. GUI starts → `discover_oracles(bundle)` finds `go-oracle` optional; no Java pack.  
2. `discover_toolchains` finds rustup stable → fills `rustup_home`, `cargo_home`, `rustc_sysroot`.  
3. `TrustedForgeContext::assemble` with `require_worker=true` for TS/Py, RA in-process.  
4. User indexes crate → `generate_with` → JobKey uses fingerprint of rustc 1.xx + empty oracles.  
5. Result in local CAS; UI loads symbols without server.

### 12.2 TypeScript offline, no system tools

1. Base app includes OXC.  
2. No toolchain fields needed.  
3. Worker optional; Hostile tier still net-off.  
4. Works fully offline.

### 12.3 Untrusted npm package

1. Trust model marks untrusted → GUI never calls local forge.  
2. Upload / queue to k8s compiler-daemon image with full oracles + bwrap.  
3. Desktop only displays returned IR.

### 12.4 User enables experimental Nix pack

1. Downloads `lang-nix` → GPL dialog + source link.  
2. Installs `producer-worker-nix` under Application Support.  
3. `OracleSet.nix_worker` set; `require_worker` forced.  
4. GUI remains without snix symbols in its link map.

---

## 13. Mapping to Existing Server Packaging

| Server (`package.nix` / image) | Desktop analogue |
|---|---|
| `compiler-daemon` binary | GUI + optional `nudox-indexer` helper |
| `producer-worker` | same binary, bundle-relative |
| `resources/go-oracle` etc. | `share/nudox/oracles/*` |
| `compiler-daemon.resources.json` | `oracles.v1.json` + `OracleSet::from_install_root` |
| `ToolchainSet::from_env` in assemble | `discover_toolchains` → explicit inject |
| bwrap in image | omitted (trusted) |
| snowydeer import paths | CI publishes oracle tarballs to CDN / release assets |

Reuse **build** graphs; replace **runtime resolution**.

---

## 14. Implementation Phases (desktop toolchains only)

| Phase | Deliverable | Exit criteria |
|---|---|---|
| **P0** | `OracleSet` + context injection; kill library `from_env` requirement | Go/Java/C# tests pass with explicit paths |
| **P1** | Feature flags; desktop default feature set | GUI builds without snix/java/csharp/tsz |
| **P2** | Bundle layout + discover_*; settings UI | Index TS/Py/Rust/Go on macOS without Buck |
| **P3** | Workers default-on for Hostile; crash tests | Kill -9 worker → GUI survives, job errors |
| **P4** | ToolchainFingerprint in JobKey (`producer/3`) | Two path layouts same rustc → same JobKey |
| **P5** | Language pack downloader | Java pack optional install works |
| **P6** | Optional GPL nix worker pack | Legal review signed off |

---

## 15. File Reference Map (absolute)

| Concern | Path |
|---|---|
| ToolchainSet | `/Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/toolchains.rs` |
| WorkerPool | `…/sandbox/worker.rs` |
| Profiles / limits | `…/sandbox/profiles.rs` |
| buck_resource | `…/compile/producer/resource.rs` |
| LocalForgeContext | `…/compile/producer/runtime.rs` |
| PRODUCER_VERSION | `…/compile/producer/mod.rs` |
| JobKey | `…/../heart/content.rs` |
| generate JobKey use | `…/generate/mod.rs` |
| RA load / sysroot | `…/compile/rust/ra/load.rs` |
| Go oracle resolve | `…/compile/go/package.rs` |
| Java doclet | `…/compile/java/oracle.rs` |
| C# oracle | `…/compile/csharp/oracle.rs` |
| tsz env | `…/compile/typescript/oracle/tsz.rs` |
| Daemon assemble | `…/daemon/forge.rs` |
| Worker bin | `…/bin/producer_worker.rs` |
| Nix package oracles | `…/package.nix` |
| Compiler BUCK resources | `…/BUCK` |
| Flake systems / devshell SDKs | `/Users/philocalyst/Projects/Backend/flake.nix` |
| GUI indexer | `…/workspace/gui/src/local_index_panel.rs` |
| Audit feed | `…/.research/librarification/01-compiler-audit/PLAN.md` |

---

## 16. Size Data Appendix (measured 2026-07-16, this host)

| Artifact | Size |
|---|---|
| rust-analyzer unwrapped (nix) | ~44 MiB |
| go-1.26.4 store | ~240 MiB |
| Zulu JDK 21 | ~342 MiB |
| .NET SDK 10.0.301 | ~707 MiB |
| rustup toolchain stable | ~1.6 GiB |
| rustup toolchain 1.95 | ~2.3 GiB |
| ra_ap_* vendor sources sum | ~17 MiB |
| rnix vendor | ~4.4 MiB |
| Go oracle source | ~1.0k LOC |
| Java Extractor source | ~1.0k LOC |

Linked binary sizes for OXC/pyrefly/snix **not** measured (no desktop build); treat as medium (10–50 MiB class each) until CI reports.

---

## 17. Risk Register (desktop-specific)

| Risk | Mitigation |
|---|---|
| Multi-GB default install | Feature gates + remote for Java/C#/Nix |
| GPL lawsuit / compliance | snix out of GUI link; remote or GPL worker pack |
| CAS pollution across machines | Fingerprint JobKey |
| Buck-only oracles | OracleSet + CI-published artifacts |
| RA OOM kills GUI | Worker process; memory limits |
| Sysroot missing | Clear UX; don't claim local Rust works without rustc |
| Ambient env in library | TrustedForgeOptions only |
| Notarization fails on unsigned oracle | Sign all ship binaries in release pipeline |

---

## Executive Summary

Desktop local compilation is blocked not by pipeline architecture — `generate_with` + `ForgeContext` are the right shape — but by **resourcing**: oracles only resolve through Buck `resources.json`, toolchains only enter through ambient `ToolchainSet::from_env`, and the full producer link set (RA + OXC + pyrefly + tsz + snix + host SDKs) is incompatible with a lean multi-platform GUI. Server packaging (`package.nix`) already materializes oracles beside `compiler-daemon`; desktop must adopt the same **layout** while replacing **resolution** with an explicit `OracleSet` + discovered `ToolchainSet` injected once at `TrustedForgeContext::assemble`.

**Producer inventory:** TypeScript (OXC) and Python (pyrefly) are pure-Rust, medium size, MIT-friendly, and ideal for offline local packs. Rust (ra_ap 0.0.341) is in-process, needs cargo/sysroot/proc-macro-srv, ~40 MiB+ code and multi-GiB RSS — flagship but must be feature-gated and eventually worker-isolated. Go/Java/C# are small Class-B oracles (binary/jar/publish dir) depending on large Class-C SDKs (Go ~240 MiB, JDK ~340 MiB, .NET SDK ~700 MiB). Nix/snix is **GPL-3.0** and must not link into a proprietary GUI.

**Packaging strategy:** Hybrid — ship Class-B oracles in-app or as language packs; **system-discover** Class-C for MVP; optional Nix channel for hermetic power users; download-on-demand for Java/C#/SDK packs later. Build-time Cargo/Buck features keep base ≤ ~150 MiB compressed. Platform matrix follows flake (darwin/linux aarch64/x86_64); Windows later.

**MVP local languages:** TypeScript, Python, Rust (if rustup present), Go (if go present). **Remote-only:** Java, C#, Nix (GPL). Untrusted packages always fleet.

**Versioning:** Migrate JobKey toolchain component from path strings to semantic/`blake3` fingerprints (oracles + compiler versions) under `nudox-producer/3` so desktop and fleet CAS can align.

**Workers:** Keep oracle subprocesses; default-on worker pools for Hostile pure-Rust producers; plan rust worker; optional GPL-only nix worker binary.

This plan closes the gap left by `01-compiler-audit` for “how desktop ships toolchains without Buck.”

---

*Research date 2026-07-16. Verified against live `workspace/compiler`, `workspace/gui`, `flake.nix`, host store sizes. No project build performed. snix GPL characterization from upstream README.*
