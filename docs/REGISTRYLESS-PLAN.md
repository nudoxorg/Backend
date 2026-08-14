# docs/REGISTRYLESS-PLAN.md — C/C++, git-native, and internal ecosystems on the INDEX catalog

Rev 1 (2026-07-19). Status: normative, implementation-grade. Junior-executable.

Scope: how the index handles ecosystems that have **no dependency manager at cargo scale** — C/C++ as the flagship, internal company code as the second target. Builds strictly on the existing abstractions: the `EcosystemSpec` trait (`workspace/ecosystem/`), the INDEX-PLAN catalog (schema v4, ObjectPack, grit `GitMonitor`, CatalogOp), and the frozen IR/StableRef laws in `docs/research/resolution/04-ir-unification.md`. Nothing here adds a structure beside the IR or the catalog; everything is a new `EcosystemSpec` impl, new followers, additive DDL, and pure derivations.

Companion research (verbatim agent reports): `docs/research/ecosystems/01-cpp-landscape.md`, `02-identity-provenance.md`, `03-registryless-patterns.md`.

---

## 0. Purpose (one paragraph)

Rust has crates.io; C/C++ has a long tail of partial registries (vcpkg 2,750 ports, ConanCenter ~1,500 recipes, Homebrew ~2,500 formulae) and a majority practice — **~68% of C++ developers vendor or inline dependencies** (ISO C++ 2023/Modern C++ DevOps 2024 surveys) — that no registry sees at all. Internal company code has the same shape: git repos, no registry, no download counts, sometimes no tags. This plan makes **the git repository the package**: identity is a normalized repo slug (the `RepoSlug` we already compute), versions come from tags with a Go-style pseudo-version fallback, curated registries (vcpkg/Conan/Homebrew) are demoted from "the registry" to **feeds** that alias curated names onto repo stems and contribute version pins, and dependency edges come from build-descriptor extraction recorded exactly as the producer sees them (EDB) with resolution derived later (IDB). The same machinery, minus the public feeds, is the internal-code story.

---

## 1. Research basis (what we know, with numbers)

### 1.1 The C/C++ reality

| Fact | Number | Source |
|---|---|---|
| Developers vendoring/inlining dep source | 68.1% / 68.5% | ISO C++ 2023 survey, Modern C++ DevOps 2024 |
| vcpkg adoption / Conan adoption | ~21% / ~19% | same surveys |
| "Managing third-party libs" as #1 pain point | 47% | ISO C++ 2023 |
| vcpkg curated ports | 2,750 (2026-01), +~200/yr | vcpkg blog |
| conan-center-index recipes | 1,500+ | conan.io |
| Homebrew core formulae (one-request JSON API) | ~2,500 | formulae.brew.sh |
| Bazel Central Registry modules | ~1,177 (8k+ versions) | BCR |
| deps.dev C/C++ coverage | **zero** ("no clear packaging model") | docs.deps.dev/faq |

Consequences: (a) no single feed covers even half the ecosystem — the git-native path is the primary identity plane, feeds are enrichment; (b) there is no download-count signal — ranking needs a substitute; (c) the vendored majority is only reachable later via content matching (Phase R, stretch); (d) content affinity alone cannot distinguish a **real fork** of a clib from a **vendored copy** of it — git ancestry must be a first-class signal (§3.6), because the two demand opposite treatment (own package vs dependency edge).

### 1.2 The proven pattern (Go, Swift, Bazel, Nix)

Every successful registry-less ecosystem converges on the same four layers (see `03-registryless-patterns.md`):

1. **Discovery feed** — a scannable list (Go: `index.golang.org`; Swift: one `packages.json` of git URLs; BCR: `metadata.json` per module; us: feeds + add-by-URL).
2. **VCS version enumeration** — semver git tags, plus **pseudo-versions** for untagged commits (Go's exact grammar: `v0.0.0-<yyyymmddhhmmss>-<12hexhash>`, three forms depending on nearest ancestor tag).
3. **Content-addressed archive + hash pin** — an immutable per-(package, version) archive identified by hash (Go `h1:`; BCR SRI; Nix `narHash`; **us: ObjectPack, already built**).
4. **Optional transparency log** — sumdb/Rekor; out of scope v1, our substitute is `registry_checksum` cross-checks recorded from feeds.

Go also proves the failure modes we must handle: tags can move (pin the rev hash at enumeration), case-sensitivity (`!`-escaping — we avoid by keeping slugs lowercase via `normalize_repo_url`), vanity URL churn (slug is identity; renames are a new stem + alias), major-version path suffixes (not applicable to cpp slugs).

### 1.3 Identity research

- **purl** names what a registry knows: `pkg:conan/zlib@1.3.1`, `pkg:github/curl/curl@7.50.3`, `pkg:generic/zlib@1.2.11?vcs_url=…@<sha>`. It is an interop rendering, not an identity source.
- **SWHID** (ISO/IEC 18670, 2025) content-addresses files/trees/commits with git-compatible SHA-1; Software Heritage archives 56B files / 415M projects. `docs/research/librarification/05` already froze: **SWHID class hashes are T0 dedup only, never identity**.
- **CPE fails structurally** for C/C++ (no canonical names, string-equality versions, no content binding). Do not build on it.
- **OSV** handles C/C++ via `GIT` commit ranges (`{"type":"GIT","repo":…,"events":[{introduced…},{fixed…}]}`) — the only advisory scheme that works without a registry; 30k+ NVD advisories enriched. Maps directly onto our repo-slug stems (Phase S-A).
- **Component/version detection** (Centris ICSE'21 P91/R94 on 10,241 repos / 229k versions; TIVER ICSE'25: 67% of vendored components mix functions from 3+ versions, P88.5/R91.6): function/file-hash matching against a versioned corpus works but is research-grade. We schedule a bounded file-hash variant (Phase R) — never per-function TLSH in v1.

### 1.4 Gaps this plan closes (from the `.research` sweep)

The `.research` tree's only treatment of C/C++ packaging is `17-commit-gate §4.4.6`: "whole repo as one package if no manifest; accept coarse dirty." Conan/vcpkg/pkg-config/purl/OSV have **zero mentions** anywhere in `.research`. The sweep's gap list D-1..D-11 maps here as: D-1 identity → §3.1; D-2 dep discovery → §8; D-3 header-only → RL-9; D-4 internal repos → §10; D-5 no-semver versions → §3.3; D-6 system libs → §3.5; D-7/D-8 purl/OSV/SBOM → §3.4, §12; D-9 compile_commands → §13 (non-goal v1) + §8; D-10 multi-root → RL-9; D-11 incremental producer → out of scope (IR plane).

Frozen laws respected throughout: StableRef `F:<eco>/<pkg>#<hex>` is the only foreign reference (no `NudoxPath::External`, no `~extern`); consumer codebases are packages on `local/<device-id>` (U-7); models-as-packages on `overlay/models` (§16); one Trustfall query surface; CAS = BLAKE3 + ObjectPack; generations are commit-gated `BLAKE3(project‖repo‖commit_oid[‖submodule_pins])`; vendored trees UNTRUSTED by default (16-trust).

---

## 2. Ground decisions (complete list)

- **RL-1 — The repo slug is the stem.** For the `cpp` ecosystem (and any future registry-less ecosystem), `packages.name_canonical` = normalized `RepoSlug` (`repo.rs`: lowercase `host/path`, scheme/credentials/`.git` stripped). Example stem: `github.com/curl/curl`. Internal: `git.corp.example/platform/allocator`.
- **RL-2 — One ecosystem string `"cpp"` covers C and C++.** Language facet (c vs c++ vs mixed) is a derived fact from manifests/file extensions, not an identity axis. `Language::Cpp` is the new enum variant.
- **RL-3 — Feeds are not registries.** vcpkg, conan-center-index, Homebrew (and later BCR/WrapDB) are **alias + version-pin feeds**. They resolve a curated name to a repo stem, contribute `ListedVersion`s, upstream checksums, licenses, and dependency edges. They never own identity. If a feed entry's upstream repo is truly underivable, fall back to a feed-scoped stem (`authority = "vcpkg"`, name = port) — expected to be rare; log it.
- **RL-4 — Version grammar = Tag ▸ Date ▸ Pseudo ▸ Raw.** Semver-ish tags when present; `version-date` (vcpkg) accepted; Go's exact pseudo-version grammar for untagged commits; raw lexical as last resort. Total order across kinds: Tag > Date > Pseudo > Raw; natural order within kind. Enumeration always records the **commit hash** alongside the tag (tags are mutable; `source_rev` pins).
- **RL-5 — Edges are EDB: store what the extractor saw.** `edges.dep_name_canonical` = the literal requirement token (`zlib`, `ZLIB`, `libpng16`), `edges.kind` = the mechanism (`find_package` | `pkg_config` | `submodule` | `fetchcontent` | `wrap` | `recipe` | `bazel_dep`), `edges.source` ∈ existing `"feed" | "manifest"` (+ documented new value `"detected"` for Phase R). `resolved_stem` is filled by a **pure resolution pass** over the alias table (IDB). No DDL change needed — these columns exist and are TEXT.
- **RL-6 — Aliases are a first-class additive table.** `package_aliases` maps `(ecosystem, alias_kind, alias) → stem_id` with a confidence tier. Feed names, `find_package` names, pkg-config names, and (curated, later) distro names all live here. This is our in-house, provenance-tracked version of Repology's rules.
- **RL-7 — Do not bulk-import Repology data.** Its data licensing is unclear (code GPLv3+, data unspecified). Use repology-rules only as a *reference while hand-curating* our seed alias file.
- **RL-8 — purl and SWHID are pure renderings.** Derived from `(stem, version, source_rev)` at export/query time (`pkg:github/...` when host is github, else `pkg:generic?vcs_url=<url>@<rev>`; `swh:1:rev:<rev>` when rev is a git SHA-1). Never stored as identity, satisfying the SWHID-is-T0-only law.
- **RL-9 — Whole repo = one package in v1.** Monorepos (Boost) get many aliases (`boost-algorithm`, `boost-asio`, …) resolving to one stem. Subpath-granular packages are explicitly deferred (ticket in §15 risks); this matches 17-commit-gate's accepted coarseness.
- **RL-10 — StableRef grammar note.** `F:cpp/github.com/curl/curl#<hex>` — the `<pkg>` field of `F:<eco>/<pkg>#<hex>` may contain `/`; `#` remains the sole terminator. Record this in the F1 grammar doc when touching it (no parser change expected; verify in P1 gate).
- **RL-11 — System/toolchain libraries are model packages.** `libc`, `posix`, `stdcpp`, `pthread`, `openssl-system`, `zlib-system` seed set on `overlay/models`, stems `system/<name>` (authority `"system"`). Resolves `.research` gap D-6 inside the frozen models-as-packages mechanism. `Threads`, `CMake`'s `Threads::Threads` etc. alias here.
- **RL-12 — No download counts; presence is the popularity prior.** `download_source()` stays `None`. Derived facet `presence` = count of distinct `alias_kind` feeds referencing the stem (0..n). Cheap, honest, computable from `package_aliases`. Ranking treats it like a small downloads substitute; the skip path for absent downloads already exists (ECOSYSTEM-PLAN §9).
- **RL-13 — Internal mode is an egress policy, not a fork.** A host-glob config (GOPRIVATE analog) marks hosts internal: no public feed lookups, no advisory calls, no outbound anything for matching stems. Discovery = add-by-URL route + optional host repo-list file. Applies to every ecosystem (a private Rust git dep benefits too, later). Sharing uses trusted remote upload (ID-18); public DHT/pinning stays rejected (edge-tech decision).
- **RL-14 — The listing body for cpp is `ls-remote` output.** `parse_version_listing` receives the bytes of `git ls-remote --tags --heads <url>` (produced by the IO layer via grit, not HTTP). The trait stays total and unchanged; go.rs already set the precedent of a plain-text listing body.
- **RL-15 — This plan is the catalog plane only.** IR production for C/C++ (clang oracle vs treesitter-skeleton dual-fidelity) is governed by 04-ir-unification and is out of scope here; C++ is already in the frozen 8-language IR set. This plan delivers everything the producer needs: stems, versions, ObjectPacks, toolchain column, edges.
- **RL-16 — Fork ≠ mirror ≠ vendored copy; the stem-vs-edge law.** A **real fork** of a clib is a package: its own stem (its slug is genuinely different), linked to the upstream stem by `repo_lineage(fork_of)` with a fork-point rev. A **mirror** is a stem with `mirror_of` lineage and no independent commits. A **vendored copy** is *never* a package: it is a subtree inside some host package's pack, recorded as a `vendored` edge host→upstream and excluded from usage mining and search results. Git ancestry is the primary discriminator; content overlap is only the fallback. Procedure in §3.6.
- **RL-17 — Advisories flow through lineage.** OSV GIT ranges evaluate on fork stems via the fork's **own** commit ancestry (shared history means `introduced`/`fixed` commits are directly checkable in the fork's DAG — a fork that cherry-picked the fix is correctly clean). Vendored edges surface upstream advisories on the host at heuristic confidence over the estimated version range.

---

## 3. Identity model (normative)

### 3.1 Names

`parse_name` for `Cpp` accepts, in order:
1. Anything `repo::normalize_repo_url` accepts (full URLs, SCP-style, `host/org/repo`) → `StructuredName { authority: Some(host), namespace: [path segments minus last], name: last segment, major: None, original: raw }`.
2. `system/<name>` → `authority: Some("system")`.
3. `vcpkg/<port>` / `conan/<name>` (feed-scoped fallback stems only; followers emit these, users normally never type them).

`render_canonical` joins `authority "/" namespace… "/" name` lowercase. Totalness: population = slugs + the two scoped forms; bare single tokens (`zlib`) are **not** valid cpp names — they are aliases, resolved before `parse_name` (server-side query normalization hook, §9).

### 3.2 Aliases

```
alias_kind ∈ "vcpkg_port" | "conan_recipe" | "brew_formula" | "find_package"
           | "pkg_config" | "meson_wrap" | "bazel_module" | "distro_debian" | …
confidence ∈ "authoritative"   -- feed itself declares the upstream repo (portfile REPO)
           | "curated"          -- our seed file / human mapping
           | "heuristic"        -- derived (homepage URL sniff), display-gated
```

### 3.3 Versions

```rust
pub enum CppVersion {
    Tag(TagVer),                       // v?MAJOR[.MINOR[.PATCH[.EXTRA]]][-pre][+build], loose
    Date { y: u16, m: u8, d: u8, port: u32 },   // vcpkg version-date [+ port-version]
    Pseudo { base: Option<Box<TagVer>>, ts: u64, hash12: SmolStr },  // Go grammar, 3 forms
    Raw(SmolStr),                      // lexical, last resort
}
```
`is_prerelease`: `Pseudo` → true; `Tag` with pre-segment → true; else false. `range_matches`: exact string equality, plus caret-semantics when both sides parse as `Tag` (mirrors go.rs's minimalism — no invented range syntax). Pseudo-version synthesis (P6): nearest ancestor tag via grit ⇒ pick the correct of Go's three forms; no tag ⇒ `v0.0.0-<ts>-<hash12>`; timestamp = commit time UTC.

### 3.4 Derived interop identifiers (pure functions, ecosystem crate)

```rust
pub fn purl(stem: &StructuredName, version: &str, rev: Option<&str>) -> String;
// github.com host → pkg:github/<org>/<repo>@<version>
// else           → pkg:generic/<name>@<version>?vcs_url=https://<slug>.git@<rev>
pub fn swhid_rev(rev_sha1: &str) -> String;   // "swh:1:rev:<hex>"
```
Used by SBOM/export projections and the Trustfall adapter; never persisted as keys.

### 3.5 System model packages

Six seed stems on `overlay/models`: `system/libc`, `system/posix`, `system/stdcpp`, `system/pthread`, `system/openssl`, `system/zlib` (the last two so `pkg_config: openssl|zlib` hits *something* even before the real stems are indexed — alias table points `pkg_config/zlib → github.com/madler/zlib` once P4 runs and the curated alias wins by confidence). Model content (sum.* frames) is IR-plane work; here we only create the packages + stub facts (P10).

### 3.6 Fork vs mirror vs vendored copy (normative discrimination procedure)

The same bytes can mean three different things; the index must never conflate them. Given high content affinity between a candidate tree and a known stem `U`:

**Case A — the candidate is a whole repo `R` (a candidate stem).**
1. **History test (primary).** Via grit (we hold clones for monitored stems, so `U`'s DAG is local): does `R` share root commits / a merge-base with `U`?
   - Shared ancestry **and** commits unique to `R` after the merge-base → `fork_of U`, evidence `"git_history"`, `fork_point_rev` = merge-base oid, confidence `authoritative`.
   - Shared ancestry **and** `R`'s HEAD is an ancestor of (or equal to) some `U` rev → `mirror_of U`, same evidence.
2. **Content test (fallback, needs Phase R's `blob_index`).** No shared ancestry (squashed import, rehosted history): if `R`'s distinctive-file overlap with `U` has `overlap_ratio ≥ 0.6` → `fork_of U`, evidence `"content_overlap"`, confidence `heuristic`; the import point is estimated TIVER-style (the `U` version whose file set is the closest superset). Below threshold → no link; **unlinked is the safe default**.

**Case B — the candidate is a subtree of a host stem's pack** (path heuristics `vendor/`, `third_party/`, `deps/`, `extern/`, `external/` — or a `blob_index` hit cluster confined to one directory): emit a `vendored` edge host→`U` (`kind="vendored"`, `source="detected"`, `requirement` = estimated version range). Never create a stem. Vendored subtrees stay excluded from usage mining (02-prior-art §7.3) and UNTRUSTED (16-trust); their symbols are attributed to `U`'s coordinate space, not the host's.

Composition: a repo can simultaneously *be* a fork (Case A) and *vendor* other libraries (Case B per subtree) — the cases apply independently per tree region. History evidence always outranks content evidence when both exist.

**Candidate generation (cheap, v1):** (a) stems sharing a terminal name segment; (b) `FetchContent`/portfile URLs that differ from the alias-canonical stem for the same token (feeds occasionally point at patched forks — record the `authoritative` alias to the fork stem *and* the lineage to canonical); (c) blob-overlap candidates once Phase R lands. Host-API fork flags (GitHub) are deliberately **not** required — the procedure must work on internal hosts too.

---

## 4. What already exists (inventory — verify, don't rebuild)

| Piece | Where | State |
|---|---|---|
| `RepoSlug` + `normalize_repo_url` | `workspace/ecosystem/repo.rs` | Built; handles npm shorthands, SCP, bare `user/repo` |
| Plain-text listing precedent | `go.rs:84-100` (`/@v/list` parsing) | Built |
| Non-JSON-API feed precedent | `nix.rs:73-93` (FlakeHub) | Built |
| `SourceAcquisition::Git { url, rev, registry_checksum }` | heart / INDEX-PLAN §6.3 | Specified (build state per INDEX-PLAN waves) |
| ObjectPack v1 (NDPK, `ObjectPackId(ContentBlake3)`) | INDEX-PLAN §6.2 | Specified |
| `GitMonitor { grit, catalog }` + `git_watermarks` | INDEX-PLAN §6.4, §8 | Specified |
| `versions.source_kind = "git"`, `source_rev`, nullable checksums | schema v4 §8 | Specified — **no change needed** |
| `edges` with nullable `resolved_stem`, TEXT `kind`/`source` | schema v4 §8 | Specified — **no change needed** |
| Ranking skip for absent downloads | ECOSYSTEM-PLAN §9 | Built |
| `[mirror] follow = []` opt-in per follower | ECOSYSTEM-PLAN §10 | Built |

The catalog columns were designed with the git path already in mind; this plan's schema footprint is therefore only the two tables in §5.

---

## 5. Schema deltas (additive; migration per INDEX-PLAN §13)

```sql
-- NEW
CREATE TABLE package_aliases (
  ecosystem       TEXT NOT NULL,
  alias_kind      TEXT NOT NULL,
  alias           TEXT NOT NULL,     -- normalized (lowercase, trimmed)
  stem_id         BLOB16 NOT NULL,
  confidence      TEXT NOT NULL,     -- "authoritative" | "curated" | "heuristic"
  recorded_at     INTEGER NOT NULL,
  PRIMARY KEY (ecosystem, alias_kind, alias)
);
CREATE INDEX idx_aliases_stem ON package_aliases(stem_id);

-- NEW (RL-16; written by the P11 classification pass, read by search + advisories)
CREATE TABLE repo_lineage (
  stem_id         BLOB16 NOT NULL,   -- the fork/mirror
  relation        TEXT NOT NULL,     -- "fork_of" | "mirror_of"
  target_stem     BLOB16 NOT NULL,   -- the upstream/canonical stem
  evidence        TEXT NOT NULL,     -- "git_history" | "content_overlap"
  fork_point_rev  TEXT,              -- merge-base oid when evidence = git_history
  overlap_ratio   REAL,              -- when evidence = content_overlap
  confidence      TEXT NOT NULL,     -- "authoritative" | "heuristic"
  recorded_at     INTEGER NOT NULL,
  PRIMARY KEY (stem_id, relation, target_stem)
);
CREATE INDEX idx_lineage_target ON repo_lineage(target_stem);

-- NEW
CREATE TABLE feed_watermarks (
  feed            TEXT PRIMARY KEY,  -- "vcpkg" | "conan-center" | "homebrew" | "osv"
  last_ref        TEXT,              -- registry repo commit / HTTP ETag / feed cursor
  last_checked_at INTEGER NOT NULL,
  last_error      TEXT
);
```

Documented (no DDL): `edges.source` gains value `"detected"` (Phase R); `edges.kind` cpp vocabulary per RL-5; `listing_events.status` `"advisory"` used by Phase S-A (value already in the v4 enum).

New CatalogOps (wire additions in §8.1 style):

```rust
UpsertAlias { ecosystem: SmolStr, kind: SmolStr, alias: SmolStr,
              stem: PackageStemId, confidence: AliasConfidence },
UpsertLineage { stem: PackageStemId, relation: LineageRelation,
                target: PackageStemId, evidence: LineageEvidence,
                fork_point_rev: Option<GitRev>, overlap_ratio: Option<f32>,
                confidence: AliasConfidence },
// SourceMoved already exists and is reused unchanged for git ticks.
```

---

## 6. Trait work (`workspace/ecosystem/`) — exact shape

New file `cpp.rs`, plus mechanical touches. Every step below is a compile-error-driven checklist:

1. `language.rs`: add `Language::Cpp`. Workspace-wide `cargo check` now lists every exhaustive match to fix (this is the designed forcing function; expect the `lib.rs:307` dispatch table, `AnyVersion`, and any `RegistryOrigin`-style matches in the registry crate — find them with `rg "Language::" --type rust -l | grep -v ecosystem` and `rg "RegistryOrigin"`).
2. `version.rs`: add `AnyVersion::Cpp(CppVersion)` + compare arm.
3. `cpp.rs`:

```rust
impl EcosystemSpec for Cpp {
    const LANGUAGE: Language = Language::Cpp;
    fn parse_name(raw: &str) -> Option<StructuredName>   // §3.1
    fn render_canonical(n: &StructuredName) -> String    // lowercase slug join
    type Version = CppVersion;                           // §3.3
    const ARCHIVE: ArchiveKind = ArchiveKind::TarGz;     // used only on the tarball fallback path
    fn endpoints() -> UpstreamEndpoints {
        // listing template is a marker the IO layer intercepts (RL-14):
        UpstreamEndpoints { listing: "git+ls-remote://{name}", archive: "", listing_status: None }
    }
    const POLICY: UpstreamPolicy = /* 1 rps, retry 3, respect_retry_after */;
    fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<CppVersion>>
        // parses `ls-remote` lines: "<oid>\trefs/tags/v1.2.3" (dereference "^{}" entries,
        // prefer the peeled oid). Emits Tag versions; records oid in ListedVersion.raw
        // as "<tag>@<oid>" so the ingestor can pin source_rev without a second call.
    type Manifest = CppManifest;                          // §8
    fn manifest_candidates() -> &'static [ManifestCandidate]
        // ordered: vcpkg.json, conanfile.py, conanfile.txt, CMakeLists.txt,
        // meson.build, MODULE.bazel, .gitmodules, *.pc.in / *.pc
    fn parse_manifest(c: &ManifestCandidate, bytes: &[u8]) -> Option<CppManifest>  // §8
    fn search_norms() -> &'static SearchNorms
        // stopwords: lib, library, cpp, cxx, c; strip_conventions: leading "lib";
        // downloads_scale: None
    // download_source / parse_download_count: defaults (None) — RL-12
}
```

4. `lib.rs` dispatch table: add the `Language::Cpp => …` arm.
5. `name_tests.rs`: totalness cases (URLs, SCP, bare slugs, `system/…`, rejection of bare `zlib`).

Gate G-6: `cargo test -p ecosystem`; zero `Language::` matches outside the ecosystem crate left unfixed (`rg` gate per ECOSYSTEM-PLAN Phase 8).

---

## 7. Followers & monitors (registry crate, `[mirror] follow` opt-in each)

All three followers implement the existing `CatalogFollower` trait (read `registry/upstream/mod.rs` first; match its shape exactly). Watermarks in `feed_watermarks`.

### 7.1 Homebrew (warm-up — pure HTTP JSON, closest to existing followers)

1. `GET https://formulae.brew.sh/api/formula.json` with `If-None-Match` (watermark = ETag).
2. Per formula: `name, desc, license (SPDX), homepage, versions.stable, urls.stable.{url,checksum}, dependencies, build_dependencies`.
3. Stem resolution: `urls.stable.url` or `homepage` through `normalize_repo_url` → confidence `heuristic`; recognizable github release tarball URLs → `authoritative`.
4. Emit `UpsertPackage`, `UpsertAlias(brew_formula)`, `UpsertVersion` (`registry_checksum` = sha256; `source: None` — brew tarballs are the *reconstruction fallback*, git is preferred when P6 enumerates the stem directly), edges `source="feed", kind="recipe"` from `dependencies`.

### 7.2 vcpkg (richest metadata)

1. grit shallow-clone/fetch `Microsoft/vcpkg` (configurable fork URL for enterprises); watermark = HEAD commit; process only changed paths since watermark.
2. Enumerate `versions/<letter>-/<port>.json` → every `{version…, port-version, git-tree}`.
3. Read that `git-tree`'s `vcpkg.json` via grit object access (no checkout): `description, license (SPDX), homepage, dependencies, supports`.
4. Read `portfile.cmake` at the same tree: `vcpkg_from_github/gitlab/bitbucket(REPO org/name, REF <tag-or-sha>, SHA512 …)` → stem (`authoritative`) + `SourceAcquisition::Git { url, rev: REF, registry_checksum: Some(sha512→normalized) }`. Ports using `vcpkg_download_distfile` only → `ReconstructedRegistryPackage { package_uri: <tarball url>, checksum }`. Neither present → feed-scoped stem `vcpkg/<port>` (log; RL-3).
5. Emit package/alias/version/edges (`kind="recipe"`, deps are vcpkg port names — recorded literally per RL-5; resolution pass joins them via `vcpkg_port` aliases).

### 7.3 conan-center-index

1. grit clone/fetch `conan-io/conan-center-index`; watermark = HEAD.
2. Per `recipes/<name>/config.yml`: version → folder map.
3. Per folder: `conandata.yml` → `sources[version] = {url, sha256}` (machine-readable pin; parse this **before** touching Python). `conanfile.py` → static text scan only (never execute): `license = "…"`, `description`, `homepage`, `topics`, `requires`/`self.requires("dep/version")` regexes; misses are logged, not fatal.
4. Stem from `homepage`/source URL as in 7.1/7.2; alias `conan_recipe`; edges `kind="recipe"` with literal `dep` names.

### 7.4 Direct-git packages (the primary plane; ecosystem-agnostic machinery)

1. Server route `POST /v1/packages/git { url, ecosystem? }` (default ecosystem inferred later; v1 requires `"cpp"`): normalize slug → `UpsertPackage` → enqueue enumeration.
2. Enumeration job: grit `ls-remote` (bytes handed to `parse_version_listing` per RL-14) → `UpsertVersion` per tag with `source: Git { url, rev: <peeled oid>, registry_checksum: None }`. Zero tags → one pseudo-version from HEAD (§3.3).
3. Acquisition (existing pipeline): checkout rev → seal ObjectPack → `source_pack` set → IR work queued (RL-15 boundary).
4. `GitMonitor.tick(stem)` (already specified, `git_watermarks` already in v4) covers cpp stems with **no code change** beyond registering cpp stems for ticks; new tag ⇒ `CatalogOp::SourceMoved` ⇒ steps 2–3 incrementally.

---

## 8. Manifest extraction & edges (dual use: feed trees and our own ObjectPacks)

`CppManifest` (implements `ManifestFacts`) carries `ExtractedFacts` plus typed dep records `(token, mechanism)`. Parsers are pure text scans; each ~50–100 lines + fixture tests (mirroring ECOSYSTEM-PLAN §7.2 sizing):

| Candidate | Extract | Edge kind |
|---|---|---|
| `vcpkg.json` | description, license, homepage, dependencies | `recipe` |
| `conanfile.py` / `.txt` | license/description/topics; `requires` | `recipe` |
| `CMakeLists.txt` | `project(… DESCRIPTION …)`; `find_package(<N>…)`; `pkg_check_modules(… <names>)`; `FetchContent_Declare(<n> GIT_REPOSITORY <url> GIT_TAG <ref>)` | `find_package` / `pkg_config` / `fetchcontent` |
| `meson.build` | `project(…)`; `dependency('<n>')`; `subproject('<n>')` | `pkg_config` / `wrap` |
| `MODULE.bazel` | `module(name…)`; `bazel_dep(name, version)` | `bazel_dep` |
| `.gitmodules` | submodule `url` → slug | `submodule` |
| `*.pc(.in)` | `Requires:` / `Requires.private:` | `pkg_config` |

Notes: `FetchContent` and `.gitmodules` yield **repo-slug tokens** — self-resolving, highest-value edges (submodule pins also feed generation identity per the commit-gate law). `find_package`/`pkg_config` tokens resolve via aliases.

**Alias seed file** `workspace/ecosystem/assets/cpp_alias_seed.ron` (~100 hand-curated entries, repology-rules as reference only per RL-7): `find_package/ZLIB → github.com/madler/zlib`, `OpenSSL → github.com/openssl/openssl`, `Boost → github.com/boostorg/boost`, `CURL`, `PNG`, `Protobuf`, `Threads → system/pthread`, … Loaded by a startup reconcile into `package_aliases` (`curated`).

**Resolution pass** (pure, idempotent, rerunnable): for edges with `resolved_stem IS NULL`, look up `(cpp, kind→alias_kind map, lower(dep_name_canonical))`; on hit, fill `resolved_stem`. Confidence ordering on conflicting aliases: authoritative > curated > heuristic. Unresolved edges are *fine* — they surface as `Absent`-tier facts per the confidence law, never invented stems.

## 9. Search & ranking

- Register cpp `SearchNorms`; the tantivy `ecosystem` filter field works as-is (string `"cpp"`).
- Query normalization hook (ECOSYSTEM-PLAN §8.4): a bare-token query in cpp scope (`zlib`) consults `package_aliases` first and expands to the stem — this is where "users never type slugs" is honored.
- Facets: standard parity checklist (S1) + derived `presence` int facet (RL-12). Verify absent-downloads ranking skip fires (existing `log::debug` path) — one test.
- Cross-eco clustering: `RepoSlug` already clusters `packages.repo_url` across ecosystems — a Rust crate with `repository = github.com/curl/curl` and the cpp stem meet on the slug. Add one integration assertion (extends the X1 matrix).
- Fork-aware results (RL-16): vendored copies produce no package rows, so nothing to suppress; fork stems carry a `fork_of:<canonical>` facet and a visible "fork of X" label; for bare-token queries, lineage **targets rank above their forks** (tie-break after the presence prior, which already favors canonical stems since feeds alias to them); mirrors are collapsed under the canonical result by default.

## 10. Internal / enterprise mode

1. Config:

```toml
[discovery]
internal_hosts = ["git.corp.example", "*.internal.example"]   # GOPRIVATE analog
repo_lists = ["/etc/nudox/repos.txt"]                          # optional, one URL per line
```

2. **Egress guard** (the one hard rule): any stem whose authority matches `internal_hosts` never triggers public-feed lookups, OSV calls, Software-Heritage anything, or any outbound request beyond the matching host itself. Enforce centrally in the HTTP/transport layer (one predicate), not per-callsite. Test with a mock transport asserting zero public calls (gate G-10).
3. Discovery = `repo_lists` reconcile + the P6 add-by-URL route. Host-API enumeration (GitHub/GitLab org walkers) is a later, separate follower — explicitly out of v1.
4. Versioning: internal repos are frequently untagged → pseudo-versions carry the load; live-at-HEAD monorepos get exactly one moving pseudo-version per generation tick, which the commit-gated generation law already models.
5. Sharing: trusted remote upload (ID-18) unchanged; catalog rows for internal stems live on the enterprise catalog (dolt branch/remote per INDEX-PLAN dual-deployment), never the public one. Public DHT pinning remains rejected.

## 11. Phase R (stretch, gated): vendored-component & squashed-fork detection

Bounded Centris/OSV-determineversion hybrid, **file-level only** (no function hashing v1). This is the content-evidence backend for both halves of §3.6: Case B (vendored subtrees) and Case A step 2 (forks whose git history was squashed on import):

1. When sealing cpp ObjectPacks, also record per-source-member `sha1_git` (git blob hash — equals SWHID `cnt` / OmniBOR gitoid; enables OSV/SWH interop) in a `Meta` member. Additive to the pack format (new Meta key, no version bump).
2. Build `blob_index(sha1_git → (stem, version_set))` **restricted to distinctive files** — files appearing in ≤K stems (K≈3; Centris' core insight: ubiquitous files carry no signal). Expect ~10-30% of files indexed.
3. Detector (offline job): for a target pack, intersect member hashes against `blob_index`; candidate components ranked by matched-distinctive-file count; version estimated by majority vote over matched files' version sets (TIVER-lite; report a *range* when mixed — 67% of real vendored components are mixed-version).
4. **Route the hit through §3.6, don't emit blindly**: matches clustered under a vendor-ish subtree of the host → Case B `vendored` edge; matches spanning the *whole* repo root → Case A candidate, run the history test first (it may be a fork with rewritten history → `repo_lineage(fork_of, content_overlap)`), and only if the repo is neither ancestrally nor structurally a fork does it stay a whole-tree vendored edge.
5. Emit `edges { kind: "vendored", source: "detected", resolved_stem, requirement: <estimated range> }` at `heuristic` confidence; display respects the UNTRUSTED-vendored-trees rule (16-trust) — advisory surfacing only, never graph-edge authority.
6. **Go/no-go gate before building**: measure `blob_index` size on the first 500 indexed cpp stems; proceed only if < 2 GB and detector precision on a 20-repo hand-labeled set ≥ 80% — the labeled set must include at least 5 fork/vendored discrimination cases (known forks like libressl↔openssl-shaped pairs, and known vendorers like game engines bundling zlib).

## 12. Phase S-A (stretch): OSV advisories via GIT ranges

Follower (`feed = "osv"`): pull OSV's C/C++ entries (GCS bucket export), match `affected[].ranges[].repo` → slug → stem; translate commit-range events to affected versions via grit ancestry checks (`introduced ≤ rev < fixed`); write `listing_events { status: "advisory", reason: <osv id> }` per affected version. Internal hosts excluded by the §10 egress guard. The Trustfall security policy plane (04-ir-unification) reads these rows — no new query surface.

## 13. Explicit non-goals / dead list (do not build)

- Our own curated recipe repository (we mirror feeds; we don't author ports).
- CPE matching in any form. deps.dev dependency for C/C++ (it has none).
- Bulk Repology data import (RL-7). SE-0292-style registry protocol serving.
- Per-function TLSH/Centris-full matching, binary SCA (cve-bin-tool-style), compile_commands.json ingestion — all post-v1 candidates at best (compile_commands adds *paths*, not identities; revisit only alongside the clang oracle work, RL-15).
- Public transparency log operation (sumdb/Rekor); we record feed checksums (`registry_checksum`) as the honesty layer v1.
- A separate "internal packages" subsystem — internal mode is config over the same machinery (RL-13).

## 14. Build order (each phase = PR-sized, with a gate a junior can run)

| Phase | Deliverable | Size | Gate |
|---|---|---|---|
| **P0** | `Language::Cpp` + `cpp.rs` skeleton + `AnyVersion::Cpp` + dispatch arm + every exhaustive match fixed | 0.5–1d | `cargo check` workspace-green; `cargo test -p ecosystem`; rg gate: no `Language::` arms outside ecosystem crate |
| **P1** | Name + `CppVersion` grammars, ls-remote listing parser, pseudo-version cases (Go's 3 forms), StableRef-slash note (RL-10) | 1–2d | grammar unit tests + ordering property test; name_tests totalness |
| **P2** | `package_aliases` + `feed_watermarks` DDL, `UpsertAlias` CatalogOp, seed-file loader | 0.5–1d | migration idempotence test (INDEX-PLAN §13 style) |
| **P3** | Homebrew follower | 1d | fixture (recorded formula.json slice) → golden catalog rows |
| **P4** | vcpkg follower (grit tree walking, portfile scan) | 2–3d | vendored mini-registry fixture → golden diff incl. aliases + checksums |
| **P5** | conan-center follower (config.yml + conandata.yml first, conanfile static scan) | 2d | fixtures; unparseable-conanfile logged-not-fatal test |
| **P6** | Add-by-URL route + tag enumeration + pseudo-versions + ObjectPack acquisition + GitMonitor registration | 1–2d | integration test against `git init` fixture repo (tagged + untagged) |
| **P7** | Manifest extractors (7 parsers) + alias seed RON + resolution pass | 2–3d | per-parser fixtures; edge-count goldens; unresolved-edge-stays-Absent test |
| **P8** | SearchNorms + query alias expansion + `presence` facet + ranking-skip verify + X1 cross-eco row | 1d | search integration tests |
| **P9** | Internal mode: config, central egress guard, repo_lists reconcile | 1–2d | **G-10**: mock-transport zero-public-egress test |
| **P10** | Six `system/*` model packages on `overlay/models` + `Threads→system/pthread` aliases | 0.5d | catalog rows + alias resolution test |
| **P11** | Fork/mirror classification pass (§3.6 history test + candidate gen a/b) + `repo_lineage` writes + `fork_of` search facet + canonical-first ranking tie-break | 1–2d | fixture repo pairs built with `git init`: diverged clone → `fork_of` with correct merge-base; non-diverged clone → `mirror_of`; unrelated same-name repos → **no link** |
| **S-A** | OSV GIT-range advisories, incl. fork-ancestry evaluation (RL-17) | 2d | fixture advisory → listing_events rows; fork-with-cherry-picked-fix marked clean |
| **R** | Vendored + squashed-fork detection (design gate first — §11.6) | gated | precision ≥ 80% on labeled set incl. fork/vendored cases |

Dependency order: P0→P1→P2 strictly; P3/P4/P5 parallel after P2; P6 after P1+P2; P7 after P6 (needs packs) but parsers unit-testable immediately after P0; P8 after any follower; P9 anytime after P6; P10 after P2; P11 after P6 (needs local git DAGs; content-evidence upgrade arrives with R).

## 15. Risks

- **conanfile.py is Turing-complete** — static scan will miss dynamic values. Mitigated: conandata.yml carries the pins; misses degrade to Absent facts, never wrong facts.
- **Tag mutability / force-pushed tags** — always pin `source_rev` oid at enumeration; a moved tag ⇒ new version event, old pack stays content-addressed and immutable.
- **Slug-identity spoofing** (mirror repos claiming a name): identity *is* the URL, so a mirror is honestly a different package; feed `authoritative` aliases anchor the canonical stem. Accepted.
- **Boost-shaped monorepos** (RL-9): many aliases → one stem is coarse for per-port versioning (vcpkg versions `boost-asio` independently). v1 records feed versions against the stem; subpath packages are the tracked follow-up ticket.
- **ls-remote polling scale**: bounded by `git_watermarks` cadence + POLICY rps; enterprise forks of the vcpkg/conan registries supported via config to avoid GitHub rate exposure.
- **Alias collisions across feeds** (same name, different upstream): PK is per-`alias_kind`, so no table conflict; resolution-pass confidence ordering handles cross-kind disagreement; log disagreements for curation.
- **Fork-candidate name collisions** (hundreds of unrelated repos named `json`, `utils`): the merge-base test is the guard — no shared ancestry and sub-threshold overlap ⇒ no link, and unlinked is the safe default (§3.6). Never link on name alone.
- **Canonical relocation** (upstream moves hosts, e.g. a lib migrating to a foundation org): both slugs exist as stems; hand-curate mutual lineage (`mirror_of`/`fork_of` as appropriate) and repoint curated aliases; the old stem's packs stay valid (content-addressed).
- **Feed licensing**: vcpkg (MIT), conan-center-index (MIT), Homebrew formulae (BSD-2) — mirrorable; Repology excluded (RL-7).

## 16. Acceptance scenarios

1. **Search "zlib"** in cpp scope → alias expansion → stem `github.com/madler/zlib`; versions merged from git tags (P6) + vcpkg + brew pins; snippets served from ObjectPack ranges; `presence ≥ 2`.
2. **`POST /v1/packages/git`** with an internal URL on a `internal_hosts` host → versions enumerated (pseudo-version if untagged), pack sealed, IR queued — and the mock-transport test proves zero public egress.
3. **A CMake project indexed** → `find_package(ZLIB)` edge recorded literally, resolution pass binds `resolved_stem = zlib stem`, `Threads` binds to `system/pthread`; unresolved `find_package(ObscureLib)` remains visible as Absent-tier.
4. **A repo with submodules** → `submodule` edges to slug stems, submodule pins present in generation identity.
5. **Cross-eco**: rust crate `libz-sys` (repo_url → madler/zlib) and cpp stem cluster on `RepoSlug` in one query.
6. *(S-A)* A curl CVE with GIT ranges → `advisory` listing_events on exactly the affected enumerated versions.
7. **A maintained fork of a clib** (own slug, shared git history, diverged commits) → its own stem labeled `fork_of` the canonical stem with the correct fork-point rev; a bare `zlib` query ranks canonical first with the fork discoverable beneath it; *(S-A)* the fork's advisory status computed from its **own** ancestry — a cherry-picked fix marks it clean while upstream's unpatched tag stays flagged.
8. **A game engine vendoring zlib under `extern/zlib`** → no new package appears anywhere; a `vendored` edge engine→zlib with an estimated version range shows on the engine's page; zlib search results are unpolluted by the copy. *(Phase R)* The same detector, hitting a whole-repo match with no shared history, classifies a squashed-import fork as `fork_of (content_overlap)` rather than a vendored edge.

## 17. Reading order for the implementer

1. This file, §2 (decisions) + §14 (your phase).
2. `docs/ECOSYSTEM-PLAN.md` §3 (trait law) + one existing impl pair: `go.rs` (listing/pseudo precedent) and `nix.rs` (odd-feed precedent).
3. `docs/INDEX-PLAN.md` §6.2–6.4 (ObjectPack, acquisition, grit) + §8 (DDL you're extending) + §13 (migrations).
4. `docs/research/ecosystems/01..03` for the evidence behind any decision you're tempted to relitigate.
5. `docs/research/resolution/04-ir-unification.md` §16/§21/§24 only if you're touching models or references.

## 18. References

Internal: docs/ECOSYSTEM-PLAN.md; docs/INDEX-PLAN.md; `docs/research/resolution/04-ir-unification.md`; `docs/research/librarification/{05,06,08,16,17}*`; `docs/research/edge-tech/00-DECISIONS.md`; `docs/research/ecosystems/{01,02,03}*.md` (this session's external research, with full URLs).
External (load-bearing): Go module reference (go.dev/ref/mod: GOPROXY endpoints, pseudo-versions, GOPRIVATE); vcpkg registries + versioning docs; conan-center-index; formulae.brew.sh API; Bazel registry spec; Centris (ICSE'21), CNEPS (ICSE'24), V1SCAN (USENIX Sec'23), TIVER (ICSE'25); purl-spec; SWHID (ISO/IEC 18670); OSV C/C++ GIT-range support; ISO C++ 2023 / Modern C++ DevOps 2024 surveys; deps.dev FAQ (C/C++ absence).
