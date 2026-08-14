# Trust Boundaries & Project Model — Multi-Language Exhaustive Design

**Research date:** 2026-07-16  
**Scope:** How nudox computes **Trusted** vs **Untrusted** source sets for every supported ecosystem; project/workspace/path/git/registry/override semantics; lockfile→INDEX coordinate mapping; security jails; GUI Project entity; Rust type sketches.  
**Audience:** client-library + embedded-compiler + GUI architecture.  
**Does not replace:** [14-client-sync](../14-client-sync/PLAN.md) (sync typestates), [03-gui-audit](../03-gui-audit/PLAN.md) / [15-gui-references](../15-gui-references/PLAN.md) (product chrome), [01-compiler-audit](../01-compiler-audit/PLAN.md) (producer pipeline).  
**Live code anchors:** `workspace/heart/{ecosystem,package,identity,version}.rs`, `workspace/compiler/generate/{mod,parse_cache}.rs`, `workspace/compiler/compile/{go,python,typescript,rust,java,csharp,nix}/`, `workspace/registry/resolve.rs`, `workspace/compiler/sandbox/landlock.rs`, `workspace/gui/src/{workspace,types,backend}.rs`.

---

## 0. Authoritative product semantics

From 14-client-sync §0 (authoritative, restated with precision):

1. A developer **assigns a PROJECT** (one or more filesystem roots).  
2. Everything classified **Trusted** is compiled **locally** by the embedded compiler library (`PackageInput { coordinates, toolchain, root }` → `generate_with`).  
3. Everything classified **Untrusted** is **never** compiled locally: it is an INDEX **coordinate** (`heart::package::Coordinates` = origin × name × version) resolved into the local REGISTRY via progressive sync.  
4. Identity for untrusted packages is always registry-shaped; identity for trusted packages is **path + local manifest identity** (may *also* carry a published-looking name for display, but trust class is path-derived).

### 0.1 Trust is a compile-path property, not a moral label

| Class | Compile path | Query path | CAS author |
|---|---|---|---|
| **Trusted** | Embedded forge / in-process producers | Local IR + local Tantivy (project index) | Desktop client |
| **Untrusted** | **Forbidden** (type-erased; no `CompileLocally`) | INDEX → REGISTRY after sync; remote-first until Ready | Remote INDEX fleet |

Plan 14's `SourcePath<Trusted>` / `SourcePath<Untrusted>` typestate is the hard boundary; this document defines **which filesystem paths and which coordinates** fill each side.

### 0.2 Vocabulary (nudox-canonical)

| Term | Meaning |
|---|---|
| **User root** | Path the user explicitly opened (or multi-root entry). Always trusted *as a container*. |
| **Project** | Typed resolved graph for one ecosystem instance under one or more user roots. |
| **Member** | A package/crate/module unit that is part of the project (workspace member, go.work use, npm workspace package, …). |
| **SourceRoot** | Canonical absolute path of a Member's source tree (manifest parent or declared package root). |
| **Path dep** | Dependency whose content is resolved from a local filesystem path (not a registry download). |
| **Git dep** | Dependency whose content is resolved from a git URL (rev/branch/tag). |
| **Registry dep** | Dependency from a package registry (crates.io, npm, PyPI, Maven Central, nuget.org, proxy.golang.org, FlakeHub, …). |
| **Override / patch** | Manifest mechanism that replaces one resolved coordinate with another source (path/git/registry). |
| **TrustedSourceSet** | Set of `SourceRoot` paths eligible for local compile. |
| **UntrustedCoordinateSet** | Set of `Coordinates` (pinned) to fetch from INDEX. |
| **DepSet** | Generation-pinned bag of untrusted coordinates + optional generation hashes (14-client-sync). |
| **Jail** | Allowed path set for trusted FS reads during resolve/compile (Landlock / path canon rules). |

### 0.3 Default trust policy (v1 product rule)

```
TRUSTED  := transitive closure of:
              user roots
            ∪ workspace / monorepo members declared by the root manifest(s)
            ∪ path / file: / link: / replace→path / workspace: protocol deps
              whose resolved path is INSIDE the jail

UNTRUSTED := every dependency that resolves to:
              registry | git remote | path OUTSIDE jail | vendored-from-registry
            after applying patches/overrides

EXCEPTION (explicit user action):
  "Promote path outside jail to trusted" → expands jail, re-resolve
  "Treat git dep as local checkout" → only if checkout is inside jail
```

**Git dependencies are UNTRUSTED by default**, even if the user has a local clone: the *source of truth* is a remote URL + rev. If the user opens that clone as a **user root** (or path-depends it), it becomes trusted via the path rule.

**Vendored trees** (`vendor/`, `third_party/` copies of registry packages): **UNTRUSTED** unless the path is a declared workspace member or an explicit path dependency *and* the user opted into "trust vendored sources" (default off — see §5). Rationale: vendored content is still third-party code; INDEX already has the coordinate; local compile of untrusted vendored code defeats the dual-path model.

---

## 1. Ecosystem project models (exhaustive)

For each language: project unit, workspace membership, dependency kinds, patch/override, lockfile, INDEX coordinate mapping, and notes from live nudox code.

### 1.1 Rust / Cargo

**Authoritative docs:** [Cargo Book — Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html), [Specifying Dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html), [Overriding Dependencies](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html), [cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html), [Package ID Spec](https://doc.rust-lang.org/cargo/reference/pkgid-spec.html).

#### 1.1.1 Project

- A **package** is a directory with `Cargo.toml` containing `[package]` (name, version, edition, …).  
- A **workspace** is a `Cargo.toml` with `[workspace]` (virtual: no `[package]`; real: both).  
- **Workspace members:** `workspace.members` globs + implicit root package if present; `workspace.default-members` subset for default builds.  
- **Excluded:** `workspace.exclude`.  
- Nested workspaces are **not** first-class; path deps to outer trees are common instead.  
- **Virtual manifest:** root has only `[workspace]` — root is not a package; members are.

#### 1.1.2 Dependency kinds

| Kind | Manifest form | cargo metadata `source` | nudox trust default |
|---|---|---|---|
| Registry | `foo = "1.2"` or `{ version = "1.2", registry = "…" }` | `registry+URL` | **Untrusted** → `Coordinates { origin: CratesIo\|Custom, name, version }` |
| Path | `{ path = "../foo" }` | `null` (path package) | **Trusted** iff path ∈ jail |
| Git | `{ git = "https://…", rev/branch/tag }` | `git+URL#rev` | **Untrusted** (git coordinate; INDEX may later support git pins) |
| Workspace dep | `{ workspace = true }` | inherits | follow resolved kind |

Optional fields: `package` (rename), `features`, `default-features`, `optional`, `public`.

#### 1.1.3 Overrides / patches

| Mechanism | Scope | Effect on trust |
|---|---|---|
| `[patch.crates-io]` / `[patch."https://…"]` | workspace root | Replaces registry source with path or git. **Path patch → Trusted if in jail; git patch → Untrusted.** |
| `[replace]` (legacy) | same | Prefer `[patch]`; treat like patch for trust. |
| `[dependencies] foo = { path, version }` | dual-spec | Cargo uses path/git locally; version used when published. For resolve: path wins → trust by path rule. |
| `.cargo/config.toml` `paths` / source replacement | config | **Dangerous:** can redirect crates.io. Treat config-level source replacement as **Untrusted override of origin** unless path is in jail. Surface in UI as "custom source". |

#### 1.1.4 Lockfile

- **`Cargo.lock`** (v3/v4): exact package id, source, checksum, deps.  
- Required for binary packages for reproducible builds; libraries often omit — **nudox must generate or refuse non-deterministic resolve**.  
- `hash_dep_lock` already hashes `Cargo.lock` when present (`workspace/compiler/generate/parse_cache.rs:50–76`).

#### 1.1.5 INDEX coordinate mapping

```
registry+https://github.com/rust-lang/crates.io-index
  → RegistryOrigin::CratesIo
  → PackageName::new(Language::Rust, name)   // _/- folded
  → PackageVersion::Cargo(semver)

registry+https://custom/index
  → RegistryOrigin::Custom { name, url }

git+https://github.com/org/repo?rev=SHA#SHA
  → NOT in heart::RegistryOrigin today
  → v1: UntrustedCoordinateKind::Git { url, rev, package_name, version_hint }
  → INDEX may refuse until git plane exists; client shows "git dep — not in INDEX"

path packages
  → TrustedSource { root, local_name, local_version from Cargo.toml }
  → may share a crates.io-looking name → identity is (TrustClass::Trusted, path), NOT PackageId of crates.io
```

Canonical name rules: `heart/package/mod.rs` `canonicalize_crate` folds `_`→`-` and lowercases (`serde_json` ≡ `serde-json`).

#### 1.1.6 Live code

- Producer: `workspace/compiler/compile/rust/` — rust-analyzer HIR lower; `generate_ir(root, name, version, document_private)` with **direct-repo** mode pulling local library deps when `document_private` (`rust/mod.rs:33–46`).  
- This is **package-root oriented** today, not full workspace multi-member ProjectGraph — librarification must add workspace expansion *before* compile.  
- Job key includes `hash_dep_lock` (`generate/mod.rs:100–111`).

#### 1.1.7 Edge cases (Rust)

- Path under multi-root sibling → trusted (jail union). Path to `~/.cargo/registry/src` → **untrusted** (registry cache). Path outside jail → diagnostic, no compile.  
- Path package named like crates.io crate → **disjoint identities** (LocalCoordinates vs `PackageId`).  
- Virtual workspace root is not a package; members form TrustedSourceSet. Cycles among path members allowed (display-only cycle detect).  
- Build/dev/proc-macro deps enter UntrustedCoordinateSet; target-specific deps unioned for resolve.

---

### 1.2 TypeScript / JavaScript (npm · pnpm · yarn)

**Docs:** [npm workspaces](https://docs.npmjs.com/cli/v9/using-npm/workspaces), [package.json](https://docs.npmjs.com/cli/v11/configuring-npm/package.json), [pnpm workspaces](https://pnpm.io/workspaces), [Yarn workspaces](https://yarnpkg.com/features/workspaces), [npm overrides](https://docs.npmjs.com/cli/v9/configuring-npm/package-json#overrides).

#### 1.2.1 Project

- **Package:** directory with `package.json` (`name`, `version`, optional `private`).  
- **npm/yarn/pnpm workspace:** root `package.json` `workspaces: ["packages/*"]` (npm/yarn) or `pnpm-workspace.yaml` `packages:`.  
- **Package manager detection (priority):**  
  1. `pnpm-lock.yaml` → pnpm  
  2. `yarn.lock` (+ optional `.yarnrc.yml`) → yarn  
  3. `package-lock.json` / `npm-shrinkwrap.json` → npm  
  4. else: parse manifests only; **warn non-deterministic**  

#### 1.2.2 Dependency kinds

| Kind | Spec examples | Trust |
|---|---|---|
| Registry | `"lodash": "^4.17.21"` | Untrusted → `RegistryOrigin::NpmPublic` |
| Workspace protocol | `"@acme/lib": "workspace:*"` (pnpm/yarn) | Trusted (member) |
| file: | `"file:../local-pkg"` | Trusted iff path ∈ jail |
| link: | `"link:../local-pkg"` | Trusted iff path ∈ jail (symlink) |
| portal: (yarn) | portal protocol | Trusted iff path ∈ jail |
| Git | `"github:user/repo#semver:^1.0.0"`, `git+https://…` | Untrusted |
| URL / tarball | `https://…/pkg.tgz` | Untrusted (opaque; INDEX may not cover) |
| Alias | `"npm:lodash@4"` | Untrusted under real package name |
| peer / optional / dev | same sources | Untrusted or Trusted by source; include peers that appear in lock |

#### 1.2.3 Overrides / resolutions

| Tool | Mechanism | Trust |
|---|---|---|
| npm | `overrides` | Forces nested dep to version/path; path → path rule; version → untrusted |
| pnpm | `pnpm.overrides` + `packageExtensions` | same |
| Yarn | `resolutions` | same |
| pnpm `patchedDependencies` | local patch files | **Still untrusted coordinate**; patch applied only on INDEX/server if we ever mirror — desktop does not recompile patched npm from path unless user promotes |

#### 1.2.4 Lockfiles

| File | Maps to |
|---|---|
| `package-lock.json` v2/v3 | `packages` / `dependencies` tree with `resolved`, `integrity` |
| `pnpm-lock.yaml` | importers + packages with `resolution` |
| `yarn.lock` (v1 / berry) | descriptor → resolved URL + checksum |

All three are hashed by `hash_dep_lock` (`parse_cache.rs:51–59`).

#### 1.2.5 INDEX mapping

```
npm name (scoped @scope/name) + version
  → Language::Typescript
  → PackageName::new (lowercase, scope preserved)
  → PackageVersion::Npm(semver)
  → RegistryOrigin::NpmPublic | Custom (Verdaccio etc.)
```

Live producer uses `package.json` for entry/`types`/`exports` (`compile/typescript/oxc/entry.rs`) — **single package root**, not workspace graph. Project model must expand workspaces before calling `PackageInput`.

#### 1.2.6 Edge cases (JS/TS)

Never add `node_modules/**` as SourceRoots. `file:` outside jail → promote. Workspace package also on npm → local trusted; other lock pins of same name remain untrusted. No workspaces field → do not auto-scan nested package.json (opt-in). Dual locks → pick PM by detection order + warn.

---

### 1.3 Python (pip · poetry · uv · pdm · hatch)

**Docs:** [PEP 621](https://peps.python.org/pep-0621/), [PEP 508](https://peps.python.org/pep-0508/), [Poetry deps](https://python-poetry.org/docs/dependency-specification/), [uv workspaces](https://docs.astral.sh/uv/concepts/projects/workspaces/), [pip requirements](https://pip.pypa.io/en/stable/reference/requirements-file-format/).

#### 1.3.1 Project

| Marker | Meaning |
|---|---|
| `pyproject.toml` `[project]` | PEP 621 project (name, version, dependencies) |
| `pyproject.toml` `[tool.poetry]` | Poetry project |
| `setup.py` / `setup.cfg` | Legacy setuptools |
| `uv` workspace | `[tool.uv.workspace] members = …` |
| Poetry | rarely true monorepo; path deps common |
| PDM | `[tool.pdm.dev-dependencies]` + workspace plugins |

**Project root discovery:** nearest `pyproject.toml` / `setup.cfg` upward from user selection; for uv workspaces, root is workspace root.

#### 1.3.2 Dependency kinds

| Kind | Form | Trust |
|---|---|---|
| PyPI | `requests>=2.28` | Untrusted → `RegistryOrigin::PyPi` |
| Path | `{ path = "packages/foo", develop = true }` (poetry/pdm/uv) | Trusted iff ∈ jail |
| Editable | `-e ./src/mypkg` in requirements | Trusted iff ∈ jail |
| VCS | `git+https://…@tag#egg=name` | Untrusted |
| URL / wheel | direct URL | Untrusted |
| Extra markers | `; python_version>="3.10"` | Filter by selected Toolchain::Python |

#### 1.3.3 Lockfiles

| File | Tool |
|---|---|
| `poetry.lock` | Poetry |
| `uv.lock` | uv |
| `pdm.lock` | PDM |
| `Pipfile.lock` | pipenv |
| `requirements.txt` | **not** a lock — pin hashes if present (`--hash`) |

`hash_dep_lock` covers `poetry.lock` + `Pipfile.lock` today; **gap:** add `uv.lock`, `pdm.lock`.

#### 1.3.4 INDEX mapping

```
PEP 503 normalized name + PEP 440 version
  → Language::Python
  → PackageName canonicalize_pep503 (heart/package/mod.rs:122–132)
  → PackageVersion::Python
  → RegistryOrigin::PyPi | Custom
```

Live code: `compile/python/package.rs` treats root as **sys.path entry** and discovers modules — no lock resolve. Project model must supply which path is the project package vs site-packages (site-packages = never trusted).

#### 1.3.5 Edge cases (Python)

Editable/path outside jail → promote. Namespace packages may yield multiple SourceRoots. Store `manifest_root` + `import_root` for `src/` layouts. Conda: v1 out of scope (optional pip-section only).

---

### 1.4 Go modules

**Docs:** [Go Modules Reference](https://go.dev/ref/mod), [Workspaces](https://go.dev/ref/mod#workspaces), [replace](https://go.dev/ref/mod#go-mod-file-replace).

#### 1.4.1 Project

- **Module:** directory with `go.mod` (`module` path + `go` version).  
- **Workspace:** `go.work` with `use ( ./a ./b )` and optional `replace`.  
- Main modules = modules in `go.work` or single module if no work file.  
- Live discovery: `compile/go/package.rs` walks upward for `go.mod`.

#### 1.4.2 Dependency kinds

| Kind | Form | Trust |
|---|---|---|
| Module proxy / sum DB | `require example.com/foo v1.2.3` | Untrusted |
| replace → local path | `replace example.com/foo => ../foo` | Trusted iff path ∈ jail |
| replace → other module version | `replace a v1 => a v1.0.1` | Untrusted (version pin) |
| replace → other module path | `replace a => b v1.0.0` | Untrusted |
| go.work use | local modules | Trusted |
| go.work replace | same as go.mod replace | path rule |
| exclude / retract | filter candidates | resolution only |

#### 1.4.3 Lock / sums

- **`go.sum`** — cryptographic sums for module versions (not a full lock of MVS graph alone; `go.mod` + `go.sum` together).  
- MVS (minimal version selection) is deterministic given `go.mod` require/exclude graph.  
- Hashed by `hash_dep_lock`.

#### 1.4.4 INDEX mapping

```
module path + version (semver or pseudo-version)
  → Language::Go
  → PackageName::new (module path, case-sensitive-ish keep)
  → PackageVersion::Go(String)
  → Origin: Custom proxy or future GoProxy origin (not in RegistryOrigin today — gap)
```

`RegistryOrigin` currently lacks `GoProxy`; use `Custom { name: "proxy.golang.org", url }` until first-class origin lands.

#### 1.4.5 Edge cases (Go)

Only `go.work use` modules + path replaces in jail are trusted. Nested go.mod without use/replace stay out. `vendor/` third-party → untrusted (modules.txt coordinates). Pseudo-versions pin exact strings.

---

### 1.5 Java (Maven · Gradle)

**Docs:** [Maven POM](https://maven.apache.org/pom.html), [Dependency Mechanism](https://maven.apache.org/guides/introduction/introduction-to-dependency-mechanism.html), [Gradle dependency types](https://docs.gradle.org/current/userguide/dependency_types.html), [Composite builds](https://docs.gradle.org/current/userguide/composite_builds.html).

#### 1.5.1 Project

| System | Project unit | Multi-module |
|---|---|---|
| Maven | `pom.xml` with `groupId:artifactId:version` | `<modules>` aggregator POM |
| Gradle | `build.gradle(.kts)` + `settings.gradle(.kts)` | `include(":")` projects |

#### 1.5.2 Dependency kinds

| Kind | Form | Trust |
|---|---|---|
| Maven Central / remote repo | `implementation("g:a:v")` | Untrusted |
| Project dependency | `implementation(project(":lib"))` / `<dependency>` with sibling module | Trusted |
| flatDir / files | local jars | **Untrusted** (binary; no source compile) unless sources jar + promote |
| Git (Gradle source deps / JitPack) | JitPack coordinates | Untrusted |
| Maven `system` scope path | local jar path | Untrusted binary |

#### 1.5.3 Overrides

| Tool | Mechanism |
|---|---|
| Maven | dependencyManagement, BOMs, exclusions |
| Gradle | resolutionStrategy.force, platform/BOM, component metadata rules, `strictly` |

All version forces → still Untrusted coordinates with forced version.

#### 1.5.4 Lockfiles

| File | Tool |
|---|---|
| `gradle.lockfile` / per-config locks | Gradle dependency locking |
| Maven — no standard lock | `maven-dependency-plugin` / optional `.mvn` — **non-deterministic without enforcer** |
| `pnpm`-style N/A | recommend Gradle lock or Maven CI enforcer for nudox |

**Gap:** `hash_dep_lock` does **not** hash `gradle.lockfile` or Maven effective POM — must extend.

#### 1.5.5 INDEX mapping

```
groupId + artifactId + version (+ classifier/packaging optional)
  → Language::Java
  → name: "groupId:artifactId" or separate fields in future Coordinates extension
  → PackageVersion::Java(String)
  → RegistryOrigin::Custom { "maven-central", url } until first-class Maven origin
```

`PackageName` Java canonicalize today is artifact-like (`heart/package/mod.rs:146–151`) — **groupId:artifactId as single string** is the practical v1 encoding (already allows `/` and `.`).

#### 1.5.6 Edge cases

`includeBuild` / reactor modules → trusted iff path ∈ jail. `mavenLocal()` → untrusted origin `maven-local`. Annotation processors → untrusted compile-classpath deps.

---

### 1.6 C# / NuGet / .NET

**Docs:** [NuGet PackageReference](https://learn.microsoft.com/en-us/nuget/consume-packages/package-references-in-project-files), [ProjectReference](https://learn.microsoft.com/en-us/dotnet/core/project-sdk/msbuild-props), [Central Package Management](https://learn.microsoft.com/en-us/nuget/consume-packages/central-package-management), [NuGet lock files](https://learn.microsoft.com/en-us/nuget/consume-packages/package-references-in-project-files#locking-dependencies).

#### 1.6.1 Project

- **Project:** `*.csproj` / `*.fsproj` / `*.vbproj`.  
- **Solution:** `*.sln` / `*.slnx` listing projects.  
- **SDK-style** projects with `PackageReference` and `ProjectReference`.

#### 1.6.2 Dependency kinds

| Kind | Form | Trust |
|---|---|---|
| PackageReference | NuGet ID + version | Untrusted → `RegistryOrigin::NuGet` |
| ProjectReference | path to csproj | Trusted iff path ∈ jail |
| FrameworkReference | shared framework | Untrusted platform (usually skip INDEX or special-case runtime packs) |
| DLL Reference HintPath | local assembly | Untrusted binary |

#### 1.6.3 Overrides

- Central Package Management (`Directory.Packages.props`) — version pins.  
- `PackageVersion` overrides, floating versions (`*`) — lockfile required.  
- `NuGet.config` package sources / clear — custom origins.

#### 1.6.4 Lockfiles

- `packages.lock.json` (NuGet lock file).  
- **Gap:** not in `hash_dep_lock` list — add.

#### 1.6.5 INDEX mapping

```
NuGet ID (case-insensitive) + version
  → Language::CSharp
  → PackageName canonicalize_csharp (lowercase)
  → PackageVersion::CSharp
  → RegistryOrigin::NuGet | Custom
```

Live: `compile/csharp/` nupkg + Roslyn oracle — package materialization assumes extracted package root, not solution graph.

---

### 1.7 Nix (flakes · classic)

**Docs:** [Nix flakes](https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-flake.html), [flake.lock](https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-flake.html#flake-lock-files), [FlakeHub](https://flakehub.com/).

#### 1.7.1 Project

- **Flake project:** directory with `flake.nix` (+ usually `flake.lock`).  
- **Classic:** `default.nix` / `shell.nix` without flake inputs — weaker identity.  
- Live producer: `compile/nix/` — snix eval + static lower; FlakeHub origin in heart.

#### 1.7.2 "Dependencies" = flake inputs

| Input type | flake.nix | Trust |
|---|---|---|
| github: / sourcehut: / git: | remote flake | Untrusted → FlakeHub or git coordinate |
| path: / `./sibling` | local path | Trusted iff ∈ jail |
| indirect (registry) | `nixpkgs` | Untrusted (resolved via registry / lock) |
| tarball URL | url | Untrusted |

#### 1.7.3 Lockfile

- **`flake.lock`** — pins every input node (narHash, rev, lastModified).  
- Hashed by `hash_dep_lock`.  
- Deterministic resolve **requires** lock; unlocked flakes are non-reproducible → refuse or ephemeral warn.

#### 1.7.4 INDEX mapping

```
FlakeHub org/project + version (X.Y.Z+rev-sha)
  → Language::Nix
  → PackageName canonicalize_nix_flake
  → PackageVersion::Nix
  → RegistryOrigin::FlakeHub
```

Path flakes: TrustedSource only.

#### 1.7.5 Edge cases

| Case | Outcome |
|---|---|
| `inputs.nixpkgs.url = "github:NixOS/nixpkgs"` | Untrusted (huge); INDEX subset / attrpath selection is a product problem (22-crate-topology / nix plans) |
| follows / input overrides | Apply lock graph; trust only path nodes |
| `nix develop` impure paths | Ignore impure for trust set |

---

## 2. Precise trust-boundary algorithm

### 2.1 Inputs

```text
UserSelection:
  roots: Vec<PathBuf>           # multi-root allowed
  explicit_members: Vec<Path>   # optional linkedProjects-style overrides
  trust_promotions: Vec<Path>   # user-expanded jail
  policy: TrustPolicy

TrustPolicy:
  trust_path_deps_inside_jail: true   # default
  trust_git_checkouts: false          # default
  trust_vendor_dir: false             # default
  include_dev_dependencies: true     # INDEX set
  include_build_dependencies: true
  allow_config_source_replacement: false  # Cargo config / NuGet.config
```

### 2.2 Phase A — Discover ecosystem graphs

```
for root in roots:
  root = canonicalize(root)  # reject non-existent
  markers = detect_manifests(root)  # may be multi-language monorepo
  for ecosystem in markers:
    graph[ecosystem].push(load_workspace(root, ecosystem))
```

**Multi-language monorepo:** one UserSelection may yield **multiple** `Project` instances (e.g. Cargo workspace + pnpm workspace). GUI binds them as sibling Projects under one App Workspace.

**Detection order per directory** (first wins as primary; others still scanned if markers present):

1. `Cargo.toml`  
2. `go.work` / `go.mod`  
3. `pnpm-workspace.yaml` / `package.json`  
4. `pyproject.toml` / `setup.cfg`  
5. `*.sln` / `*.csproj`  
6. `settings.gradle*` / `pom.xml`  
7. `flake.nix`  

### 2.3 Phase B — Build jail

```
jail := empty path set (store as realpath prefixes)

for root in roots ∪ trust_promotions ∪ explicit_members' parents:
  jail.add(canonicalize(root))

# Optional: if policy.auto_include_workspace_member_paths:
for member in discovered workspace members:
  jail.add(member.manifest_parent)
```

**Jail rule:** path `P` is **inside jail** iff ∃ `J ∈ jail` such that `realpath(P) == J` OR `realpath(P)` is a strict descendant of `J` (after symlink resolution).

**Symlinks:** always resolve with `canonicalize` before test. A symlink *inside* jail pointing *outside* → target is outside → **not trusted**.

### 2.4 Phase C — Expand trusted members

```
TrustedSourceSet := {}
queue := workspace members of each graph rooted in roots
         ∪ explicit_members
         ∪ path-like deps (see per-ecosystem) once paths known

while queue non-empty:
  m = queue.pop()
  p = canonicalize(m.path)
  if p not in jail:
    record EdgeCase::PathOutsideJail(m)
    continue  # NOT trusted
  if p already in TrustedSourceSet: continue
  TrustedSourceSet.insert(SourceRoot {
    path: p,
    ecosystem,
    local_identity: parse_manifest_name_version(p),
    role: Member | PathDep | Promoted,
  })
  for path_dep in path_dependencies_of(m):
    queue.push(path_dep)
```

**Stop conditions:** cycles via visited set; max depth safety (e.g. 256).

### 2.5 Phase D — Resolve untrusted coordinates

Prefer **lockfile-first** resolution:

```
if lockfile present and valid:
  UntrustedCoordinateSet := parse_lock(lockfile)
    .filter(pkg => pkg not represented by any TrustedSourceSet local identity
                   OR pkg.source is registry/git)
    .map(to_coordinates)
else:
  # soft-resolve from manifests (version ranges) — mark ResolveQuality::Floating
  UntrustedCoordinateSet := soft_resolve(manifests)  # may need INDEX version list
```

Apply patches/overrides **before** final coordinate emission:

```
for patch in patches:
  if patch.target matches coord:
    if patch.replacement is Path:
      if path in jail: move to TrustedSourceSet; remove from Untrusted
      else: flag PathOutsideJail
    if patch.replacement is Git: coord := GitCoordinate
    if patch.replacement is Version: coord.version := forced
```

### 2.6 Phase E — Classification matrix (normative)

| Resolved source | Inside jail? | TrustClass |
|---|---|---|
| Workspace / solution member | yes (by construction) | Trusted |
| Workspace member | path missing / unreadable | Error |
| Path / file: / ProjectReference / replace→path | yes | Trusted |
| Path / … | no | UntrustedCandidate + PromotePrompt (**do not compile**) |
| Registry | n/a | Untrusted |
| Git remote | n/a | Untrusted |
| Git checkout path used only as cache | n/a | Untrusted |
| Vendor directory copy | policy false | Untrusted (coordinate from lock) |
| Vendor directory | policy true + path in jail | Trusted (discouraged) |
| Config-level registry redirect to path | allow false | Error / ignore config |
| Config-level redirect to path in jail | allow true | Trusted |
| Binary-only local jar/dll | n/a | UntrustedBinary (no local IR compile; optional skip) |

### 2.7 Phase F — Outputs

```rust
// Conceptual — full types in §6
struct ResolveResult {
  projects: Vec<ProjectGraph>,
  trusted: TrustedSourceSet,
  untrusted: UntrustedCoordinateSet,
  dep_set: DepSet,              // generation pins filled later by INDEX
  lock_hash: ContentHash,       // parse_cache::hash_dep_lock aggregate
  quality: ResolveQuality,      // Locked | Floating | Partial
  diagnostics: Vec<ResolveDiag>,
  jail: Jail,
}
```

### 2.8 Worked examples

#### Example A — Cargo virtual workspace

```
/app/Cargo.toml          # [workspace] members = ["crates/*"]
/app/crates/api/Cargo.toml
/app/crates/core/Cargo.toml   # path dep? no, member
/app/crates/api depends on serde = "1"
/app/crates/api depends on core = { path = "../core" }
```

- User root: `/app`  
- Trusted: `/app/crates/api`, `/app/crates/core`  
- Untrusted: `crates.io/serde/<locked>`  
- `core` not double-counted as untrusted despite also being publishable

#### Example B — path dep outside root

```
/app/Cargo.toml depends on helper = { path = "../../libs/helper" }
User root: /app only
```

- `../../libs/helper` canonical `/libs/helper` ∉ jail  
- Diagnostic: `PathOutsideJail { path: /libs/helper, suggested_action: AddRootOrPromote }`  
- Not in TrustedSourceSet; not silently compiled  
- If user multi-roots `/libs`, jail expands → Trusted

#### Example C — npm workspaces + file:

```
/repo/package.json workspaces: ["packages/*"]
/repo/packages/web depends on "@acme/ui": "workspace:*"
/repo/packages/web depends on "legacy": "file:../../outside/legacy"
```

- Trusted: each `packages/*` with package.json  
- `file:../../outside/legacy` → PathOutsideJail  
- registry deps from lock → Untrusted

#### Example D — go.work + replace

```
go.work: use ( ./cmd/a ./lib/b )
go.mod in a: require gopkg.in/yaml.v3 v3.0.1
             replace example.com/b => ../lib/b
```

- Trusted: `./cmd/a`, `./lib/b`  
- Untrusted: `gopkg.in/yaml.v3@v3.0.1` (+ transitive from go.mod/go.sum)

#### Example E — patch + name shadow

`[patch.crates-io] serde = { path = "vendor/serde" }`: if path ∈ jail (and vendor policy or under user root), serde leaves UntrustedCoordinateSet; local trusted wins for this Project's compile. Renamed path package (`anyhow-local = { path, package = "anyhow" }`) uses **LocalCoordinates**, never crates.io `PackageId`.

---

## 3. Lockfile role & INDEX mapping

### 3.1 Why locks are mandatory for DepSet

INDEX serves **exact** `Coordinates` (name + version + origin). Range dependencies (`^1.2`, `>=2`) are not INDEX keys. Therefore:

| ResolveQuality | DepSet usable for sync? | Search generation pin? |
|---|---|---|
| **Locked** | Yes | Yes |
| **Floating** | Only after soft-resolve against INDEX version lists | Pin after resolve |
| **Partial** (missing lock entries) | Sync known; warn on rest | Partial |

### 3.2 What must be committed (developer guidance)

| Ecosystem | Commit | Optional |
|---|---|---|
| Rust binary / app | `Cargo.lock` | — |
| Rust lib-only | lock recommended for nudox | generate on open |
| npm/pnpm/yarn | lock of the PM in use | — |
| Go | `go.mod` + `go.sum` | `go.work` usually **not** committed (local); if committed, honor it |
| Python | `uv.lock` / `poetry.lock` / hashed requirements | — |
| Gradle | `*.lockfile` | — |
| NuGet | `packages.lock.json` | enable RestorePackagesWithLockFile |
| Nix | `flake.lock` | — |

### 3.3 Lock entry → Coordinates algorithm (generic)

```
for entry in lock_entries:
  if entry is replaced by trusted path: skip (trusted owns surface)
  origin = map_source_url(entry.source)
  name = canonicalize(ecosystem, entry.name)
  version = parse_version(ecosystem, entry.version)
  coords = Coordinates { origin, name, version }
  integrity = entry.checksum  # store alongside for supply-chain UI
  UntrustedCoordinateSet.insert(PinnedCoord { coords, integrity, kind: Registry|Git })
```

### 3.4 Aggregate lock hash (live code alignment)

`hash_dep_lock(root)` (`parse_cache.rs:50–76`) folds present lock filenames. Project-level resolve should:

1. Hash **workspace root** locks (not each member separately for Cargo/npm).  
2. For multi-root, hash **each root's** lock set and fold into `ResolveResult.lock_hash`.  
3. Feed `JobKey::derive(..., dep_lock)` for trusted compiles so dep graph changes invalidate CAS (`generate/mod.rs` + `heart/content.rs` JobKey).

**Extend LOCKS list:**

```rust
const LOCKS: &[&str] = &[
  "Cargo.lock",
  "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
  "go.sum", // + consider go.mod as required sibling
  "poetry.lock", "Pipfile.lock", "uv.lock", "pdm.lock",
  "flake.lock",
  "packages.lock.json",      // NuGet
  "gradle.lockfile",         // may be nested — walk?
];
```

### 3.5 Soft resolve without lock

When lock missing:

1. Parse direct deps as `VersionRequest` (`registry/resolve.rs` vocabulary).  
2. For each, `resolve(source, name, request)` against INDEX published set.  
3. Transitive closure: **v1 may only pin directs** and let INDEX return recommended generation that includes transitives; or run ecosystem-native resolver offline (cargo metadata, `go list -m all`, etc.) if toolchain present.

**Recommendation:** Prefer invoking **native resolver read-only** when toolchain exists (`cargo metadata --format-version 1`, `go list -m -json all`, `pnpm ls -r --json`, …) because they encode edge-case rules; fall back to pure-Rust parsers when offline.

---

## 4. Security model

### 4.1 Threat: untrusted content on the trusted compile path

| Attack | Mechanism | Mitigation |
|---|---|---|
| Path dep to `/etc` or home ssh keys | malicious Cargo.toml in opened project | Jail: only paths under user roots; never follow path deps out without promotion |
| Symlink escape | `path = "./innocent"` → symlink to `/` | `canonicalize` before jail test; refuse non-canonical |
| `node_modules` as source root | install scripts plant code | Never add `node_modules` to TrustedSourceSet |
| Vendor path that is registry unpack | pretends to be first-party | default `trust_vendor_dir=false`; coordinate still untrusted |
| Cargo `[patch]` to outside path | silent trust expansion | PathOutsideJail diagnostic; no compile |
| `.cargo/config.toml` source replace | redirect serde to evil path | ignore unless policy.allow_config_source_replacement |
| Git submodule pointing outside | member path | canonicalize + jail |
| Zip-slip when materializing untrusted | INDEX path | only trusted path is local FS; untrusted never extracted into compile roots on desktop for local forge |
| Build script / proc-macro execution | RA may run build scripts | Trusted compile may execute code — **Workspace Trust** UX (VS Code model): user confirms project trusted for execution |
| Overlay FS race | TOCTOU path swap | re-canonicalize at compile seal time; include path realpath in job metadata |

### 4.2 Separation from sandbox (server fleet)

Server compilers seal **untrusted** package tarballs in Landlock/bwrap (`sandbox/landlock.rs`, `compile/producer/mod.rs` seal). Desktop **trusted** path may use looser cage (developer machine) but still:

- RO bind only TrustedSourceSet roots + toolchain paths  
- **No network** during produce (same as server) for determinism  
- Never mount entire `$HOME`

### 4.3 Jail implementation sketch

```rust
pub struct Jail {
  /// Canonical directory prefixes.
  prefixes: Vec<PathBuf>,
}

impl Jail {
  pub fn contains(&self, path: &Path) -> Result<bool, IoError> {
    let real = path.canonicalize()?;
    Ok(self.prefixes.iter().any(|j| real.starts_with(j)))
  }

  pub fn assert_contains(&self, path: &Path) -> Result<PathBuf, TrustError> {
    let real = path.canonicalize().map_err(...)?;
    if self.contains_real(&real) { Ok(real) } else {
      Err(TrustError::OutsideJail { path: real })
    }
  }
}
```

At `seal_package` for trusted compile, pass `Mounts.read_only = trusted_roots + toolchains`.

### 4.4 Can untrusted IR enter trusted index?

Policy:

1. Local project index keys by `SourceRoot` + source hash, **not** by crates.io PackageId.  
2. Cross-references from trusted code **to** untrusted symbols use `PackageId` of the untrusted coordinate (INDEX/REGISTRY), never a local recompile.  
3. If a path package shadows a registry name, references resolve to **trusted local** PackageId variant: e.g. `LocalPackageKey { project_id, relative_path, name, version }` distinct from `PackageId` of registry.

### 4.5 Workspace Trust (product)

Align with [VS Code Workspace Trust](https://code.visualstudio.com/docs/editing/workspaces/workspace-trust):

- Opening a folder does not auto-run build scripts / oracles until user **Trusts** the project.  
- Restricted mode: parse manifests only, show dependency list, no compile.  
- Trust is per user root path (persisted in app support dir).

---

## 5. GUI / product model

Companion UI sketches: 15-gui-references §2.2 Project manager. This section defines **data fields** and **triggers**.

### 5.1 Entities

```text
AppWorkspace (GPUI entity)          # multi-project desktop session
  projects: Vec<Entity<Project>>
  active: ProjectId

Project
  id: ProjectId
  display_name: String
  user_roots: Vec<PathBuf>
  ecosystem: Language | Multi(Vec<Language>)
  state: Unresolved | Resolved | Indexed   # typestate in client; enum in GUI
  trust: TrustSummary
  members: Vec<MemberRow>
  deps: Vec<DepRow>
  diagnostics: Vec<ResolveDiag>
  resolve_generation: u64
  lock_hash: Option<ContentHash>
  last_resolved_at: DateTime

MemberRow
  source_root: PathBuf
  name: String
  version: String
  role: WorkspaceMember | PathDep | Promoted
  compile_status: Idle | Queued | Ready | Error
  trust: Trusted

DepRow
  coordinates: Coordinates
  kind: Registry | Git | Binary
  lock_integrity: Option<String>
  sync: Missing | Fetching | Ready | Error
  generation: Option<ContentHash>
  shadowed_by_trusted: bool

TrustSummary
  trusted_count: usize
  untrusted_count: usize
  outside_jail: usize
  execution_trusted: bool   # Workspace Trust
```

### 5.2 Re-resolve triggers

| Event | Action |
|---|---|
| Open / add folder | Full resolve |
| FS notify: any manifest / lock change under roots | Debounced re-resolve (250–500ms) |
| FS notify: source file change | Recompile trusted members (not full dep resolve) |
| User edits TrustPolicy / promotions | Full resolve |
| Toolchain change | Full resolve + invalidate JobKeys |
| INDEX connectivity restored | ensure_deps only |
| Manual "Refresh project" | Full resolve |
| Lockfile deleted | quality→Floating; warn |

### 5.3 Multi-project apps

- **AppWorkspace** holds N Projects (polyglot or multi-repo).  
- Search scope: `Project | Project+Deps | AllProjects | AllSyncedRegistry`.  
- Shared REGISTRY CAS across projects (14-client-sync risk #5); DepSets remain per-project.  
- Graph view: default active project's trusted + ready deps.

### 5.4 UX for edge cases

| Diagnostic | UI |
|---|---|
| PathOutsideJail | amber row + "Add folder to workspace" / "Trust this path" |
| Missing lockfile | banner "Dependency versions not locked — resolve may drift" |
| Git dep unsupported by INDEX | grey "Local clone not indexed"; link open as project |
| Name shadow trusted vs registry | split badge LOCAL vs crates.io |
| Execution not trusted | Restricted mode banner |

### 5.5 Mapping to current lindsey GUI

Today (`workspace/gui/src/workspace.rs`): single remote library load via `POST /api/packages` — **no Project entity**. Migration:

1. Add `ProjectStore` entity (15-gui-references).  
2. Wire trust list before search polish.  
3. `BackendClient` → `Client<Live>` with project-scoped `Session` (14-client-sync).

---

## 6. Concrete Rust types / traits

Aligned with `heart` vocabulary and plan 14 typestates.

### 6.1–6.3 Trust markers, sets, graph

```rust
// Brands (14-client-sync §4.4) — CompileLocally only for Trusted.
pub enum Trusted {}
pub enum Untrusted {}

pub struct SourcePath<Trust> {
    path: PathBuf, // always canonical
    _trust: PhantomData<Trust>,
}
impl SourcePath<Trusted> {
    /// Sole constructor: requires Jail proof.
    pub fn try_new(path: PathBuf, jail: &Jail) -> Result<Self, TrustError> { /* assert_contains */ }
}

pub enum TrustClass { Trusted, Untrusted, UntrustedBinary }

pub struct SourceRoot {
    pub path: PathBuf,
    pub ecosystem: Language,
    pub name: PackageName,
    pub version: PackageVersion,
    pub role: SourceRole, // UserRootPackage | WorkspaceMember | PathDependency | PromotedExternal
}

pub struct PinnedCoordinate {
    pub coordinates: Coordinates, // heart::package::Coordinates
    pub integrity: Option<Integrity>,
    pub source_kind: UntrustedSourceKind, // Registry | Git{url,rev} | DirectUrl | OutsideJailPath
}

pub struct Jail { prefixes: Vec<PathBuf> } // realpath prefixes
pub struct TrustedSourceSet { roots: Vec<SourceRoot> }
pub struct UntrustedCoordinateSet { pins: Vec<PinnedCoordinate> }

pub struct ProjectGraph {
    pub id: ProjectId,
    pub ecosystem: Language,
    pub manifest_kind: ManifestKind, // CargoWorkspace | NpmWorkspace | GoWork | …
    pub user_roots: Vec<PathBuf>,
    pub nodes: HashMap<NodeId, DepNode>,
    pub edges: Vec<DepEdge>,
    pub lock: Option<LockfileRef>,
    pub toolchain: Option<Toolchain>,
}
pub enum DepNode {
    Trusted(SourceRoot),
    Untrusted(PinnedCoordinate),
    PendingTrust { path: PathBuf, wanted_name: PackageName },
}
pub struct DepEdge {
    pub from: NodeId, pub to: NodeId,
    pub kind: EdgeKind, // Normal | Dev | Build | Optional | Peer | Patch
    pub rename: Option<String>,
}
```

### 6.4 Resolve pipeline (typestate)

```rust
pub struct Unresolved; pub struct Resolved; pub struct Indexed;

pub struct Project<S> {
    graph: ProjectGraph,
    trusted: TrustedSourceSet,
    untrusted: UntrustedCoordinateSet,
    jail: Jail,
    lock_hash: ContentHash,
    quality: ResolveQuality, // Locked | Floating | Partial
    diagnostics: Vec<ResolveDiag>,
    _state: PhantomData<S>,
}

impl Project<Unresolved> {
    pub fn open(selection: UserSelection) -> Result<Self, ProjectError>;
    pub async fn resolve(self, native: Option<&NativeToolchainProbe>,
        local: LocalComputeCapability) -> Result<Project<Resolved>, ProjectError>;
}
impl Project<Resolved> {
    pub fn trusted_sources(&self) -> &TrustedSourceSet;
    pub fn untrusted_coordinates(&self) -> &UntrustedCoordinateSet;
    pub fn dep_set(&self) -> DepSet; // → SyncEngine (14)
    pub fn trusted_paths(&self) -> Vec<SourcePath<Trusted>>;
    pub async fn ensure_deps(self, client: &Client<Live>,
        net: Option<NetworkCapability>) -> Result<Project<Indexed>, ProjectError>;
}
```

### 6.5 Compile integration + local identity

```rust
pub trait CompileLocally {
    async fn compile_locally(&self, forge: &ForgeRuntime<Ready>,
        coords: LocalCoordinates, toolchain: &Toolchain)
        -> Result<GeneratedPackage, CompileError>;
}
impl CompileLocally for SourcePath<Trusted> {
    // PackageInput { coordinates, toolchain, root: self.path } → generate_with
    // (workspace/compiler/generate/mod.rs:45–52, 96+)
}
// NO CompileLocally for PinnedCoordinate — only sync_from_index → SyncedWitness.

/// Client-side only — do NOT put Local into RegistryOrigin / PackageId space.
pub struct LocalCoordinates {
    pub project: ProjectId,
    pub member_key: String, // relative path stable within project
    pub name: PackageName,
    pub version: PackageVersion,
}
```

**Recommendation:** Keep `PackageId` registry-pure; trusted surfaces use `LocalCoordinates` / `LocalPackageKey`. Cross-edges from trusted code to deps use real untrusted `PackageId`.

### 6.6 Resolver trait + native probe

```rust
pub trait EcosystemResolver: Send + Sync {
    fn language(&self) -> Language;
    fn detect(&self, root: &Path) -> Option<ManifestKind>;
    fn load_graph(&self, roots: &[PathBuf], jail: &Jail, policy: &TrustPolicy)
        -> Result<ProjectGraph, ResolveError>;
    fn parse_lock(&self, root: &Path) -> Result<UntrustedCoordinateSet, ResolveError>;
    fn path_deps(&self, graph: &ProjectGraph) -> Vec<PathBuf>;
}
// Impls: Cargo, NpmFamily, Go, Python, Maven, Gradle, Nuget, NixFlake.

pub struct NativeToolchainProbe; // cargo metadata, go list -m -json all, pnpm ls -r, …
// Prefer native when present; pure parsers = offline fallback.
```

---

## 7. Prior art

### 7.1 rust-analyzer project model

Sources: [project_model crate docs](https://rust-lang.github.io/rust-analyzer/project_model/index.html), [configuration linkedProjects](https://rust-analyzer.github.io/book/configuration.html), issues on lazy discovery.

| Concept | rust-analyzer | nudox mapping |
|---|---|---|
| `CargoWorkspace` | from `cargo metadata` | `ProjectGraph` + CargoResolver |
| Workspace member vs library dep | sysroot + crates | Trusted members vs Untrusted coordinates |
| `linkedProjects` | extra Cargo.toml paths | `UserSelection.explicit_members` / multi-root |
| `rust-project.json` | non-Cargo build systems | future ManifestKind::Custom |
| Sysroot crates | special | Untrusted or bundled platform set |
| Path deps | joined into workspace crates | Trusted if jail allows |
| Load cargo metadata | spawns `cargo` | NativeToolchainProbe |

**Lesson:** Prefer `cargo metadata` over re-implementing feature resolution. Use metadata `packages[].source == null` as path/member signal and `workspace_members` for membership.

### 7.2 VS Code multi-root + Workspace Trust

Sources: [Multi-root Workspaces](https://code.visualstudio.com/docs/editing/workspaces/multi-root-workspaces), [Workspace Trust](https://code.visualstudio.com/docs/editing/workspaces/workspace-trust).

| Concept | VS Code | nudox |
|---|---|---|
| `.code-workspace` folders[] | multi-root | `UserSelection.roots` |
| Folder settings | per-root | per-Project policy overrides |
| Workspace Trust | execution gate | `execution_trusted` before compile/oracles |
| Restricted Mode | limited features | manifest-only resolve |

### 7.3 cargo metadata source strings

From Cargo book / cargo-metadata:

- `null` source → path or workspace member  
- `registry+URL` → Untrusted registry  
- `git+URL#rev` → Untrusted git  

Package ID specs encode source; use them when logging.

### 7.4 go/packages & `go list`

`golang.org/x/tools/go/packages` and `go list -m -json all` give the build list after MVS + replace. Path replaces appear as `Dir` set to local path — trust via jail. Live nudox oracle is a separate Go program under `compile/go/oracle/`.

### 7.5 SCIP indexing scopes

[SCIP](https://github.com/sourcegraph/scip) indexes are typically **per-project** with package-manager-specific indexers (scip-java, rust-analyzer scip, scip-typescript). Scopes:

- Index only first-party roots (≈ TrustedSourceSet)  
- Dependencies as external symbols with package manager coordinates (≈ UntrustedCoordinateSet)

nudox should mirror this split: local occurrence index for trusted; REGISTRY IR for untrusted.

### 7.6 Other analogues

| System | Relevance |
|---|---|
| Buck/Bazel package boundaries | explicit package graph; jail ≈ package path prefixes |
| Nix store purity | path inputs must be declared — same as TrustedSourceSet explicitness |
| Deno import maps | URL deps untrusted; local files trusted |
| Cargo vendor + `[source.crates-io] replace-with` | config redirect danger |

---

## 8. Cross-cutting decision records

| ID | Decision | Rationale |
|---|---|---|
| D1 | Path-in-jail ⇒ Trusted; else not | User control of trust expansion |
| D2 | Git always Untrusted by default | Remote content; INDEX or open-as-root |
| D3 | Vendor dirs Untrusted by default | Avoid third-party on compile path |
| D4 | Lockfile-first DepSet | Deterministic INDEX pins |
| D5 | Local identity ≠ Registry PackageId | Shadowing safety |
| D6 | Multi-language monorepo ⇒ multiple ProjectGraphs | Ecosystem resolvers stay pure |
| D7 | Prefer native metadata tools when present | Correctness of feature/MVS/workspace protocol |
| D8 | Typestate Trusted/Untrusted for compile API | Untrusted compile unrepresentable (plan 14) |
| D9 | Workspace Trust for execution | Build scripts / oracles are code execution |
| D10 | Extend hash_dep_lock for all ecosystems | JobKey + re-resolve coherence |

---

## 9. Roadmap, live gaps, open questions, recommendations

**Roadmap (ordered):** (1) `client::project` — Jail, sets, Project typestate; (2) CargoResolver via `cargo metadata` + golden trust fixtures; (3) Go + NpmFamily resolvers; (4) trusted_paths → `generate_with`; (5) untrusted → DepSet → SyncEngine (14); (6) GUI ProjectStore + trust panel (15); (7) Python/NuGet/Gradle/Nix; (8) RegistryOrigin for GoProxy/Maven if INDEX gains them; (9) extend `hash_dep_lock`; (10) security tests (symlink, outside jail, patch, config replace).

**Live gaps:** no ProjectGraph (`PackageInput.root` only — `generate/mod.rs:45–52`); `RegistryOrigin` lacks Go/Maven (`heart/identity/package.rs:168–178`); lock hash misses uv/pdm/NuGet/Gradle (`parse_cache.rs:51–59`); GUI remote-only (`gui/src/workspace.rs`); Rust direct-repo flag not full workspace (`rust/mod.rs:37–39`); Go single-module (`go/package.rs`); TS single package (`typescript/oxc/entry.rs`).

**Open questions:** (1) Persist promotions in app config vs committed nudox project file? (2) Git plane on INDEX vs open-checkout-as-root? (3) Path proc-macro/build-script always untrusted? (4) Hybrid search before DepSet Ready (14 Q#10)? (5) Optional features: union vs defaults in DepSet? (6) npm peers when not hoisted? (7) LocalCoordinates ever on wire? (8) Polyglot folder: two Projects (D6) vs Multi UX group? (9) Empty lock hash: warn or refuse? (10) Symlink farms / realpath jail surprises?

**Recommendations (priority):** Jail+canonicalize first; CargoResolver+fixtures; local identity ≠ PackageId; lockfile-first DepSet; default-deny vendor/git/config-replace; `CompileLocally` only on `SourcePath<Trusted>`; extend `hash_dep_lock`; GUI project+trust before search polish; Workspace Trust before oracles; multi-root jail union (never silent outside follow).

---

## 10. Executive synthesis

nudox's dual-path architecture (trusted local compile vs untrusted INDEX) requires a **deterministic, multi-language project model** that plans 03/14/15 only sketched. This document defines per-ecosystem notions of project, workspace member, path/git/registry dependency, and override/patch for Rust/Cargo, JS (npm/pnpm/yarn), Python, Go, Java (Maven/Gradle), C#/NuGet, and Nix flakes — each mapped to heart `Coordinates` / `PackageName` / `RegistryOrigin` where they exist, and to explicit gaps (Go proxy, Maven Central origins; incomplete lock hashing).

The normative algorithm is: **(1)** detect graphs under user roots, **(2)** build a **Jail** as the realpath prefix set of user roots ∪ promotions, **(3)** BFS path-like dependencies that stay inside the jail into `TrustedSourceSet`, **(4)** lockfile-parse registry/git pins into `UntrustedCoordinateSet` after applying patches, **(5)** never local-compile outside the jail. Git and vendor trees are untrusted by default; name collisions between path members and registry packages use **disjoint identity** (local key vs `PackageId`). Security hinges on canonicalize-before-jail, refusing `node_modules` as roots, ignoring Cargo/NuGet config redirects by default, and a VS Code-like Workspace Trust gate before executing toolchains.

Lockfiles are mandatory for generation-pinned `DepSet` sync; soft resolve is a degraded mode. Prefer native resolvers (`cargo metadata`, `go list`, …) when toolchains exist. Types extend plan 14: `Project<Unresolved|Resolved|Indexed>`, `SourcePath<Trusted|Untrusted>`, `ProjectGraph`, `Jail`, `TrustedSourceSet`, `UntrustedCoordinateSet`, `PinnedCoordinate`, with `CompileLocally` only on trusted paths feeding existing `PackageInput` / `generate_with`. Prior art (rust-analyzer project_model, VS Code multi-root + trust, SCIP first-party scopes) validates the split. Live tree today is package-root oriented (`PackageInput`) without project trust classification — this plan is the missing layer between GUI "open folder" and both the embedded forge and the INDEX sync engine.

---

## Appendix A — Manifest / lock detection cheatsheet

| Ecosystem | Manifests | Workspace file | Lock |
|---|---|---|---|
| Rust | `Cargo.toml` | same `[workspace]` | `Cargo.lock` |
| npm | `package.json` | `workspaces` field | `package-lock.json` |
| pnpm | `package.json` | `pnpm-workspace.yaml` | `pnpm-lock.yaml` |
| yarn | `package.json` | `workspaces` | `yarn.lock` |
| Go | `go.mod` | `go.work` | `go.sum` |
| Python | `pyproject.toml`, `setup.cfg` | uv/pdm workspace tables | `poetry.lock`, `uv.lock`, `pdm.lock`, `Pipfile.lock` |
| Maven | `pom.xml` | aggregator `<modules>` | (none standard) |
| Gradle | `build.gradle*` | `settings.gradle*` | `*.lockfile` |
| NuGet | `*.csproj` | `*.sln` | `packages.lock.json` |
| Nix | `flake.nix` | inputs | `flake.lock` |

## Appendix B — Path-like dependency syntax cheatsheet

| Ecosystem | Syntax |
|---|---|
| Cargo | `{ path = "…" }` |
| npm | `file:…`, `link:…`, `workspace:*` |
| pnpm | `workspace:`, `link:`, `file:` |
| yarn | `workspace:`, `portal:`, `link:` |
| Poetry | `{ path = "…", develop = true }` |
| PEP 508 | `@ file://…` |
| Go | `replace X => ./dir` |
| Maven | module `<module>` / reactor |
| Gradle | `project(":x")`, `includeBuild` |
| NuGet | `ProjectReference` |
| Nix | `inputs.x.url = "path:…"` |

## Appendix C — Diagnostic codes

| Code | Severity | Message pattern |
|---|---|---|
| `trust::path_outside_jail` | warning | path dep {path} not under any project root |
| `trust::symlink_escape` | error | path resolves outside jail via symlink |
| `trust::vendor_ignored` | info | vendor/ not trusted (policy) |
| `trust::git_unindexed` | info | git dep not available from INDEX |
| `resolve::missing_lock` | warning | no lockfile; floating versions |
| `resolve::multi_lock` | warning | multiple lockfiles; chose {pm} |
| `resolve::config_source_replace` | error/warn | ignored registry redirect |
| `resolve::native_tool_failed` | warning | fell back to pure parser |
| `identity::shadow_local_registry` | info | local {name} shadows registry |

## Appendix D — Relationship to other research docs

| Doc | Relationship |
|---|---|
| 01-compiler-audit | Consumes Trusted `PackageInput`; no project graph yet |
| 02-registry-server-audit | INDEX side for Untrusted coordinates |
| 03 / 15 GUI | Project manager UI consumes ResolveResult |
| 14-client-sync | DepSet + typestate Project + SourcePath trust brands |
| 12-orchestration | Job scheduling for trusted recompile vs dep sync |
| 22-crate-topology | May refine multi-crate trusted compile order |

## Appendix E — Test matrix (engineering acceptance)

| # | Fixture | Expect |
|---|---|---|
| T1 | Cargo virtual workspace 3 members | 3 trusted, registry deps untrusted |
| T2 | Path dep outside root | 0 compile for outside; diagnostic |
| T3 | Multi-root includes outside path | path becomes trusted |
| T4 | `[patch]` path in-tree | patched registry id removed from untrusted |
| T5 | Symlink path dep out of jail | reject |
| T6 | npm workspaces + workspace: | members trusted; no node_modules roots |
| T7 | file: outside | diagnostic |
| T8 | go.work two modules + replace | both trusted; proxy modules untrusted |
| T9 | Missing Cargo.lock | Floating quality |
| T10 | Published name = member name | local key ≠ PackageId |
| T11 | pnpm lock only | Untrusted pins match lock |
| T12 | flake path input + github input | path trusted; github untrusted |
| T13 | Vendor dir default policy | untrusted coordinates from lock |
| T14 | NuGet ProjectReference | trusted project; PackageReference untrusted |
| T15 | Gradle includeBuild outside | diagnostic unless multi-root |

---

**End of 16 — Trust Boundaries & Project Model.**
