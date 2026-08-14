# 01 — C and C++ Package/Dependency Sources: Mirroring Survey

Research agent report (2026-07-19), feeding docs/REGISTRYLESS-PLAN.md. Question: what package/dependency sources exist for C and C++, and which can a third-party index practically mirror the way one mirrors crates.io?

## 1. Conan / ConanCenter

**Identity scheme.** `name/version@user/channel` — four-part reference; ConanCenter recipes omit user/channel and use plain `name/version`. Two revision layers sit on top: **RREV** (recipe revision — SHA hash of the recipe manifest, or optionally the Git commit hash when `revision_mode = "scm"`), and **PREV** (package revision — hash of the compiled binary identity, i.e. settings + options + deps).

**Version enumeration.** All recipe files live in the [conan-center-index](https://github.com/conan-io/conan-center-index) Git repository under `recipes/<name>/`. Each package directory contains a `conanfile.py` (the recipe) and can carry multiple version subdirectories. All versions for a library are co-located in one recipe folder.

**Source artifact origin.** Recipes download upstream tarballs at build time using URLs encoded in the recipe; ConanCenter itself does not rehost source archives.

**Machine-readable deps/licenses pre-build?** Yes — recipes declare `requires`, `license` (SPDX preferred), `description`, `homepage`, and `topics` attributes in the `conanfile.py` before any build step. These are statically parsable from the Git repo without running Conan.

**Mirroring practicality.** The entire recipe collection is a plain Git repo — `git clone https://github.com/conan-io/conan-center-index` gives you all recipes and all their history. There is no official REST API in Conan 2 for querying ConanCenter metadata programmatically; queries go through the CLI (`conan search`) or the Artifactory backend (JFrog hosts ConanCenter on Artifactory). For a read-only index, the Git tree is sufficient. Private remotes are first-class: any Conan-compatible Artifactory or Nexus server can be added as an additional remote.

**Scale.** Over 1,500 recipes (the repo celebrated 1,000 recipes in September 2021; docs now say "over 1500 libraries and applications"). The conan.io center page shows entries in the low-to-mid thousands.

**URLs.** [conan-center-index GitHub](https://github.com/conan-io/conan-center-index), [conan.io/center](https://conan.io/center), [Conan docs — local recipes index](https://docs.conan.io/2/devops/devops_local_recipes_index.html)

## 2. vcpkg

**Identity scheme.** Port name only — no version in the identifier itself (e.g., `boost`, `zlib`). Version lives in `vcpkg.json` with one of four versioning schemes: `version`, `version-semver`, `version-date`, or `version-string`. A `port-version` integer tracks packaging-only changes.

**Version enumeration.** The [Microsoft/vcpkg](https://github.com/Microsoft/vcpkg) Git registry maintains a `versions/` database: per-port files at `versions/<first-letter>-/<portname>.json` list every known version plus a `git-tree` SHA pointing to the exact commit in Git history. `versions/baseline.json` names the canonical-latest version of each port. This makes the full version history retrievable by checking out specific git-tree SHAs.

**Source artifact origin.** `portfile.cmake` scripts fetch upstream source archives at install time via URLs; vcpkg does not rehost source. The portfile specifies the upstream tarball URL and expected SHA512 hash.

**Machine-readable deps/licenses pre-build?** Yes — `vcpkg.json` per port lists `dependencies` (with optional feature constraints), `license` (SPDX), `description`, `homepage`, and `supports` expressions, all statically parsable. The versions database and port manifests are fully static JSON/CMake.

**Mirroring practicality.** Excellent — the entire curated registry is a Git repo. Any organization can fork it or operate a custom Git registry (or a filesystem registry). `vcpkg-configuration.json` lets consumers point at any custom registry endpoint. The design explicitly supports third-party and private registries with the same versioning guarantees as the curated one.

**Scale.** 2,558 ports (February 2025) → 2,750 ports (January 2026), growing ~200/year. Sources: [vcpkg February 2025 blog](https://devblogs.microsoft.com/cppblog/whats-new-in-vcpkg-february-2025-package-installation-performance-new-tested-triplet-and-more/), [vcpkg registries concepts](https://learn.microsoft.com/en-us/vcpkg/concepts/registries), [vcpkg versioning reference](https://learn.microsoft.com/en-us/vcpkg/users/versioning).

## 3. Meson WrapDB

**Identity scheme.** Project name only (e.g., `zlib`, `libpng`). Multiple versions of a wrap are tracked as Git tags/releases in the [mesonbuild/wrapdb](https://github.com/mesonbuild/wrapdb) repository.

**Version enumeration.** The wrapdb GitHub repo carries 2,419 releases (individual versioned wrap files), covering a smaller set of distinct projects. Each wrap release is a `.wrap` file specifying `[wrap-file]` or `[wrap-git]` with `source_url`, `source_hash`, and optional patch metadata.

**Source artifact origin.** Wraps reference the **upstream** tarball URL directly (WrapDB does not rehost source). Optionally a patch tarball (Meson build system additions) is fetched from `wrapdb.mesonbuild.com`. Similar to the Debian "unmodified upstream + packaging overlay" model.

**Machine-readable deps/licenses pre-build?** Limited — `.wrap` files identify the upstream source URL and hash; dependency information is in the `meson.build` patch overlays, not in a structured metadata file. License is not expressed in machine-readable form in the `.wrap` file.

**Mirroring practicality.** The whole wrapdb is a Git repo. The service endpoint (`https://wrapdb.mesonbuild.com/`) is also static: wraptool fetches a JSON list of available packages and downloads individual `.wrap` files. Mirror-ability is good: clone the repo and serve the same static JSON/file layout. Scale is modest — a few hundred distinct projects.

**URLs.** [WrapDB GitHub](https://github.com/mesonbuild/wrapdb), [WrapDB projects listing](https://mesonbuild.com/Wrapdb-projects.html), [Wrap dependency system manual](https://mesonbuild.com/Wrap-dependency-system-manual.html).

## 4. Bazel Central Registry (BCR)

**Identity scheme.** Module name + version (e.g., `zlib@1.3.1`). Declared in a `MODULE.bazel` file per version.

**Version enumeration.** The [bazelbuild/bazel-central-registry](https://github.com/bazelbuild/bazel-central-registry) Git repository stores metadata under `modules/<module-name>/<version>/`:
- `MODULE.bazel` — dependency declarations, version constraints, compatibility.
- `source.json` — how to fetch the source (type `archive` by default: URL + integrity hash in SRI format). The registry does **not** rehost source archives.
- `metadata.json` at `modules/<module-name>/` — homepage, maintainers, etc.

**Source artifact origin.** Upstream URLs; the registry is metadata-only (static files).

**Machine-readable deps/licenses pre-build?** `MODULE.bazel` provides structured dep declarations (`bazel_dep()`). Source integrity hashes are in `source.json`. License is not a first-class field in the standard schema.

**Mirroring practicality.** Near-perfect — the BCR is explicitly designed as a static-file registry. Bazel's `--registry` flag accepts any URL serving the same directory layout. A mirror is just `git clone` + serve as static HTTP. The registry spec says "an index registry is a local directory or a static HTTP server."

**Scale.** ~1,115–1,177 modules (8,000–8,842 module versions), growing rapidly since Bazel 8. Mix of Bazel rulesets and C/C++ libraries. Sources: [BCR](https://registry.bazel.build/), [BCR GitHub](https://github.com/bazelbuild/bazel-central-registry), [Q1 2025 Bazel community update](https://blog.bazel.build/2025/04/10/bazel-q1-2025-community-update.html).

## 5. xmake/xrepo, CPM.cmake, Hunter

**xmake/xrepo.** The [xmake-repo](https://github.com/xmake-io/xmake-repo) repository holds Lua-based recipe files (`packages/<letter>/<name>/xmake.lua`) declaring `set_homepage()`, `set_description()`, `add_urls()` with version+hash, `add_deps()`, and build logic. License metadata is present per recipe. Plain Git, supports decentralized private repos, can delegate to vcpkg/Homebrew/Conan. Several hundred to ~1,000 packages.

**CPM.cmake.** A single CMake script wrapping `FetchContent`; no dedicated package database. Dependencies are declared inline in `CMakeLists.txt` pointing at arbitrary Git URLs or archive URLs. No central registry to mirror — the "index" is the community's set of `CMakeLists.txt` files, unindexable at source. [CPM.cmake GitHub](https://github.com/cpm-cmake/CPM.cmake).

**Hunter.** CMake-driven; source packages declared in a central `cmake/packages/` directory in the [ruslo/hunter](https://github.com/ruslo/hunter) repo (largely inactive). Each package entry holds version and download URL. Low adoption; no active API.

Both CPM and Hunter are effectively frozen or minimalist relative to vcpkg/Conan. Neither exposes a mirrored registry in any structured sense.

## 6. Distro Metadata

### Debian/Ubuntu
Debian source packages are described by `.dsc` files and `debian/control`, exposed in bulk as compressed `Sources.gz` / `Sources.xz` index files per suite (e.g., `http://deb.debian.org/debian/dists/bookworm/main/source/Sources.xz`). Each source record includes: `Package`, `Version`, `Build-Depends`, `Build-Depends-Indep`, `Depends` (from binary `control`), `Homepage`, `Standards-Version`. Machine-readable copyright is in `debian/copyright` (DEP-5 format, SPDX optional). **Dep-graph richness: high** — build deps explicit. **Mapping problem:** Debian package names frequently diverge from upstream project names (e.g., `libjpeg-turbo8-dev` → libjpeg-turbo). Sources: [Debian Policy — control fields](https://www.debian.org/doc/debian-policy/ch-controlfields.html), [Debian source packages](https://www.debian.org/doc/debian-policy/ap-pkg-sourcepkg.html).

### Fedora/RHEL
RPM `.spec` files under Fedora's [dist-git](https://src.fedoraproject.org) declare `BuildRequires`, `Requires`, `License`, `URL`, `Source0`. Fedora exposes a [Koji build API](https://koji.fedoraproject.org/koji/api) and [mdapi](https://apps.fedoraproject.org/mdapi/) for SQLite dumps of package metadata. Dep-graph richness: high. Mapping problem: similar to Debian.

### Homebrew
Rich JSON API at [formulae.brew.sh](https://formulae.brew.sh/docs/api/): `https://formulae.brew.sh/api/formula.json` returns all ~2,000–2,500 core formulae in one JSON array, each with `name`, `desc`, `license` (SPDX), `homepage`, `versions`, `urls`, `dependencies`, `build_dependencies`, `optional_dependencies`, `recommended_dependencies`. **Best machine-readable dep+license coverage among distro sources.** SPDX license requirement since ~2020. Mapping problem: moderate — Homebrew names usually close to upstream.

### Alpine Linux
`APKINDEX.tar.gz` per repository is a plaintext record file (one field per line, blank-line-separated records) with package name, version, architecture, description, URL, license, and depend/provides fields. Structured and machine-readable without a build step. Sources: [Alpine Package Keeper wiki](https://wiki.alpinelinux.org/wiki/Alpine_Package_Keeper), [APKINDEX spec](https://wiki.alpinelinux.org/wiki/Apk_spec).

### Arch Linux
`PKGBUILD` files in [arch/svntogit](https://github.com/archlinux/svntogit-packages) express `depends`, `makedepends`, `license`, `url`, `source`. Also exposed via [pkgfile](https://wiki.archlinux.org/title/pkgfile) and the [Arch Linux package API](https://archlinux.org/packages/). Dep-graph richness: medium. Arch names often closer to upstream than Debian.

**Cross-distro naming problem (all distros).** None use canonical upstream identifiers (CPE, PURL) consistently. `libssl-dev` (Debian), `openssl-devel` (Fedora), `openssl` (Homebrew), `openssl-dev` (Alpine) all refer to the same upstream project. Resolving this requires an explicit ruleset — see Repology.

## 7. Repology

**What it aggregates.** Monitors and normalizes package versions from more than 120 repositories: Linux distros, BSD ports, Homebrew, MacPorts, and language-specific repos (Conan, vcpkg listed). Does **not** index CPM/CMake-style inline deps.

**Normalization mechanism.** The [repology-rules](https://github.com/repology/repology-rules) YAML ruleset maps package names across repos to a canonical upstream project name by merging aliases (e.g., `firefox`, `firefox-lts`, `firefox43` → `firefox`) and splitting ambiguities. The only public, maintained cross-distro name-to-upstream mapping for C/C++ libraries.

**API.** REST API at `https://repology.org/api/v1/project/{name}` and paginated bulk endpoint `/api/v1/projects/`. Returns per-package: `repo`, `srcname`, `binname`, `version`, `status`, `summary`, `licenses`, `maintainers`. **Dependencies are not in the API response.** Bulk access: PostgreSQL database dumps at [dumps.repology.org](https://dumps.repology.org). Rate limit: 1 req/s.

**Data licensing.** Repology code is GPLv3+; the data licensing is not explicitly documented (aggregated from upstream sources with their own licenses).

**URLs.** [Repology API](https://repology.org/api), [Repology about](https://repology.org/docs/about), [repology-rules](https://github.com/repology/repology-rules), [repology-updater](https://github.com/repology/repology-updater).

## 8. Adoption Reality: Survey Numbers

**2023 ISO C++ Developer Survey "Lite"** (n ≈ 1,700): How do C++ developers manage third-party libraries?

| Method | % |
|---|---|
| Library source inlined/vendored into build | 68.11% |
| Compile libraries separately (manual) | 48.30% |
| System package managers | 36.23% |
| Download prebuilt libraries | 27.43% |
| vcpkg | 21.34% |
| Conan | 18.93% |
| Other | 11.25% |
| NuGet | 7.50% |
| None of the above | 1.41% |

**2024 Modern C++ DevOps survey** ([source](https://moderncppdevops.com/2024-survey-results/)):

| Method | % |
|---|---|
| Inlined/vendored code | 68.54% |
| Dedicated build instructions | 48.48% |
| System package managers | 37.80% |
| Downloaded prebuilt libraries | 25.60% |
| Conan | 19.34% |
| Vcpkg | 19.10% |
| NuGet | 5.30% |
| Other | 14.53% |

Roughly **two-thirds of C++ developers vendor or inline** deps directly, entirely outside any indexable package manager. Managing third-party libraries is the #1 pain point (47% of respondents, ISO C++ survey). Sources: [ISO C++ 2023 survey PDF](https://isocpp.org/files/papers/CppDevSurvey-2023-summary.pdf), [ISO C++ 2024 results](https://isocpp.org/blog/2024/04/results-summary-2024-annual-cpp-developer-survey-lite), [JetBrains C++ 2023](https://www.jetbrains.com/lp/devecosystem-2023/cpp/), [Modern C++ DevOps 2024](https://moderncppdevops.com/2024-survey-results/).

## Ranked Shortlist: Best 3–4 Sources for a C/C++ Package Index

### Tier 1 — Clone and parse immediately
1. **vcpkg curated registry** — all 2,750 ports have structured JSON (`vcpkg.json`: name, version, SPDX license, description, deps) and a fully enumerable versions database with git-tree SHAs. Zero API dependency. Best-in-class structured metadata, most complete version history, excellent mirroring support by design.
2. **Conan / conan-center-index** — 1,500+ recipes, each with statically-parsable `license`, `description`, `requires`, `homepage`, `topics`. RREV/PREV model means binary-level provenance is trackable. Strong enterprise adoption (~19%).

### Tier 2 — High value, slightly more work
3. **Homebrew formulae API** — single HTTP request returns all formulae with structured deps + SPDX licenses. Most popular C/C++ system libraries present. Best dep-graph + license metadata among distro-class sources; trivially bulk-downloadable.
4. **Repology (DB dump)** — normalized upstream-project identities across 120+ repos. No dep graphs, but provides the critical name→upstream-project mapping. `repology-rules` is the de-facto community standard for C/C++ name normalization.

### Tier 3 — Complementary, lower ROI
**Bazel Central Registry** and **Meson WrapDB** add build-system-specific coverage but overlap heavily with vcpkg/Conan. **Distro Sources files** (Debian, Alpine, Arch, Fedora) add tens of thousands of packages but require the Repology-style name-normalization layer, and dep-graph quality varies.

## The Irreducible Coverage Gap

Even with all Tier 1–2 sources combined, **~68% of C/C++ projects are not indexed by any package manager**. They use vendored source trees, Git submodules, manual tarballs, or bundled copies — visible only through:
- Source-level analysis (scanning `CMakeLists.txt`, `configure.ac`, `Makefile` for `find_package`, `pkg_check_modules`, `FetchContent_Declare`).
- Binary SCA tooling (matching compiled artifacts against known library signatures).
- CPE/PURL databases (NVD, OSV) which track at the advisory level but don't model dep trees.

There is no structured, indexable metadata source for the majority C++ practice of "copy the source in." This is the defining gap separating C/C++ indexing from crates.io.
