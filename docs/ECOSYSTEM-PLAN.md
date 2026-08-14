# ECOSYSTEM-PLAN — Unified per-ecosystem abstraction for the registry mirror + tantivy search

Status: DRAFT for execution (2026-07-18)
Scope: `workspace/heart`, `workspace/registry`, `workspace/server`, new crate `workspace/ecosystem`
Supersedes: nothing (extends the lib.rs-port search stack in place)
Companion audit: the four-dimension review of 2026-07-18 (schema/ingest, tokenizer,
query/ranking, mirror) — every finding from it is folded into a phase below and
cross-referenced by ID in §2.

---

## 0. Goal and design principles

Today every ecosystem-specific decision is a `match ecosystem { ... }` arm scattered
across at least seven files (`heart/package/mod.rs`, `registry/resolve.rs`,
`server/coordination/indexing.rs`, `registry/metadata/rich.rs`,
`registry/search/ranking.rs`, `server/search/query.rs`, `compiler/compile/csharp/nupkg.rs`).
Rust gets a real implementation in each arm; everything else gets a stub, a default,
or a silently-wrong fallthrough. The audit showed this is not seven small bugs — it is
one structural bug: **there is no single place where "what does ecosystem X need?" is
answered, so no arm can be verified complete.**

The fix is one sealed trait, `EcosystemSpec`, with associated types for the parts that
genuinely differ in *shape* (structured name, version grammar, upstream cursor,
manifest), const/config for the parts that differ only in *value* (archive format,
stopwords, ranking calibration), and a static dispatch table from the existing
`heart::Language` enum. Adding a new language then means writing one impl block, and
the compiler tells you every capability you have not provided.

Principles (read these before writing any code):

1. **Sealed and total.** `EcosystemSpec` is sealed; exactly one ZST per `Language`
   variant implements it. A `Language::spec()` table makes runtime dispatch total —
   no `_ => {}` arms anywhere in registry/server code after this plan lands.
2. **Pure spec vs. IO client.** The spec crate (`workspace/ecosystem`) is pure:
   string/version/URL/policy logic only, no reqwest, no tokio, fully unit-testable.
   The IO side (`registry/upstream/`) consumes the spec through a shared HTTP stack.
3. **Structured names are the common currency.** One `StructuredName` type
   (purl-shaped: authority / namespace segments / name / version-qualifier) is what
   every ecosystem parses into and renders from. Traits produce and consume it;
   downstream code (tantivy fields, PackageSelector, contains-bonus) never touches
   raw strings again.
4. **Structured search is the default path.** Queries carry an explicit
   `Option<Language>` scope plus structured constraints; the ecosystem filter is a
   tantivy `Must` term (index-pruned), not a post-hoc Rust filter. Free-text is the
   degenerate case of the structured query, not the other way round.
5. **No silent Rust defaults.** Wherever a value used to default to the crates.io
   behavior (stopwords, `cargo`/`rs` stripping, download thresholds, TarGz), the trait
   makes it a required item so each impl states its answer explicitly — even if the
   answer is "same as Rust".
6. **Mirror correctness is part of the spec.** Listing status (yank/unlist/deprecate),
   catalog cursors, and tombstoning are trait capabilities, not afterthoughts.

---

## 1. Crate layout after this plan

```
workspace/
  ecosystem/                  # NEW — pure spec crate (no IO deps)
    lib.rs                    # EcosystemSpec trait, sealed module, Language::spec() table
    name.rs                   # StructuredName + NameGrammar trait machinery
    version.rs                # VersionGrammar trait + per-ecosystem impls (moved from resolve.rs)
    manifest.rs               # ManifestFacts trait + RawManifest input type
    archive.rs                # ArchiveKind (moved/extended from registry::ingest::ArchiveFormat)
    search.rs                 # SearchNorms: stopwords, specificity separators, contains normalization
    policy.rs                 # UpstreamPolicy: rate limits, user-agent, retry budget
    rust.rs ts.rs python.rs go.rs java.rs csharp.rs nix.rs   # one impl module per Language
  registry/
    upstream/                 # NEW — IO side
      mod.rs                  # UpstreamClient (shared reqwest stack: UA, retry, rate limiter)
      catalog.rs              # CatalogFollower trait + cursor persistence
      nuget_catalog.rs        # NuGet V3 catalog follower (first follower)
      crates_catalog.rs       # crates.io index follower (second)
    ...
  heart/
    package/mod.rs            # PackageName becomes a thin wrapper over ecosystem::StructuredName
```

`workspace/ecosystem` depends on `heart` (for `Language`) and nothing heavier than
`smol_str`/`serde`. `registry` depends on `ecosystem`. `server` reaches ecosystem
behavior only through the trait, never through `match language`.

---

## 2. Defect ledger → phase map

Every audit finding, where it is fixed, and by what mechanism. (Severity from the
audit; IDs are stable — reference them in commit messages.)

| ID | Defect (file:line at audit time) | Sev | Phase | Mechanism |
|----|----------------------------------|-----|-------|-----------|
| M1 | NuGet `.nupkg` extracted as TarGz — all NuGet ingest fails (`indexing.rs:121`, no Zip variant) | CRIT | P3 | `EcosystemSpec::ARCHIVE = ArchiveKind::Zip` + zip extraction in ingest |
| M2 | No upstream discovery/catalog followers for any ecosystem | HIGH | P7 | `CatalogFollower` trait w/ associated `Cursor` |
| M3 | NuGet unlisted versions served as listed (`resolve.rs:338-345`) | HIGH | P3 | `listing_status` required trait method |
| M4 | No Delete outbox intent; no tombstone to tantivy/qdrant/terminus | HIGH | P7 | `SinkIntent::Delete` + `TextIndex::remove` wiring |
| M5 | Go + Java resolve fully stubbed (`resolve.rs:346`) | HIGH | P3 | goproxy + Maven impls of `UpstreamEndpoints` |
| M6 | No rate limit / retry / User-Agent on upstream HTTP (`resolve.rs:264`) | MED | P3 | `UpstreamPolicy` + shared client in `registry/upstream` |
| M7 | PyPI `yanked` / npm `deprecated` never read | MED | P3 | `listing_status` parses per-version status |
| S1 | `extract_facets` early-returns non-Rust: no description/keywords/readme/quality (`indexing.rs:705-717`) | HIGH | P4 | `ManifestFacts` associated type, required for every impl |
| S2 | `description` tantivy field declared+queried, never written (`search/tantivy.rs` absorb) | HIGH | P4 | `SearchFacets.description` + write in `absorb()` |
| S3 | Compiler identifiers discarded (`_identifiers`, `&[]` at `indexing.rs:159,199`) | MED | P4 | one-line rewire, covered by parity test |
| S4 | `downloads: 0` hardwired; ranking stages d/e dead (`search/mod.rs:164`) | HIGH | P6 | `DownloadSource` optional trait capability + `SearchFacets.downloads` |
| S5 | Facets-only update bumps `parse_status.updated_at`, sync polls `packages.updated_at` (`schema/queries.rs`) | MED | P4 | touch `packages.updated_at` in `set_facets` (do FIRST in P4) |
| Q1 | Double-escape: LiteralQuery grammar-escaping + `escape_regex` → `::`/`-`/qualified queries never match exact/regex tiers (`query.rs` both files) | CRIT | P5 | raw-text `StructuredQuery` end-to-end; delete grammar escaping on this path |
| Q2 | `PackageSelector.matches` first-segment root fails for Go module paths + dotted Java groups (`server/search/query.rs:116-123`) | HIGH | P5 | match on `StructuredName` roots, not string splits |
| Q3 | Package index passes raw user text to tantivy `QueryParser` unescaped, no field boosts, default `en` tokenizer on `name` | MED | P5 | hand-built BooleanQuery + IdentifierTokenizer on name field |
| Q4 | Ecosystem filter is post-hoc Rust code, not an indexed Must term | MED | P5 | ecosystem `Must` TermQuery (structured search core) |
| R1 | `contains_query_names` strips `cargo`/`rust`/`rs` for all ecosystems (`ranking.rs:316-331`) | MED | P6 | `SearchNorms::strip_conventions` per impl |
| R2 | `query_is_specific` checks only `-`/`_` (`ranking.rs:270`) | LOW | P6 | `SearchNorms::SPECIFICITY_SEPARATORS` |
| R3 | Stopwords ship `crate`/`crates`/`rust` globally; `KNOWN_CATEGORIES` = crates.io taxonomy (`rich.rs`) | MED | P4/P6 | `SearchNorms::stopwords()` + `category_taxonomy()` |
| R4 | Go domain tokens (`github`,`com`) will pollute IDF once Go lands | MED | P1 | `search_surface()` on `StructuredName` strips authority |
| T1 | No fuzzy/typo tolerance; contains tier is O(terms) RegexQuery | MED | P5 | opt-in `FuzzyTermQuery` Should-clause (single-term queries) |
| T2 | C# `I`-prefix single-char `i` token IDF noise | LOW | P5 | min-token-length 2 on subtoken query clause |
| T3 | npm `@scope` dropped at subtoken level; scoped disambiguation impossible | LOW | P1/P5 | `namespace` indexed as its own field |
| X1 | Ranking/search tests are Rust-only; single NuGet findability test | MED | P8 | cross-ecosystem fixture corpus + parity suite |

---

## 3. The core trait (write this first, Phase 0)

File: `workspace/ecosystem/lib.rs`. This is the complete skeleton — copy it, then
fill per-phase.

```rust
use heart::ecosystem::Language;

mod sealed {
    pub trait Sealed {}
}

/// One implementation per `Language` variant. Everything the registry, mirror,
/// and search pipeline need to know about an ecosystem, in one place.
///
/// Object safety: this trait is NOT object safe (associated types). Runtime
/// dispatch goes through [`DynSpec`], an object-safe erasure implemented
/// blanket-style for every `EcosystemSpec`. Application code should prefer
/// `language.spec()` (returns `&'static dyn DynSpec`); generic code that knows
/// the ecosystem at compile time (tests, per-ecosystem modules) uses the trait
/// directly.
pub trait EcosystemSpec: sealed::Sealed + 'static {
    const LANGUAGE: Language;

    // ---- Phase 1: names ----
    /// Parse a raw package identifier into the structured form. Returns None
    /// for names invalid in this ecosystem. MUST be total over its own
    /// registry's population (fixture-tested in Phase 8).
    fn parse_name(raw: &str) -> Option<name::StructuredName>;
    /// Render the canonical (identity) string form. Round-trip law:
    /// `parse_name(&render_canonical(&n)) == Some(n)`.
    fn render_canonical(name: &name::StructuredName) -> String;

    // ---- Phase 2: versions ----
    type Version: version::VersionGrammar;

    // ---- Phase 3: upstream ----
    /// Static archive + endpoint facts (URL templates, archive kind, policy).
    const ARCHIVE: archive::ArchiveKind;
    fn endpoints() -> upstream::UpstreamEndpoints;
    const POLICY: policy::UpstreamPolicy;
    /// Parse the registry's version-list/metadata response body into versions
    /// WITH listing status. Replaces the `match` in `resolve.rs`.
    fn parse_version_listing(body: &serde_json::Value)
        -> Vec<upstream::ListedVersion<Self::Version>>;

    // ---- Phase 4: manifests / facets ----
    type Manifest: manifest::ManifestFacts;
    /// Which file names inside the extracted archive are the manifest (+readme),
    /// in priority order. E.g. `["package.json"]`, `["pyproject.toml","setup.cfg"]`.
    fn manifest_candidates() -> &'static [manifest::ManifestCandidate];
    fn parse_manifest(candidate: &manifest::ManifestCandidate, bytes: &[u8])
        -> Option<Self::Manifest>;

    // ---- Phase 5/6: search norms ----
    fn search_norms() -> &'static search::SearchNorms;
}
```

### 3.1 Object-safe erasure (`DynSpec`)

Associated types (`Version`, `Manifest`) make `EcosystemSpec` non-object-safe, but
call sites are runtime-dispatched on `Language`. Bridge with an erased trait whose
methods speak in the type-erased currencies (`StructuredName`, `AnyVersion`,
`ExtractedFacts`):

```rust
pub trait DynSpec: Send + Sync {
    fn language(&self) -> Language;
    fn parse_name(&self, raw: &str) -> Option<name::StructuredName>;
    fn render_canonical(&self, name: &name::StructuredName) -> String;
    fn parse_version(&self, raw: &str) -> Option<version::AnyVersion>;
    fn compare_versions(&self, a: &str, b: &str) -> Option<std::cmp::Ordering>;
    fn archive(&self) -> archive::ArchiveKind;
    fn endpoints(&self) -> upstream::UpstreamEndpoints;
    fn policy(&self) -> policy::UpstreamPolicy;
    fn parse_version_listing(&self, body: &serde_json::Value)
        -> Vec<upstream::ListedVersion<version::AnyVersion>>;
    fn manifest_candidates(&self) -> &'static [manifest::ManifestCandidate];
    fn extract_facts(&self, candidate: &manifest::ManifestCandidate, bytes: &[u8])
        -> Option<manifest::ExtractedFacts>;
    fn search_norms(&self) -> &'static search::SearchNorms;
}

/// Blanket adapter: every `EcosystemSpec` is a `DynSpec` via a ZST wrapper.
struct Erased<E: EcosystemSpec>(core::marker::PhantomData<E>);
impl<E: EcosystemSpec> DynSpec for Erased<E> { /* forward each method */ }

/// The dispatch table. New Language variants fail to compile until listed here
/// (use an exhaustive match, NOT a lookup that can return None).
pub fn spec(language: Language) -> &'static dyn DynSpec {
    match language {
        Language::Rust => &Erased::<rust::Rust>(PhantomData),
        Language::Typescript => &Erased::<ts::TypeScript>(PhantomData),
        Language::Python => &Erased::<python::Python>(PhantomData),
        Language::Go => &Erased::<go::Go>(PhantomData),
        Language::Java => &Erased::<java::Java>(PhantomData),
        Language::CSharp => &Erased::<csharp::CSharp>(PhantomData),
        Language::Nix => &Erased::<nix::Nix>(PhantomData),
    }
}
```

Also add an extension trait so call sites read naturally:
`impl LanguageExt for Language { fn spec(self) -> &'static dyn DynSpec { spec(self) } }`.

`AnyVersion` is a small enum over the concrete version types (one variant per
ecosystem) implementing `Ord` where the underlying grammar does; it exists only so
`DynSpec` can talk about versions without generics. Do not leak it into `heart`.

### 3.2 Phase 0 execution steps

1. `cargo new --lib workspace/ecosystem` (match workspace lints/edition of sibling
   crates; add to root `Cargo.toml` members).
2. Create the module skeleton above with `todo!()` bodies in the seven impl modules.
3. Add `LanguageExt` and the `spec()` table.
4. Add a compile-time totality test: a unit test that calls
   `spec(l)` for every `Language` variant via `strum`-style iteration if available,
   else a hand-written array — the *match* in `spec()` is the real guard.
5. Wire `registry` and `server` `Cargo.toml` to depend on `ecosystem`.
   Nothing else changes yet; the crate compiles empty.

**Acceptance:** workspace builds; `ecosystem` crate has the trait, erasure, table,
and seven stub impls; totality test passes.

---

## 4. Phase 1 — Structured names

### 4.1 The type

File: `workspace/ecosystem/name.rs`. This is deliberately purl-shaped (see the
Package URL spec) so it maps onto every ecosystem we have and every one we might add:

```rust
/// A package identity decomposed into its ecosystem-meaningful parts.
/// Examples (authority / namespace / name):
///   Rust   `serde_json`                    → (None, [], "serde_json")
///   npm    `@types/node`                   → (None, ["types"], "node")
///   PyPI   `typing-extensions`             → (None, [], "typing-extensions")
///   Go     `github.com/gorilla/mux/v2`     → (Some("github.com"), ["gorilla"], "mux") + major=Some(2)
///   Maven  `org.springframework:spring-core` → (None, ["org","springframework"], "spring-core")
///   NuGet  `Newtonsoft.Json`               → (None, ["newtonsoft"], "json")  [dot-namespaced]
///   Nix    `NixOS/nixpkgs`                 → (None, ["nixos"], "nixpkgs")
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StructuredName {
    pub ecosystem: Language,
    /// Host/domain part, only where the grammar has one (Go module paths).
    pub authority: Option<SmolStr>,
    /// Ordered namespace segments (npm scope, Maven groupId parts, Go path dirs,
    /// NuGet dotted prefix). Already case-folded per the ecosystem's rules.
    pub namespace: Vec<SmolStr>,
    /// The final, most-specific segment. Case-folded per ecosystem rules.
    pub name: SmolStr,
    /// Go major-version suffix (`/v2`) and similar identity-relevant qualifiers.
    pub major: Option<u32>,
    /// The exact original input, preserved for display.
    pub original: SmolStr,
}

impl StructuredName {
    /// Identity string — feeds PackageId hashing. MUST equal what the old
    /// `canonicalize_*` functions produced (see §4.3 compat table) so no
    /// PackageId changes.
    pub fn canonical(&self) -> String { self.ecosystem.spec().render_canonical(self) }
    /// The short, IDF-clean form for the search index: name + namespace,
    /// WITHOUT authority (fixes R4 — `github`/`com` never become tokens).
    pub fn search_surface(&self) -> String;
    /// The roots a fully-qualified *symbol* path may start with, for
    /// PackageSelector matching (fixes Q2). Rust: ["serde_json"]; Go: the full
    /// module path prefix; Java: groupId-dotted prefix; npm: "@scope/name" and "name".
    pub fn symbol_roots(&self) -> Vec<String>;
}
```

### 4.2 Per-ecosystem `parse_name` rules (implement exactly)

| Ecosystem | Case fold | Separators | Namespace rule | Notes |
|---|---|---|---|---|
| Rust | lowercase, `-`≡`_` (keep existing canonical choice — check `canonicalize_crate` and DO NOT change its output) | none | always empty | |
| npm | lowercase | `@scope/` | `@scope` → `namespace=["scope"]` | reject uppercase (npm rule for new pkgs) but *parse* legacy uppercase, folding |
| PyPI | PEP 503: lowercase, `-`/`_`/`.` runs → `-` | none | empty | keep byte-identical to `canonicalize_pep503` |
| Go | preserve case in `original`; canonical = lowercase host + case-preserved path (match `canonicalize_go_module` output exactly) | `/` | authority = first segment containing `.`; namespace = middle path segments | strip+record trailing `/vN`, N≥2 |
| Maven | lowercase | `:` between group and artifact; `.` inside group | group parts → namespace | ACCEPT `groupId:artifactId` (new capability; old code rejected `:`) |
| NuGet | case-fold (NuGet IDs case-insensitive; match `canonicalize_csharp`) | `.` | leading dotted segments → namespace, last → name | `Newtonsoft.Json` → ns=["newtonsoft"], name="json" |
| Nix | match `canonicalize_nix_flake` | `/` | org → namespace | |

### 4.3 Execution steps

1. Implement `StructuredName` + the seven `parse_name`/`render_canonical` pairs.
2. **Compat gate (do before anything else consumes it):** port every existing test
   for `canonicalize_*` from `heart/package/mod.rs`, plus add a differential test
   that runs both old and new canonicalization over a fixture list of ≥50 real
   package names per ecosystem and asserts byte equality. PackageId is a hash of
   the canonical form — any drift silently orphans stored packages. The ONE
   allowed divergence: Maven now accepts `groupId:artifactId` (old code rejected
   it, so no stored IDs exist to collide with).
3. Rewrite `heart::PackageName::new` to delegate:
   `Language::spec().parse_name(raw)` → store the `StructuredName` inside
   `PackageName` (keep the public `canonical()`/`original()` API identical; add
   `structured(&self) -> &StructuredName`). Delete `canonicalize_*` once the
   differential test passes.
4. Fix Q2: rewrite `PackageSelector::matches` (`server/search/query.rs:116`) to
   use `symbol_roots()`:
   ```rust
   pub fn matches(&self, symbol: &heart::Symbol) -> bool {
       symbol.ecosystem == self.name.ecosystem()
           && self.name.structured().symbol_roots().iter().any(|root| {
               let fq = &symbol.name.fully_qualified;
               fq == root || fq.strip_prefix(root.as_str())
                   .is_some_and(|rest| rest.starts_with([':', '.', '/']))
           })
   }
   ```
   Add tests: Rust `axum::Router` vs selector `axum`; Go
   `github.com/gorilla/mux.Router` vs selector `github.com/gorilla/mux`; Java
   `org.springframework.context.ApplicationContext` vs selector
   `org.springframework:spring-context`; npm both `@scope/pkg` and bare forms.
5. Index the structure (feeds P5): in `registry/search/tantivy.rs` schema add
   `name_ns` (TEXT, identifier tokenizer — namespace segments joined by space)
   and switch `name` population from raw canonical/original to
   `search_surface()` + `original` (T3, R4). This is a schema change → bump the
   index directory version marker so `sync_from` rebuilds from watermark 0
   (mechanism already exists: orphan-watermark reset; force it by placing a
   `schema_version` file next to `watermark.json`, mismatch → wipe + resync).

**Acceptance:** differential canonicalization test green; PackageSelector tests for
all 7 ecosystems green; package index rebuilds and `spring boot`-style
namespace queries now match (add as test in P5 but the fields exist now).

---

## 5. Phase 2 — Version grammar behind the trait

Everything in `resolve.rs` that matches on ecosystem for version semantics moves to
`ecosystem::version`.

```rust
pub trait VersionGrammar: Sized + Ord + Clone + Send + Sync + 'static {
    fn parse(raw: &str) -> Option<Self>;
    fn is_prerelease(&self) -> bool;
    /// Whether `candidate` satisfies the ecosystem-native range `spec`
    /// (semver ranges, PEP 440 specifiers, NuGet intervals, Maven ranges,
    /// Go pseudo-version/module rules).
    fn range_matches(spec: &str, candidate: &Self) -> bool;
    fn spec_is_valid(spec: &str) -> bool;
}
```

Steps:

1. Move the existing implementations verbatim: semver (Rust/TS/Nix), the PEP 440
   handling (Python), `NuGetVersion` (from `resolve.rs:365`+) — keep their tests.
2. Write the two missing grammars:
   - **Go**: semver with mandatory `v` prefix; pseudo-versions
     (`v0.0.0-20200828..-hash`) parse and order by timestamp; `+incompatible`
     suffix tolerated. Ranges: Go has no range syntax in our path — `range_matches`
     = exact or MVS-latest; document this in the impl.
   - **Maven**: the Maven ComparableVersion ordering (qualifier ordering
     `alpha < beta < milestone < rc < '' < sp`) and bracket ranges `[1.0,2.0)`.
     Port the ordering table from Maven's documented algorithm; property-test
     transitivity.
3. Replace `RangeConstraint::matches` and `select()` ecosystem matches in
   `resolve.rs` with `spec.compare_versions` / erased calls. Delete
   `Language::Go | Language::Java => false` arms (M5 partially — the grammar half).

**Acceptance:** all existing version tests pass from their new home; new Go/Maven
grammar tests (≥15 ordering cases each, including Maven qualifier table and Go
pseudo-versions); `resolve.rs` contains zero `match ecosystem` on version logic.

---

## 6. Phase 3 — Upstream endpoints, archive formats, listing status, HTTP hygiene

### 6.1 Spec side (`ecosystem/upstream.rs`, `archive.rs`, `policy.rs`)

```rust
pub struct UpstreamEndpoints {
    /// Template for the version-list/metadata request. `{name}`, `{name_lower}`
    /// placeholders; the base URL comes from server config (existing pattern).
    pub listing: &'static str,
    /// Template for the archive download. `{name}`, `{version}` placeholders.
    pub archive: &'static str,
}

pub enum ArchiveKind { TarGz, TarZst, Tar, Zip }   // extends registry::ingest::ArchiveFormat

pub struct ListedVersion<V> {
    pub version: V,
    pub status: ListingStatus,
}
pub enum ListingStatus {
    Listed,
    /// Yanked (crates.io/PyPI), unlisted (NuGet), deprecated (npm).
    /// Withdrawn versions resolve only under exact pins and never enter search.
    Withdrawn { reason: Option<SmolStr> },
}

pub struct UpstreamPolicy {
    pub max_requests_per_second: f32,   // crates.io: 1.0; npm: 10.0; pypi: 10.0; nuget: 20.0; goproxy: 20.0; maven: 10.0
    pub retry_budget: u8,               // transient (5xx/429) retries, expo backoff
    pub respect_retry_after: bool,      // true everywhere
}
```

Per-impl values (fill exactly):

| | listing endpoint | archive | ArchiveKind | listing-status source |
|---|---|---|---|---|
| Rust | `/api/v1/crates/{name}` (existing) | static.crates.io `.crate` (existing) | TarGz | `versions[].yanked` (existing filter → becomes `Withdrawn`) |
| npm | `/{name}` (existing) | `/{name}/-/{leaf}-{version}.tgz` (existing) | TarGz | `versions[*].deprecated` present → Withdrawn (M7) |
| PyPI | `/pypi/{name}/json` (existing) | sdist URL from JSON (existing two-step — keep the bespoke acquisition, but route status through this parse) | TarGz | per-file `yanked` flag: a release is Withdrawn iff ALL its files are yanked (M7) |
| NuGet | flat-container `index.json` for versions **plus** registration page for `listed` (M3) | flat-container `.nupkg` (existing URL logic in `nupkg.rs`) | **Zip** (M1) | registration `listed == false` → Withdrawn |
| Go | `https://proxy.golang.org/{module}/@v/list` (+ `@latest`); module path lowercased with `!` capital-escaping per goproxy protocol — implement `escape_module_path` in the Go impl | `/{module}/@v/{version}.zip` | **Zip** | goproxy has no unlist; retractions come from `go.mod retract` — parse `@v/{version}.mod` retract directives (best-effort; document) |
| Maven | `https://repo1.maven.org/maven2/{group/path}/{artifact}/maven-metadata.xml` (XML! see step 4) | `.../{artifact}-{version}-sources.jar` (sources jar; fall back to binary jar) | **Zip** (jars are zips) | Central has no unlist; treat 404 of previously-seen version as Withdrawn during catalog sweeps |
| Nix | existing FlakeHub releases route | existing tar.gz | TarGz | none (document) |

### 6.2 IO side (`registry/upstream/mod.rs`)

One shared `UpstreamClient` replacing every bare `reqwest::get` (M6):

```rust
pub struct UpstreamClient {
    http: reqwest::Client,          // built once: UA "nudox-registry-mirror/<ver> (+contact-url)", 30s timeout, pooling
    limiters: HashMap<Language, RateLimiter>,   // token bucket per POLICY
}
impl UpstreamClient {
    /// GET with per-ecosystem rate limiting, retry budget on 429/5xx/transport
    /// errors, exponential backoff (250ms * 2^n, jittered), Retry-After honored.
    pub async fn get(&self, language: Language, url: &str) -> Result<Bytes, UpstreamError>;
}
```

Steps:

1. Implement `UpstreamClient`; a plain token-bucket (Instant-based) is fine — no
   new deps needed; wrap `tokio::time::sleep` for backoff.
2. Replace `reqwest::get` in `resolve.rs:264` and the acquisition client usage in
   `indexing.rs` with `UpstreamClient` (inject via the existing `Indexer`
   construction path; one instance per server).
3. **M1 fix:** add `Zip` to `registry::ingest::ArchiveFormat` (or re-export
   `ecosystem::ArchiveKind` and migrate). Add the `zip` crate (pick `zip` 2.x,
   default features minus encryption) to the workspace. Implement zip extraction in
   `ingest_archive` with the SAME defenses as tar: `ExtractionLimits` byte/file
   ceilings enforced on *decompressed* size (zip bombs — read entry-by-entry with a
   limited reader, never trust the header size), `EntryAllowlist::SAFE` path checks,
   reject absolute paths / `..` / non-UTF-8. Then change `indexing.rs:121` from the
   hardcoded `ArchiveFormat::TarGz` to `record.package.…language.spec().archive()`.
4. **M3 fix:** NuGet `parse_version_listing` needs *two* requests (flat-container
   list + registration `listed`). Extend the resolve path: `published_versions`
   becomes `spec.fetch_versions(&client, base, name)` where the default impl is
   one GET + `parse_version_listing`, and NuGet overrides with the two-request
   join. (Add `fetch_versions` to `DynSpec` with a default; it's the one place
   the pure/IO boundary needs a seam — keep the *parsing* pure and tested, the
   fetch orchestration thin.)
5. **M5 fix:** implement Go + Maven listing/archive per the table. Maven metadata
   is XML: add `quick-xml` (workspace dep) and parse `<versions><version>` — keep
   it in the Maven impl module. Add `RegistryOrigin::{GoProxy, MavenCentral}`
   variants wherever `RegistryOrigin` is matched (compiler will surface every
   site).
6. Thread `ListingStatus` through `resolve::select`: Withdrawn versions are
   excluded from range resolution unless the request is an exact-version pin;
   NEVER enqueued for search indexing. Store the status on the package record
   (new column/field `listing: ListingStatus` on the version row — follow the
   existing schema-migration pattern in `registry/schema/`).

**Acceptance:** a NuGet package ingests end-to-end in a test (fixture `.nupkg`
zip); unlisted NuGet fixture is resolvable by exact pin but absent from search;
Go + Maven fixtures list versions and download archives (wiremock-style local
HTTP fixtures — follow the existing offline-test patterns; do NOT hit real
registries in tests); every outbound request in the codebase goes through
`UpstreamClient` (grep gate: `reqwest::get` count == 0 outside `upstream/`).

---

## 7. Phase 4 — Manifest facts and facet parity

**Step 0 (S5, do first):** in `registry/schema/queries.rs::set_facets`, also bump
`packages.updated_at` (same transaction). Otherwise everything below silently
never reaches tantivy for already-stored packages. Add a regression test:
`set_facets` → `changed_since(pre)` returns the package.

### 7.1 The trait currency

```rust
/// What a manifest parse yields, ecosystem-erased. Field-for-field mirror of
/// the Rust-only ExtractionInput population in indexing.rs:719-759.
pub struct ExtractedFacts {
    pub description: Option<String>,
    pub keywords: Vec<String>,          // npm keywords / PyPI keywords / NuGet tags / Cargo keywords
    pub categories: Vec<String>,        // Cargo categories / PyPI trove classifiers (mapped, §7.3)
    pub readme_hint: Option<String>,    // manifest-declared readme path, if any
    pub repository: bool,
    pub documentation: bool,
    pub license: bool,
    pub dependencies: Vec<String>,      // for the dep: invisible-keyword feature
}

pub struct ManifestCandidate { pub path_suffix: &'static str }  // matched case-insensitively against archive entries
```

### 7.2 Per-ecosystem parsers (each ~50-100 lines + tests; all pure)

| Impl | Files | Extract |
|---|---|---|
| Rust | `Cargo.toml` | MOVE the existing `parse_cargo_toml` here unchanged; add `[dependencies]` keys (S ledger: dependencies were always empty) |
| npm | `package.json` | `description`, `keywords`, `repository`, `homepage`→documentation, `license`, `dependencies` keys |
| PyPI | `pyproject.toml`, `PKG-INFO`, `setup.cfg` (priority order) | `project.description`, `keywords`, `classifiers` (→ categories via §7.3), `project.urls`, `license`, `dependencies` |
| NuGet | `*.nuspec` (XML, quick-xml) | `description`, `tags` (space-split), `projectUrl`, `licenseUrl`/`license`, `repository`, `dependencies` group ids |
| Go | `go.mod` | module path confirmation + `require` paths as dependencies. No description field exists — README carries the weight (see readme step) |
| Java | `pom.xml` | `description`, `url`→documentation, `licenses`, `scm`→repository, `dependencies` `groupId:artifactId` |
| Nix | `flake.nix` | the `description = "…";` string — regex/simple-scan, do NOT evaluate Nix |

README: generalize the existing Rust readme lookup in `indexing.rs` — search the
extracted snapshot for `README.md`/`README.rst`/`README` (case-insensitive, root
first, then `readme_hint`) for EVERY ecosystem. The existing
`readme_relevant_text`/`section_weight` machinery in `rich.rs` is
markdown-generic — reuse as-is.

### 7.3 Category taxonomy (R3, half)

Add `SearchNorms::map_category(raw: &str) -> Option<&'static str>` translating
native taxonomies into ONE shared internal taxonomy (start from the existing
`KNOWN_CATEGORIES` list as the internal one). PyPI: map trove classifiers
(`Topic :: Internet :: WWW/HTTP` → `web-programming`, etc. — table of the ~40
common ones, rest `None`). npm/NuGet have no taxonomy — `None` (their
keywords/tags already flow as keywords).

### 7.4 Rewire `extract_facets` (S1, S2, S3)

Rewrite `server/coordination/indexing.rs::extract_facets` to be
ecosystem-generic — the Rust special-case DIES:

1. Find manifest: iterate `spec.manifest_candidates()` against snapshot entries;
   first parse win → `ExtractedFacts` (default empty on none — never skip the rest).
2. Find README (all ecosystems, §7.2).
3. Build `ExtractionInput` with ALL fields populated from facts + readme + loc
   (count newlines in the source files already in hand — the audit's suggested
   cheap loc; cap at u32) + **identifiers from the compile phase** (S3: rename
   `_identifiers`, pass `&identifiers` — delete the `&[]`).
4. `SearchFacets` gains `description: Option<SmolStr>` and (P6) `downloads: Option<u64>`;
   `from_rich` carries description through; `PackageIndex::absorb` writes
   `fields.description` (S2). Bump the tantivy `schema_version` marker again (or
   fold into P1's bump if executing together).
5. Stopwords: replace `is_stopword` with `spec.search_norms().is_stopword(w)` —
   shared English list + per-ecosystem extras (`crate`/`crates`/`rust` move to the
   Rust impl; add `gem`-equivalents: npm `node`,`js`; PyPI `python`,`py`; NuGet
   `dotnet`,`net`; Go `go`,`golang`; Nix `nix`,`flake`) (R3).

**Acceptance:** fixture package per ecosystem (real manifest bytes vendored under
`registry/tests/fixtures/manifests/`) produces non-empty description+keywords and
quality ≥ 0.4 (above the ranking kink) for a well-formed manifest; the
`quality ≈ 0` collapse test — npm fixture vs Rust fixture with same BM25 — shows
fused scores within 2× (was ~9×); `set_facets` freshness regression test green.

---

## 8. Phase 5 — Structured search (the query path)

### 8.1 The query type

New, in `registry/search/mod.rs` (package) and threaded to
`runtime/text/query.rs` (symbols):

```rust
/// What every search API surface parses user input into. Raw text stays RAW —
/// no tantivy-grammar escaping anywhere on this path (Q1: that escaping fed
/// hand-built queries that never see the tantivy grammar, corrupting them).
pub struct StructuredQuery {
    /// The dominant case: scoped to one language. None = cross-ecosystem.
    pub ecosystem: Option<Language>,
    /// Free terms, verbatim from the user (post length/control-char validation
    /// — KEEP LiteralQuery's length + control-char checks, DELETE its escaping).
    pub terms: String,
    /// Structured constraints parsed from `key:value` tokens in the input
    /// (`lang:go`, `scope:types`, `group:org.springframework`, `kind:function`)
    /// or set by API parameters. Unknown keys → treated as free text, never an error.
    pub namespace: Option<String>,
    pub kinds: Vec<SymbolKind>,          // symbols path only
    pub package: Option<PackageSelector>, // symbols path only
}
```

Parsing rules (`StructuredQuery::parse(raw, api_scope)`), exhaustive:
- Split on whitespace outside of nothing (no quoting v1 — document).
- A token matching `^(lang|ecosystem):(\w+)$` sets `ecosystem` via
  `Language::from_token` (case-insensitive; aliases: `ts`→Typescript, `js`→Typescript,
  `c#`/`cs`→CSharp, `golang`→Go). Unknown language value → keep as free text.
- `scope:X` / `group:X` / `ns:X` → `namespace`. `kind:X` → kinds (symbols).
- Everything else rejoins (single spaces) into `terms`.
- API-level scope (route param / request field) wins over inline tokens; inline
  tokens win over nothing. Precedence test required.

### 8.2 Fix Q1 (double-escape) — exact surgery

1. `server/search/query.rs`: `LiteralQuery::parse` keeps `MAXIMUM_LENGTH` and
   control-character rejection; DELETE the `TANTIVY_OPERATORS` escaping loop and
   the constant. `LiteralQuery` now holds the trimmed raw string. (Nothing on
   either search path uses tantivy's `QueryParser` after 8.3, so grammar escaping
   has no remaining consumer.)
2. `runtime/text/query.rs::build_query` already regex-escapes correctly for raw
   input — with escaping removed upstream, `axum::Router`, `react-query`,
   `Option<T>` all match. Add regression tests for exactly these three through
   the full server path (`symbols.rs`).

### 8.3 Package index: hand-built query, indexed ecosystem filter (Q3, Q4)

Replace `PackageIndex::query`'s `QueryParser` with a hand-built tree mirroring the
symbol path's shape:

```text
Must(
  BooleanQuery Should-tiers over terms:
    exact  TermQuery(name_exact, full_terms_lowercased)         boost 4.0   # new STRING field storing search_surface + canonical verbatim
    tokens Must-conjunction of subtokens on name_tokens          boost 2.0   # name field re-tokenized with IdentifierTokenizer (P1 did the schema)
    ns     Must-conjunction of subtokens on name_ns              boost 1.5
    desc   Must-conjunction on description                       boost 1.0
    kw     Must-conjunction on keywords                          boost 1.2
    fuzzy  FuzzyTermQuery(name_tokens, distance 1)               boost 0.3   # ONLY when terms is a single token of length 4..=12 (T1)
)
AND Must(TermQuery(ecosystem)) when query.ecosystem.is_some()                # Q4
AND Must(subtoken-conjunction on name_ns) when query.namespace.is_some()
```

Boost values: tantivy `BoostQuery` wrappers; start with the numbers above,
mark them as a single `struct PackageQueryBoosts` const so tuning is one place.
Keep the post-hoc ecosystem check in `collect_ranked_hits` as a debug_assert,
not a filter (belt-and-braces during rollout, delete after one release).

Subtoken hygiene (T2): in BOTH query builders, drop subtokens of length < 2
from Must-conjunctions (if that empties the conjunction, omit the clause —
the exact/fuzzy tiers still apply). Index side unchanged (single-char tokens
still indexed; they're only noisy as query conjuncts).

### 8.4 Per-ecosystem query normalization hook

Before building clauses, when `ecosystem` is known:
`spec.search_norms().normalize_query(&mut terms)` — npm strips a leading `@` from
`@scope/name` into the namespace constraint; Go strips a leading authority
(`github.com/...` → namespace+name); Maven splits `group:artifact` into
namespace+terms. Each is ~5 lines in its impl; default is identity.

**Acceptance (tests, all through the public server search API):**
`lang:go mux` returns the Go fixture and zero Rust results; `@types/node` (npm
scope) matches exactly; `spring boot` matches the `org.springframework.boot`
fixture via `name_ns`; `axum::Router`, `react-query`, `Option<T>` regression
trio; `getUseById` (typo) finds `getUserById` via the fuzzy tier; ecosystem
Must-filter verified by asserting tantivy result counts (not post-filter counts).

---

## 9. Phase 6 — Ranking parity

1. **Ecosystem-aware Candidate:** `ranking::Candidate<T>` gains
   `ecosystem: Language` (populate in `search/mod.rs::collect_ranked_hits`).
2. **R1:** `contains_query_names` → `spec.search_norms().strip_conventions(name)`:
   Rust keeps `cargo`/`rust` prefix + `rs` suffix stripping (verbatim move); npm
   strips `@scope/`; Python strips `py`/`python` prefix and `-python`/`-py`
   suffix; Go strips authority; others identity. Regression test: npm `axios`
   with query `axio` must NOT get the contains bonus anymore.
3. **R2:** `query_is_specific` separator set becomes
   `spec.search_norms().SPECIFICITY_SEPARATORS` — default `['-','_','/','.','@',':']`
   (same for all current impls; the trait item exists so future ecosystems can
   differ, and so the value is documented next to its peers).
4. **S4 (downloads):** add to the trait an optional capability:
   ```rust
   fn download_source() -> Option<upstream::DownloadEndpoint>;
   // crates.io: crate metadata already fetched (recent_downloads);
   // npm: api.npmjs.org/downloads/point/last-month/{name};
   // pypi: pypistats /packages/{name}/recent (or None v1 — acceptable);
   // nuget: search API totalDownloads; go/maven/nix: None.
   ```
   Fetch at facet-extraction time through `UpstreamClient` (rate-limited), store
   in `SearchFacets.downloads: Option<u64>`, thread to `Candidate.downloads`.
   **Calibration:** the lib.rs thresholds (`bubble_downloads_min: 200`,
   `representative_downloads_floor: 100_000`) are per-ecosystem scale-dependent —
   move them into `SearchNorms` as `downloads_scale: f32` multipliers
   (crates.io 1.0; npm 0.05 [npm volumes ~20× crates]; NuGet 0.2; PyPI 0.1;
   None-sourced ecosystems: thresholds never fire, exactly like today — document
   that this is graceful, not silent: `log::debug!` once per process when a
   downloads-stage is skipped for lack of data).
5. **Cross-ecosystem fairness guard:** in `fuse_scores`, when the query is
   UNSCOPED (cross-ecosystem), quality and downloads signals compare packages
   with different data richness. Add a rule: a candidate whose facets carry
   `downloads: None` participates in stages d/e as if it had exactly the floor —
   never below a candidate that merely *has* data. Test: NuGet fixture without
   downloads vs Rust fixture with 1M downloads and equal BM25 — NuGet result must
   remain within the first page.

**Acceptance:** ranking unit tests parameterized over ecosystems; the R1/R2
regressions above; a scoped `lang:python requests`-style query ranks the Python
fixture first even with Rust lookalikes present.

---

## 10. Phase 7 — Mirror: catalog followers + tombstones

### 10.1 Delete/tombstone plumbing first (M4)

1. Add `Delete` to the outbox intent enum (wherever `SinkKind`
   materialization intents are defined in `server/poll.rs` / `registry/coordination.rs`),
   carrying `PackageId` + generation.
2. `materialize_text` handles Delete via the existing `TextIndex::remove`;
   `PackageIndex` gets `remove(package_id)` (delete-term, same as the
   delete-before-add in `absorb`); qdrant/terminus sinks get their delete calls
   (both stores support point/node deletion — follow each sink's existing
   materialize function shape).
3. When resolve/refresh observes `ListingStatus::Withdrawn` for a version that is
   stored+indexed: write the status, emit Delete intents for its search documents
   (package doc if ALL versions withdrawn; symbol docs for that version). Blob/CAS
   data is retained (mirror keeps history; only search visibility dies) — state
   this in a comment where the decision is made.

### 10.2 CatalogFollower

```rust
/// A resumable reader of "what changed upstream since my cursor".
pub trait CatalogFollower: Send + Sync {
    fn language(&self) -> Language;
    /// Fetch the next batch of upstream events at-or-after the persisted cursor.
    /// MUST be resumable: crashing between poll() and commit() re-yields events
    /// (downstream registration is idempotent — same guarantee the internal
    /// outbox already relies on).
    async fn poll(&self, client: &UpstreamClient, cursor: &CatalogCursor)
        -> Result<CatalogBatch, UpstreamError>;
}
pub struct CatalogCursor(pub serde_json::Value);   // follower-defined shape, opaque to the driver
pub struct CatalogBatch {
    pub events: Vec<CatalogEvent>,
    pub next: CatalogCursor,
    pub exhausted: bool,   // caught up → driver sleeps poll_interval
}
pub enum CatalogEvent {
    Published { name: String, version: String },
    Withdrawn { name: String, version: String },
}
```

Driver: a new entry in `server/poll.rs` (`catalog_follower_worker`), one task per
configured follower, gated behind server config (`[mirror] follow = ["csharp", "rust"]`
— default EMPTY: demand-pull remains the default; full-mirror is opt-in per
ecosystem). Cursor persisted via the existing atomic write-rename watermark
pattern (`catalog-cursor-{lang}.json` next to the other watermarks) — commit the
cursor only AFTER all events in the batch are registered. Events call the same
package-registration entry point the API uses (idempotent by PackageId+version).
Backpressure: the follower pauses (does not advance) while the indexing queue
depth exceeds a config ceiling — read the existing queue-depth gauge.

Followers, in order:
1. **NuGet V3 catalog** (`nuget_catalog.rs`): catalog index → pages →
   leaves; cursor = `commitTimeStamp`; leaf `@type` nuget:PackageDelete /
   listed=false → Withdrawn. This is the reference implementation — document
   heavily; page fetches go through `UpstreamClient` (per-policy limited).
2. **crates.io** (`crates_catalog.rs`): poll the index dump/db-dump or the
   `/api/v1/summary` new-crates + the sparse index changed files; simplest
   correct v1: RSS of new versions + periodic re-check of yanked on packages we
   hold. Cursor = last-seen publish timestamp.
3. npm (`_changes` since-seq), PyPI (RSS + XML-RPC changelog serial), goproxy
   (index.golang.org/index?since=), Maven (incremental index) — each ~1 page of
   code once the driver exists; ship as capacity allows, each behind its config
   flag.

**Acceptance:** wiremock catalog fixtures: follower ingests a Published event
end-to-end into tantivy; a Withdrawn event removes the search doc (verify via
search API) while CAS data remains; kill-between-poll-and-commit test replays the
batch without duplicate packages; queue-ceiling pause test.

---

## 11. Phase 8 — Cross-ecosystem test matrix (X1)

Build once, in `registry/tests/common/`: `fixture_package(lang) -> StoredPackage`
with REAL vendored manifests + tiny archives for all 7 ecosystems (the P6/P7
fixtures consolidate here).

Required suites:
1. **Name grammar property tests:** per ecosystem: parse→render→parse
   round-trip; canonical stability vs the ≥50-name fixture lists (from P1).
2. **Version grammar:** ordering transitivity property test per grammar;
   the Maven qualifier table; Go pseudo-versions.
3. **Search parity suite** (the headline): for EACH ecosystem, the same six
   scenarios — exact name, namespace term (`spring boot`, `@types/node`,
   `gorilla mux`…), description term, typo (fuzzy), scoped `lang:X` filter
   excludes others, qualified-path symbol query (`::`/`.`/`/`). One
   parameterized test, seven ecosystems, six scenarios = 42 cases; failures name
   the (ecosystem, scenario) pair.
4. **Ranking fairness suite:** the P6 acceptance cases (cross-ecosystem BM25-tie,
   downloads-None guard, contains-bonus regressions).
5. **Mirror suite:** the P3/P7 acceptance cases (zip bomb rejection for the new
   zip path included — reuse the tar bomb test's shape).
6. **Grep gates** (cheap, in a test or CI script): zero `match .*ecosystem`
   /`match .*Language::` arms in `registry/search/`, `registry/resolve.rs`,
   `server/coordination/indexing.rs` outside the `ecosystem` crate (allowlist:
   the `spec()` table itself); zero `reqwest::get` outside `registry/upstream/`.

---

## 12. Execution order, dependencies, sizing

```
P0 trait skeleton            (small)   ──┐
P1 structured names          (medium)  ──┼─→ P5 structured search (large)
P2 version grammars          (medium)  ──┤
P3 upstream/archive/status   (large)   ──┼─→ P7 catalog followers (large)
P4 manifests/facets  [S5 first] (large)──┼─→ P6 ranking parity (medium)
                                         └─→ P8 test matrix (medium, grows with each phase)
```

- P0→P1→P2 are strictly sequential (each builds on the previous).
- P3 and P4 are independent of each other after P1; parallelizable.
- P5 needs P1 (fields) but not P3/P4; the Q1 escaping fix inside P5 is
  independent and may be cherry-picked FIRST as a hotfix (it is two deletions +
  three tests).
- P6 needs P4 (facets) + P5 (Candidate wiring). P7 needs P3.
- Ship the M1 zip fix (inside P3) early too — it unblocks all NuGet work.

Suggested landing sequence for maximal early value:
**Q1 hotfix → P0 → P1 → M1+M3 (NuGet un-break) → P4 (facet parity) → P5
(structured search) → P2 → P3 rest → P6 → P7 → P8 continuously.**

Definition of done for the whole plan: the §11.3 parity suite is green for all
seven ecosystems, the grep gates pass, and adding a hypothetical eighth
`Language` variant produces compile errors in exactly one crate (`ecosystem`)
until its impl is written.
