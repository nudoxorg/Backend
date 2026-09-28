# W-Browse plan: package browsing, from prototype to product

> Copied verbatim from the lead's scratch folder
> (`$S/wave4/browse-impl/PLAN.md`) into `docs/architecture/briefs/package-browsing/` by the
> W-Browse finisher, 2026-09-27. Bare filenames this plan cites as evidence
> (`meta-host.json`, `fixture-meta.json`, `stills/`, `cli-data`, `cli-fixture-tree.txt`,
> `cli-project-tree.txt`, `head-Cargo.lock`, `head-check.log`, `harness-state/`) are scratch
> evidence from the planning pass, relative to that same scratch directory, and are **not
> preserved in this repository**. Status as of HEAD `92f974b370660b8273147886893703d3305e99c4`:
> S1 ("Your tree") described below is fully implemented and committed (see
> `package-browsing.md` §2 and `browse-impl-checkpoint-1.md`); S2 onward have not been
> started. Where the lead's later rulings (`browse-rulings.md`, or the plan-approval message
> quoted in `package-browsing.md` §4) narrow or override a proposal below, the rulings win.

Status: PLAN for review. No code is written. Every file:line below was read in
this pass, either by me or by a read-only Sonnet/Haiku agent. Where only an
agent read it, I spot-checked the lines this plan depends on (marked "agent"
where I did not). Numbers marked **measured** come from commands I ran on this
repo on 2026-09-27 (outputs in this directory: `meta-host.json`).

---

## 0. What changes the plan (read this first)

1. **Cargo should be the authority for "your tree", not a lockfile parser of ours.**
   - Today the only lockfile reader is a line scan that keeps `name@version` and drops every edge: `crates/local-service/src/builtin/product_state.rs:1411-1438` (`parse_lockfile`). Member manifests never read `[target.*]` tables or `package =` renames (`crates/local-service/src/builtin/local_manifest.rs:245-266`, agent).
   - `cargo metadata --offline --locked --format-version 1 --filter-platform aarch64-apple-darwin` on this repo **measured**: 0.9 s, 4.5 MB, 928 packages, 884 external. It carries every package's license, description, categories, keywords and `rust_version` (toml: `"categories": ["encoding","parser-implementations","parsing","config"]`, `"rust_version": "1.66"`), the resolved graph with `dep_kinds[].{kind,target}`, and bin/lib targets.
   - The unfiltered run fails offline (`failed to download antithesis_sdk v0.3.0`), so "other platforms" = Cargo.lock's external set minus the host set: **1,189 − 884 = 309 measured** (plus 4 host packages that are `[patch]`ed to `vendor/`, `Cargo.toml:107-113`).
   - House precedent for running Cargo from the backend: `frontends/rust/src/legacy/purl.rs:229-260` (`cargo metadata --offline --no-deps`, byte-bounded, cancellable, through `RustToolchain`).
   - With the rule "shortest path; ties go to members with a bin target, then by name", BFS over that graph reproduces the lead's strings exactly (**measured**): `desktop → toml 0.8.23`, `frontend-rust → ra_ap_project_model 0.0.341 → toml 1.1.5`, `desktop → gpui_ce_components 0.2.0 → gpui_ce_components_base 0.2.0 → syntect 5.3.0 → bincode 1.3.3`.
   - The page's numbers become "Your 44 packages lean on 75 others directly, and 884 in all", with "and 309 more for other platforms" on hover (ruling 3). The prototype's 79 and 1,189 were all-platform counts.
2. **Our advisory parsers misread the real RustSec data, so "bincode 1.3.3 is unmaintained" cannot come out of them today.** I fetched the real RUSTSEC-2025-0141 in both published forms (`gh api`, read-only):
   - The advisory-db file is Markdown with a ```` ```toml ```` fence. `parse_rustsec` parses the whole body as TOML (`crates/advisory/src/parse.rs:131-145`), so it fails with `InvalidToml`.
   - The unmaintained notice is `informational = "unmaintained"` in `[advisory]` (Markdown form), or `affected[].database_specific.informational` (OSV form). Neither parser reads `informational`. Both default an empty category list to `Vulnerability` (`parse.rs:181-209` RustSec, `parse.rs:660-673` OSV). So bincode would be reported as a **vulnerability**.
   - The title is dropped: `Advisory` has no summary field (`crates/advisory/src/model.rs:282-308`), and neither does `AdvisorySurfaceDto` (`crates/advisory/src/wire.rs:18-48`).
   - Nothing ever fills the authority. `refresh_authority_source` (`crates/local-service/src/builtin/registry.rs:504`) has no caller. The comment at `registry.rs:495-499` says refresh is deliberately left to "a future explicit refresh command". The test fixtures are synthetic (`crates/advisory/src/lib.rs:48-107`).
3. **Resolved uses of a dependency exist nowhere today, not even for standalone projects.**
   - **Workspace members get no compiler publication.** `apps/desktop/tests/content_truth.rs:9-13, 297-305`: references come back `GapReason::NoSemanticPublication`. The cause is rust-analyzer's loader, which opens the member's own root (`frontends/rust/src/legacy/authority.rs:174`) and walks up to the root workspace. `frontends/rust/fixtures/rich_project/Cargo.toml:6-14` documents the workaround.
   - **A compiled project loses what its references resolved to.** Every reference that resolves into a library crate becomes `ForeignOrigin::Universe{cargo}`, keyed only by the written spelling: `authority.rs:643` `if source_root.is_library { return None; }` → `crates/engine/src/driver/lower/rust.rs:4175-4190` `foreign_universe`. This happens even though rust-analyzer resolved it.
   - **No later join repairs it** (agent, searched).
   - **`references` only scans the target's own package**, so it cannot answer "where does desktop use toml": `crates/local-service/src/builtin/commands/semantic_query.rs:853-856`, `key.package_key() != package → continue`.
   - The prototype's "80 places" came from `releases.mjs`'s token scan, and every other dependency used grep. See §1.5.
4. **The compiler lane can't give public paths after re-exports either.**
   - `RustReexport` keeps only the written name and a `resolved: bool` (`authority.rs:1228-1235`). `emit_reexports` records an `unknown_record(NoIrRepresentation)` (`rust.rs:3679-3714`), so a re-export reaches clients as a name-only `Import` row (`crates/local-service/src/builtin/view_build/identity.rs:77`).
   - Visibility is never copied to `Row` or `Document` (`crates/library/view/model.rs:659-693`).
   - So v1's API table comes from a syntactic extractor, as the prototype and `releases.mjs` did (§1.4a). The compiler lane enriches it once D8 lands.
5. **Nothing resolves requirements, evaluates `cfg(...)`, or knows publish times.**
   - The Cargo index decoder reads `name, vers, cksum, yanked`, and per dependency `package|name, req, kind, optional`. It does not read `target`, `features2`, `pubtime`, `rust_version` or `default_features` (`crates/engine/src/registry/ecosystem_decoders.rs:221-283`, `:1645-1681`; `features2` is never referenced).
   - `package-versions` lists the locally committed publications, not the registry's history, and has no publish time (`RegistryPackageRecord`, `crates/library/surface.rs:855-885`).
   - The only semver code is advisory range matching, with RustSec semantics: a bare version means `>=` for patched (`crates/advisory/src/version.rs:266-300`). It does have `caret_upper` / `tilde_upper` (`version.rs:352-395`) to build on.
6. **Types in plain words already exist in `facet`.** `apps/facet/src/semantics/types.rs:1-16`: `parse` gives a `TypeExpr`, and `Scope::spell` gives "maybe X", "list of X", "X or fails with E", "text", "path". The upgrade-lens model already compares signatures as trees and folds re-exports to `via` (`apps/facet/src/data/release.rs:1-22`). Search-by-shape needs the same vocabulary *in the backend* to rank. House rule: one authority per thing (gui-plan §7). See decision D1.
7. **`DTO_VERSION` is still 7** in both the tree and HEAD (`crates/library/wire/mod.rs:65`). W-Facts has so far added only a canonical-bytes pin test (`crates/library/canonical/tests.rs`, +75 lines, `git diff --stat`). My wire additions ride W-Facts' 8 (§2.3).
8. **`nudox://` is a display string, not a parser.** `Address::full()` (`apps/desktop/src/shell/thread.rs:262-282`) formats it. Routes are the typed `Route` enum (`apps/desktop/src/navigation/route.rs:201-211`: `Orbit | Package | Symbol | World`). The three browse routes are new variants plus display addresses.
9. **The upgrade lens is waiting on the same release data Judge needs.** `facet::data::release` is built (`apps/facet/src/data/release.rs`). gui-plan §3 J3 is specified, and §8.5 says a journey reports `BLOCKED (data)` until the index serves release history and diffs. Browse's release facts (§1.4) serve both.

---

## 1. Data contracts

One row per fact the prototype computed. Status: **exists** (usable as is), **partial** (the index has the raw material but no projection), or **new**.

| Fact | Prototype source | Real source today | Status | Owner (built here) |
|---|---|---|---|---|
| Tree: members, direct deps per member with kind, pins, all packages | `load_tree()` over Cargo.lock + member Cargo.toml (`build_data.py:317-390`) | line-scan lockfile, no edges (`product_state.rs:1411`) | new | `cargo metadata` + Cargo.lock → `library::browse::tree` (pure) via `local-service::builtin::browse::cargo` (I/O); command `project-tree` |
| Tree: why-paths, duplicates, host vs other platforms | BFS over lock graph (`build_data.py:1617-1644`), dupes by name (`:2041-2058`) | none (agent: no transitive/why/duplicate code anywhere) | new | same |
| Tree: role + evidence per direct dep | `derive_role` + `ROLE_RULES` over unpacked `Cargo.toml` (`build_data.py:478-519`) | categories/keywords are in `cargo metadata` (**measured**) | new | rule table `library::browse::roles` (pure); words in `crates/present` |
| Tree: license per package | unpacked `Cargo.toml` | `cargo metadata` `license` (**measured**) | exists (via Cargo) | carried in the tree reply; the license *reading* is D-Marks' (`readSpdx`) |
| Advisories: per package-version and whole-tree health | `gh api` RustSec, 92 of 1,189 checked (`build_data.py:1704-1761`) | parsers misread real data; authority never filled (§0.2) | partial | fix `crates/advisory`; command `advisory-refresh`; health inside `project-tree` |
| Release versions with pubtime, yanked, MSRV | index cache `.cache` files (`build_data.py:152-180`, `stability` `:1326-1372`) | decoder drops pubtime/rust_version (§0.4) | partial | full index-row decoder `library::browse::index_row`; command `release-history` |
| API diffs per release pair, semver slip | `releases.mjs` (toml, smallvec only) | none; `facet::data::release` consumes the shape | new | `library::browse::diff` (pure, ports `releases.mjs:291-359`); command `release-diff` (lazy, last 12, cached) |
| Public API per package: display path after re-exports, kind, owner, signature | regex scan of sources (`build_data.py:891-1088`); `extract.mjs` publicApi for `releases.mjs` | semantic `Item{…visibility…}` exists (`crates/semantic/src/ir/semantic/relations.rs:240-250`, agent), but re-export targets are not kept (`authority.rs:1228-1235`, `rust.rs:3679-3714`) and visibility never reaches `Row` (§0.4) | partial | syntactic extractor `frontends/rust/src/api.rs` (§1.4a) → `library::browse::api`; command `package-api` |
| First doc sentence | `first_sentence` (`build_data.py:540`) | `summary_of` keeps the first *line*, not sentence (`crates/present/assemble.rs:294-304`) | partial | `first_sentence(&[Fragment])` in `crates/present` (shared by CLI/MCP/desktop) |
| Shape in words, bound-aware | `shape_of` / `word_for` / `bound_word` (`build_data.py:1094-1228`) | structural `TypeExpr` + `TypeParameter.bounds` in the IR (`crates/semantic/src/ir/semantic/packed_types.rs:1360-1465`, agent); GUI speller in `facet::semantics::types` | partial | decision D1: one speller shared by facet and backend |
| Trait-impl facts (which types implement Deserialize) | `parse_impl` over sources (`build_data.py:805`) | `Implementation` entities with `set_impl_trait` (`crates/engine/src/driver/lower.rs:1618`, agent); no `Implements` links are ever built (agent, verified by grep); W-Facts R1 adds `ImplementationFacts{self_type, contract, blanket}` on rows (W-Facts PLAN §4 R1) | partial | consume W-Facts R1; `package-api` lists `impls` |
| Resolved uses of each direct dep: item, file, line, line text | releases.mjs scan for toml/smallvec; grep for the rest (`code_uses`, `build_data.py:1547-1605`) | none: `references` is own-package only; foreign refs are `Universe` spellings (§0.3) | new | by-path tier now (`frontends/rust/src/uses.rs`), resolved tier after D8; command `package-uses` (§1.5) |
| Closure cost vs the lockfile | `closure()` with feature unification lite (`build_data.py:251-313`) | none | new | `library::browse::closure` (pure) over index rows + host cfg |
| Search: words / shape / like-X / pasted code, and the verdict rule | `browse.js:141-344` | lexical search over rows (`crates/local-service/src/query/local.rs:513-527`, agent); `index-search` is a name/label filter (`product_state.rs:796-840`) | new | `library::browse::find` (pure ranking); command `find` |
| Capability rows per family (compare "what else it can do") | `CAPS` detectors (`build_data.py:1381-1491`) | none | new | `library::browse::caps` (detectors over the API table) |
| Equivalence of an incumbent's items in a candidate | by name + `TWINS` (`build_data.py:1494-1545`) | none | new | `library::browse::equivalence`, type-aware (ruling 1, §1.6) |
| Cousins in other ecosystems | hand-written `COUSINS` (`build_data.py:1648-1700`) | none | deferred (§6) | — |
| Downloads (⌥ only) | crates.io API | forge facts carry stars, not downloads (`crates/engine/src/forge/facts.rs:180-215`, agent) | deferred (§6) | — |

### 1.1 Tree facts (slice 1)

**Source.** The tree comes from Cargo, invoked by the local service through the Rust frontend's toolchain (the `purl.rs:229-260` precedent). Two reads, both offline:
- `cargo metadata --offline --locked --format-version 1 --filter-platform <host>`. The host comes from `rustc -vV`. The engine already calls `rustc --print sysroot` (`crates/engine/src/application/host/authority.rs:170`).
- Cargo.lock, parsed properly with `toml`, for the all-platform set and for the fallback when Cargo can't answer.

The output bound is 32 MB (4.5 MB measured) and the call is cancellable.

**Pure model** (`crates/library/browse/tree.rs`, no I/O):
```rust
pub struct ProjectTree {
    pub source: TreeSource,            // Cargo { host: Triple } | Lockfile  (fallback says which)
    pub members: Box<[Member]>,        // name, short name (common "<prefix>-" dropped), has_bin
    pub packages: Box<[TreePackage]>,  // name, version, origin: Registry|Git{url}|Vendored{path}, license, description,
                                       // categories, keywords, rust_version, builds_here: bool
    pub direct: Box<[DirectDep]>,      // package, by: [(member, DepKinds{normal,dev,build}, requirement, target: Option<cfg>)],
                                       // role: RoleId, evidence: RoleEvidence
    pub why: Box<[WhyPath]>,           // per package: [member, pkg…] shortest, bin-first tie-break
    pub twice: Box<[Duplicate]>,       // name → versions (each with why + who requires it and with what req), yours: Option<version>,
                                       // movable: Option<Move{to, drops_copy: bool, blockers: count asking for the other bucket}>
    pub health: TreeHealth,            // from the advisory authority: coverage, checked/of, affecting: [(pkg, ver, id, summary, status, why)]
    pub other_platforms: u32,          // lock external − host (309 here)
}
```
- **Short member names.** The prototype hard-coded `replace("backend-", "")`. The rule is instead: drop the longest `<word>-` prefix that every member shares. All 44 members start with `backend-` (**measured**).
- **Why-paths.** BFS from members over the host graph. Ties go to members with a bin target first, then name (12 bin members, **measured**). Versions display without build metadata (`1.1.5`); ⌥ spells `1.1.5+spec-1.1.0`.
- **Duplicates.** Group by name, then by semver bucket (the caret class). This gives 60 on the host (**measured**) versus 114 in the whole lock.
  - "Moving yours to 1.1.5 drops a copy" is derived: your members require the lower bucket, and every other requirer's `req` (from `packages[].dependencies[].req`) accepts the higher version.
  - "Neither is yours to move: N crates still ask for 2.x" counts the requirers per bucket.
  - "4 of your 80 uses touch from_str…" needs uses (§1.5) and diffs (§1.4), so it arrives in slice 4.
- **Vendored.** A package whose Cargo id is `path+file://…/vendor/…` and whose name has a `[patch]` entry reads "gpui-ce 0.2.2 · vendored". It is never counted as unread or unlicensed.

**Roles** (`crates/library/browse/roles.rs`, pure; words in `crates/present`): the prototype's rule order (`build_data.py:478-519`), with the evidence recorded, and two changes that keep it *derived*:
- **Drop the name-prefix clause.** `startswith(("tree-sitter","oxc_","ruff_","ra_ap_","clang"))` (`build_data.py:504`) is hand knowledge. In its place comes **cohort inheritance**: a dependency with no categories or keywords takes the role of a categorized dependency used by exactly the same members. The evidence names that peer: "used only by frontend-rust, like tree-sitter-rust (parser-implementations)".
  - The rule order is: dev-only, then the category/keyword vote, then the description pattern, then cohort, then other.
  - **Measured on the host graph**: 21 of 75 direct dependencies have no categories or keywords. The cohort gives 15 of them a peer: every `ra_ap_*` → tree-sitter-rust, every `ruff_*` → tree-sitter-python, `gpui_ce_platform` → gpui-ce. The description pattern catches blake3 ("hash function") and `futures-*`.
  - Where the peers disagree (clang-sys: tempfile and tree-sitter-cpp), the majority wins, and a tie goes to the peer whose name shares the longest prefix.
  - The rest (trustfall, trustfall_core, turso) land in **other** with "no category, keyword, telling description or peer".
- **Role labels must be generic.** "reads the seven languages" can't be derived; "reads languages" can. Ruling 5 keeps the vocabulary derived and not editable, so the prototype's project-flavoured label goes. The other eleven labels are already generic.

**Caching.** Keyed by blake3(Cargo.lock bytes ‖ each member manifest's bytes ‖ host triple). A changed lockfile rebuilds on the next read.

### 1.2 Advisories (slice 1)

Fixes in `crates/advisory` (owner: nobody claims it; I take it):
1. `parse_rustsec_markdown(bytes)`: strip the ```` ```toml ```` fence and parse the TOML. Keep the Markdown `# Title` as the summary.
2. Read `informational` in both forms: RustSec `[advisory].informational`, and OSV `affected[].database_specific.informational`. `"unmaintained" → AdvisoryCategory::Unmaintained`; `"unsound" → Unsound`; `"notice"` gets a new `Notice` category. An informational advisory is **never** defaulted to `Vulnerability`. Today's default for an empty list stays only for non-informational objects.
3. `Advisory.summary: Option<String>` (`#[serde(default)]`, so persisted authority files written before the change still open) and `AdvisorySurfaceDto.summary` (wire, rides DTO 8).
4. OSV `introduced: "0.0.0-0"` must parse. The real bincode OSV uses it, and I will test it.
5. **Sources.** The RustSec source accepts a local advisory-db **directory** (the git checkout cargo-audit keeps). It walks `crates/*/*.md` into `AuthorityFeed::from_entries`, which exists for "a RustSec tree" (`crates/advisory/src/authority.rs:88-118`). OSV stays URL/file based. I do not add zip support in v1 (§6).
6. **`advisory-refresh`** (Write, System domain): runs the existing bounded adapter, `refresh_authority_source` (`registry.rs:504-560`), for each configured source, then persists. The desktop calls it when the tree opens and a frontier is older than `max_age_secs`, never at startup. That keeps the `registry.rs:495-499` stance.
7. **Tree health** = `AdvisoryAuthority::observe(package, version, …)` (`authority.rs:523`) for every host package. The line reads "advisories checked for 884 of 884" when coverage is complete, and "advisories not checked: no source configured" when `configured` is empty (`authority.rs:530-539`). Never "92 of 1,189".

### 1.3 Registry index rows (slices 2–3)

`library::browse::index_row::CargoIndexRow` holds the full typed row: `vers, yanked, pubtime, rust_version, links, deps[{name, package, req, features, optional, default_features, target, kind}], features ∪ features2, v`. Read from:
1. **Cargo's own sparse cache**, `$CARGO_HOME/registry/index/*/.cache/<shard>/<name>`, read-only and offline. Its format is a one-byte version, a u32 index version, a revision string, then `(version \0 json \0)` pairs (as the prototype parsed it, `build_data.py:163-180`). D-Marks counted "62,044 entries carry `pubtime`" in this cache (marks CHECKPOINT). It is exactly what Cargo resolved your lock from.
2. The engine's registry transport (`crates/engine/src/registry/ecosystem.rs:1246` sparse path) when the package isn't cached and the registry policy allows network. The acquisition decoder stays as it is; `CargoIndexRow` is a second, read-only projection of the same JSON line. I extend `cargo_dependencies` only if the owner wants one decoder (D4).

**Requirements and cfg:**
- Add `pub fn cargo_requirement_matches(req, version)` to `crates/advisory/src/version.rs`, with Cargo semantics: bare = caret, `~`, `=`, `*`/`1.*`, comma-AND, and pre-releases only on a matching comparator. It shares `VersionKey`, `caret_upper` and `tilde_upper` with the RustSec matcher, so there is one semver authority. The tests are Cargo reference examples.
- Add a `cfg(...)` evaluator (`library::browse::cfg`) against the host set from `rustc --print cfg`. The prototype hard-coded macOS (`build_data.py:200-241`); this reads the real set.

### 1.4 Releases and API diffs (slice 3)

- **`release-history {package}`**: every version with `pubtime`, yanked, `rust_version`, a pre-release flag and a caret-class break flag, from `CargoIndexRow`. It is a new command, because `package-versions` means local publications (`surface.rs:855`). It is the data behind `controls::comb::VersionComb` (`apps/facet/src/controls/comb.rs:102-117`) and D-Marks' Rider/Baseline.
  - "Breaking releases per year, claimed" = caret-class changes in the last 3 years. This is `stability()` (`build_data.py:1326-1357`).
- **`release-diff {package, from, to}`**: measured, lazily (ruling 4). When a package is judged or compared, its last 12 adjacent pairs plus pin→latest are measured and cached by `(package, from, to, extractor version)`.
  - The diff engine ports `releases.mjs:291-359`: added/removed/changed/deprecated/fields/variants/renamed, breaking vs additive, `semverSlip` by caret class.
  - Compare rules:
    - keys are the **declared path**, with aliases folded to `via` (`releases.mjs:33-47`);
    - signatures are normalized as in `releases.mjs:253-270` (`Self`→owner, positional lifetimes, trailing commas, whitespace), **and parameter names are erased**, per the brief;
    - a change that reads the same in plain words counts as respelled, as `facet::data::release` already does.
  - **Per-release API source.** Each release's source comes through the existing acquisition (fetch → verify → stage: `crates/engine/src/acquisition/phase.rs:113-278`, `crates/local-service/src/builtin/registry.rs:696-751`, agent). It is then read by the **§1.4a extractor**, the way `releases.mjs` works, not by the compiler: twelve rust-analyzer runs per judge would be far too slow.
  - The reply says "measured from source, not type-checked". When a compiler publication of that release exists, it wins.
  - The lead's list notes smallvec showing zero diffs as an extractor defect. My port is tested on smallvec 1.16.0→1.16.1 with the real sources, and must report the real difference or fail.

### 1.4a The public API extractor (slice 2; also feeds 1.4 and 1.8)

One syntactic extractor, `frontends/rust/src/api.rs`. It is a Rust port of `extract.mjs`'s `publicApi` (the reference `releases.mjs` builds on) and of `build_data.py:891-1088`, over tree-sitter-rust, which the frontend already embeds (`frontends/rust/lib.rs:15-40`). Given a crate root (`src/lib.rs` or the `[lib] path`), it:
- walks `mod` declarations to files and records visibility on each step;
- folds `pub use` (renames, globs, `self`, nested groups, crate-level `pub use other_crate::X` as an external `via`) to every public alias of each declared item. The display path is the shortest alias; `via` is the declared path (`releases.mjs:33-47`);
- records inherent and trait `impl` blocks (owner, trait path as written), so it can answer "which types implement Deserialize" (`deser` in the prototype, `build_data.py:1917`);
- records the doc comment, the `#[deprecated]` note, `#![no_std]` and `#![forbid(unsafe_code)]`;
- normalizes signatures (`releases.mjs:253-270`, plus parameter names erased for comparison).

It serves four readers with one algorithm: `package-api` (§2), per-release diffs (§1.4), std (§1.8) and uses resolution (§1.5). It is labelled `Structural` in replies.

Where a compiler publication exists and D8 has landed, resolved re-export targets and impls replace the syntactic ones, and the label becomes `Semantic`. W-Facts R1 (impl rows with `ImplementationFacts`) and F1 (deprecation) are the same facts on `Row`. When those land, the extractor's impl and deprecation fields are cross-checked against them in a test rather than kept as a second opinion.

Cost (**measured** file counts): toml 0.8.23 has 14 `.rs` files. The toolchain's std, core and alloc have 669, 269 and 76. The API table is cached by blake3 of the crate's source bytes, so std is paid once per toolchain.

### 1.5 Uses of each direct dependency (slices 2 and 4)

**The fact:** `(dependency item's declared path, file, line, line text)` for every place a project's own code uses a direct dependency. §0.3 shows that no path in the index produces it today. There are two tiers, and every count names its tier.

**Tier B, "by path": build now (S2). It works on the owner's repo today.**
- `frontends/rust/src/uses.rs` ports `releases.mjs`'s `scanUses` (`releases.mjs:104-122` for the schema, `:367-463` for the code). Over tree-sitter, for each dependency ident, it records:
  - full path expressions (`toml::Value::as_table`);
  - turbofish arguments (`from_str::<toml::Value>` records both paths);
  - `use` imports followed by bare names, including UFCS such as `.and_then(Value::as_str)`;
  - `#[derive(Deserialize)]` names imported from the dependency.
- It resolves each spelling against the dependency's §1.4a API table, falling back to the owner prefix for variants (`toml::Value::String`).
- The ident comes from Cargo (`packages[].dependencies[].rename`), so `gpui = { package = "gpui-ce" }` is scanned as `gpui::`.
- Excluded: `tests/`, `benches/`, `examples/`, `#[cfg(test)]` and `mod tests` (`releases.mjs:116-122`).
- **Not seen:** method calls on values (`value.as_str()`). The reply says so, and the page counts "at least N places".
- **Measured scale on this repo:** the same algorithm (`releases.json`) finds toml 0.8.23 used 80 times in 4 files: `toml::Value` 29, `Value::as_str` 20, `Value::as_table` 11, `Value::as_bool` 5, `from_str` 4, … It finds smallvec used 8 times in 3 files. These are the prototype's "80 places".
- It runs on the project's own `.rs` files. Results are cached per file by content hash, so one changed file rescans only that file.

**Tier A, "resolved": after D8.** Two frontend changes make the compiler lane answer, method calls on values included:
1. **Keep what rust-analyzer resolved.** In `frontends/rust/src/legacy/authority.rs` (`:556-568`, `:643`): when a reference resolves into a library source root, emit `ForeignOrigin::Package { ecosystem: cargo, package: <crate name> }` with the definition's canonical path. Today it emits `Universe` plus the written spelling.
2. **Give re-exports their target.** `RustReexport` gets its resolved target, lowered as a `LinkKind::Reexports` link. That kind is already in the vocabulary (`crates/semantic/src/ir/semantic/relations.rs:263`, encoded at `crates/engine/src/index_build/fact/value.rs:63`), so there is no image-format change.

Then `package-uses` reads the link occurrences in the *project's own* publications (`LinkOccurrence.source`, `relations.rs:301-305`) whose origin is the dependency.
- **Standalone projects** (the `toml_pin` fixture) get tier A as soon as change (1) lands.
- **The owner's workspace** also needs workspace members to compile (§0.3, D9). That is an engine decision outside Browse, so until then its counts stay "by path".

**Command.** `package-uses { project, dependency }` → `Uses { tier: Resolved | ByPath, sites: [UseSite{declared, file, line, text, spelling}], unseen: Option<Unseen::MethodCallsOnValues> }`. When both tiers exist for a project, Resolved wins.

**Uses feed:**
- the verdict's "calls it in N places" (S2);
- the tree rows' trailing "N places" (S2, back into S1's page);
- the Judge care and switch lines (S3);
- the Compare "today" band and the adoption preview (S4);
- "Here twice": "4 of your 80 uses touch from_str" (S4, with `release-diff`).

### 1.6 Equivalence and the adoption preview (slice 4; ruling 1 is binding)

`library::browse::equivalence` classifies, for every incumbent use path P and candidate package C, a cell:
- `Same { item }`: C has an item standing where P stands, and its **shape** matches: parameters in words, output in words, and receiver type in words, under the type mapping below.
- `ReadsDifferently { item, ours, theirs }`: an item of the same name exists with a different shape. The cell shows the difference in words ("(Item) → maybe Table").
- `Missing`: honest empty space.

Type mapping. A candidate type T′ stands for the incumbent type T only when **every method your code calls on T** (from §1.5 uses, project-wide, not only on that line) has a `Same` cell on T′. This is stricter than the ruling's "on that line", on purpose. A field declared `BTreeMap<String, toml::Value>` calls nothing on its own line, but it *carries* the uses made elsewhere. Please confirm (D5). Under this rule, `toml_edit::Value` does not stand for `toml::Value`: `as_table` lives on `Item`, and `toml_edit::Value::as_table` does not exist.

Line verdicts in the preview:
- "changes only in name": every substitution is `Same` or a covering type;
- "reads differently";
- "no equivalent for X".

**Coverage depends on which uses tier is known (§1.5).**
- Tier B does not see method calls on values, so the covering claim for a type can only rest on the calls it did see.
- **Under tier B, a type substitution never reads "only in name".** It reads "covers the N calls we can see (method calls on values not seen)".
- Function and UFCS substitutions whose shapes match may still read "changes only in name", because tier B sees those calls in full.
- "Only in name" for a *type* needs tier A.

**Until shapes exist for both packages, every line says "matched by name" and never "only in name"** (ruling 1). The `TWINS` table (`build_data.py:1530-1535`: SmallVec↔ArrayVec, Agent↔Client) is hand knowledge. It goes, replaced by the covering-type rule, which derives the same pairs where they are real.

### 1.7 Closure cost (slice 3)

`library::browse::closure` ports `closure()` (`build_data.py:251-313`): a worklist, features with `dep:` and `a/b` / `a?/b`, optional deps, default features, the build+normal kinds, and `cfg` evaluated for the host (§1.3). It compares against the tree's host set by `(name, caret bucket)`:
- `adds: [(name, version)]`
- `second_copies: [(name, version, yours)]`
- `already_here`
- `unknown: [name]`

The reply says "resolved with default features for this machine; Cargo may unify features differently", and `unknown` is never hidden. The same function serves "adds 3 crates" in Find rows, the Judge care line and the Compare costs band.

### 1.8 Search (slice 2)

`library::browse::find` (pure) ranks over a `Corpus` the local service assembles:
- the API tables of every package the index has read;
- std (below);
- the tree (for "yours / in your tree");
- `cargo metadata` descriptions and keywords for tree packages the index hasn't read. These appear with "not read yet"; they are never ranked by an invented item.

It ports `browse.js` as specified:
- **Query reading** (`browse.js:141-159`): words, `a -> b` shape, `like X`, and pasted code (`path::with::segments` plus `(`/`!`/`<`).
- **Words** (`:176-251`): the `SYN`/`NOUN` tables, noun exclusion (`:191-198`, so `parse toml` never answers `str::parse`), item scoring (`:199-223`), and a 0.6 floor under the top score.
- **Shape** (`:252-276`): inputs in any order, output through maybe/fails, each extra parameter costing 0.6. The **Deserialize rule**: a generic `T: Deserialize` output answers `Value` when this package's `Value` implements Deserialize (from impl facts, §1).
- **Like X** (`:278-301`): shared public item names minus `NOISE`, normalized by √(|A|·|B|), plus keyword and category overlap.
- **Code** (`:302-329`): the pasted path is resolved in its package, and other packages are ranked by change size: same name and shape, then same shape in the family, then same name. Under ruling 1, a "same name, other shape" row reads "same name, other shape" (as the prototype did) and never "changes one word".
- **The verdict rule** (`:330-339`): for words and shape queries, std, or a direct dependency of yours, whose score is ≥ 0.72 × the top score leads. Std wins over yours when both qualify.
- **Std.** The prototype read 750 items from 12 rust-src files (`build_data.py:1232-1272`). The product reads the toolchain's `library/{std,core,alloc}` (from `rustc --print sysroot`) through the **§1.4a extractor**, as a package `std@<toolchain version>`. The `std` facade's `pub use core::…` / `alloc::…` re-exports fold to `std::` display paths. It is sealed once per toolchain and labelled "read from your toolchain's source". The compiler lane for std is deferred (§6).

The SYN/NOUN/NOISE tables are search heuristics, not facts. They live in the pure module with a test per entry that shows what it buys. For example, `parse toml` with `NOUN.toml` removed must let `str::parse` in: that is the mutation.

---

## 2. The wire

### 2.1 House pattern (verified end to end on `dependencies`)

A read command rides `Command::Surface(SurfaceCommand)` / `CommandReply::Surface(SurfaceReply)` (`crates/library/command.rs:873, 957`). The JSON is the enum itself; no bespoke `wire/*.rs` projection is needed. Adding one touches:
- `CommandId` (`crates/library/command.rs:29-118`);
- a `COMMANDS` row and the array length (`crates/library/command_registry.rs:87`, currently `[CommandSpec; 40]`), `command_spec` (`:412-456`) and `is_surface` (`:22-59`);
- `SurfaceCommand` / `SurfaceReply` variants and their `id()` arms (`crates/library/surface.rs:421-424, 1115, 1175`);
- the present grammar row and `is_destructive` (`crates/present/grammar.rs:650-660, 260-296`), the invocation (`crates/present/call.rs:441-443`) and the rendering (`crates/present/product.rs`);
- the local-service handler (`crates/local-service/src/builtin/product_state.rs:175-177`, or a new `builtin/browse.rs` reached from `commands/adapter.rs:468-474`);
- the CLI/MCP tables that walk `COMMANDS` (`apps/cli/src/tests.rs:386`, `apps/mcp/src/jsonrpc/tests.rs:406`).

So every browse command appears in the CLI (`backend find "parse toml"`) and in MCP (`backend.find`) the day it exists. That is the parity rule.

### 2.2 New commands

| Command | Mutation / domain | Request | Reply | Slice |
|---|---|---|---|---|
| `project-tree` | Read / Home | `{ project: ProjectSelector }` (root from the project's lockfile path, `ProjectRecord.lockfile`, `surface.rs:1050-1063`) | `ProjectTree` (§1.1) | 1 |
| `advisory-refresh` | Write / System | `{}` | `AdvisoryFrontiers { per source: complete, observed_at, count, error }` | 1 |
| `find` | Read / Registry | `{ query: ProductText, project: Option<ProjectSelector>, limit: u16 }` | `FindPage { reading: QueryReading, verdict: Option<Verdict{kind: Std\|Yours, row}>, rows: [AnswerRow], unread: [PackageName], cousins: [] }` | 2 |
| `package-api` | Read / Library | `{ package: PackageReference, limit, after }` | `ApiTable { items: [ApiItem{declared, display, aliases, kind, owner, signature, first_sentence, shape: Option<Shape>, deprecated}], impls: [(self, contract)], no_std, source: Semantic\|Structural }` | 2 |
| `package-uses` | Read / Home | `{ project, dependency: name }` | `Uses { tier: Resolved\|ByPath, sites: [UseSite{declared, file, line, text, spelling}], unseen }` | 2 |
| `release-history` | Read / Registry | `{ package: name, ecosystem }` | `[Release{version, published, yanked, pre, breaks_from_previous, rust_version}]` | 3 |
| `release-diff` | Read / Registry | `{ package, from, to }` | `ReleaseDiff` (the `releases.mjs` schema, plus `measured_from: Structural\|Semantic`) | 3 |
| `judge` | Read / Registry | `{ package, answering: Option<ItemPath>, project }` | `Judge { care: Care, version: pin/latest, sentence, signature, switching: Option<Switch>, xray: {churn claimed+measured, api size, msrv, local dependents, advisories} }` | 3 |
| `compare` | Read / Registry | `{ packages: [2..=4], incumbent: Option<name>, project }` | `Ledger { verdict_facts, today: [UseRow{path, count, files, cells}], can: [CapRow], costs: [CostRow], preview: Option<Preview> }` | 4 |
| `dependency-plan` | Read / Home | `{ project, package, version, member: Option<name> }` | `Plan { member, table: WorkspaceDependencies\|Member, hunks: [FileHunk{path, before, after}], digest }` | 5 |
| `dependency-add` | **Write** / Home | `{ digest }` | `Added { member, package, version }`, or a refusal when any planned file's bytes changed since the plan | 5 |
| `package-progress` | Read / Registry | `{ package }` | `Stages { fetch, unpack, read, seal: StageState }` | 5 |

Notes:
- **Replies carry facts, not prose.** Sentences such as "You already have this…" and the verdict lede are spelled in `crates/present`, so the CLI, MCP and desktop say the same thing.
- **`dependency-add` is the first command that writes a user's project file** (agent: none exists today; `CommandMutation::Write` only touches owner state, `command_registry.rs:63-67`). It is two-phase:
  - The plan shows the exact diff, and the add refuses a stale digest. Ruling 2 says never write silently.
  - `grammar.rs` `is_destructive` returns false for it, but MCP gets the `destructiveHint` (`apps/mcp/src/jsonrpc/tools.rs:140`), because it writes the user's files.
  - The writer preserves formatting. `toml_edit` is already in the lock transitively, but not as a product dependency, so adding it is D6.
- **`package-progress` reads real stages.** The owner has no staged progress events; progress is polled counters (`crates/library/progress.rs`, `wire/reply_capability.rs:24-37`, agent). The four stages map onto what exists: fetch = acquisition up to `VerifiedObject`, unpack = `stage_archive`, read = compile/structural, seal = `index_publish::seal` (`crates/engine/src/index_publish/mod.rs:74`, agent).

### 2.3 DTO version

All additions are new enum variants and new optional fields (`AdvisorySurfaceDto.summary`). An older peer fails to decode an unknown variant, so the version must move. W-Facts is taking 7 → 8 (their PLAN §1). I add my variants **under 8** and do not bump separately, provided no build speaking 8 has been handed to anyone before my first wire change lands. If the owner has committed W-Facts' 8 by then, I ask the lead whether to ride 8 or take 9. My recommendation is to ride 8 while every surface is built from one tree.

No golden JSON files pin the version: `wire/tests.rs:931, 949` and `apps/cli/src/tests.rs:143` use the constant (agent).

---

## 3. The GUI

### 3.1 Where each piece lives

New facet module `apps/facet/src/browse/` (a new directory I own, plus one line in `lib.rs`). The components take plain view-model structs and `&Measure` (gui-plan §7: "Measure flows down explicitly"). The desktop maps replies into those structs.

| Component | Built from (existing) | New |
|---|---|---|
| **FindPage**: hero query, reading line, verdict block, answer rows, cousins fold, graph door | `controls::field::field` (`apps/facet/src/controls/field.rs:77`), `motion::shared::shared` for ask field → hero (`motion/shared.rs:224`), `data::door::Door` for the graph door (`data/door.rs:42`), `motion::presence::Presence` for rows arriving (`motion/presence.rs:750`) | the page layout and verdict block |
| **AnswerRow**: mark · name · answering item · one reason | `motion::flow::Flow::item` keyed by package for the FLIP re-rank (`motion/flow.rs:559-604`), `overlay::text::Words` for the underlined hit and a hoverable item (agent), marks' `ecosystem_mark` in the mark slot (stand-in `paint::gem`, `paint/gem.rs:99`, until marks land) | the row |
| **JudgeCard** | `overlay::peek` (`overlay/peek.rs:118-155`, `Peek::Package(PackagePeek)`), `controls::comb::version_comb` (`controls/comb.rs:117`) → marks' `version(Baseline)`, `controls::kbd` for ⌘ key feet, `facet::code` for the signature | **a builder option on `PackagePeek`** (W-Float owns `overlay/**`): care line (mint word, no box, ruling 3), sentence, signature, switch line, ⌥ x-ray. A one-line additive option on the peek (D7), never a copy of it. |
| **CapabilityLedger**: bands, row unfold, column lift, × drop, "+ a fourth" | `motion::flow` for column drop and re-flow, `motion::keys::make_room` (`motion/keys.rs:243`), `overlay::float` for cell peeks | the ledger. The sticky header either covers fully or does not overlap (ruling 2): it paints an opaque ground plate at the ledger's width, and a layout lint asserts that no text sits under it. |
| **AdoptionPreview**: before/after pairs, candidate picker, "no equivalent for X" | `facet::code` highlighter, `controls::seg` for the picker | the pair layout. Changed words are underlined from the equivalence spans, not re-diffed in the view. |
| **LibraryPage**: roles in two columns, transitive fold, why-line, "Here twice" | `measure` columns, `motion::presence` for the fold, `data::door` for the why-line's hops | the page |
| **Adopt drop**: arc flight, squash and bounce, make-room | `motion::shared` (glides straight today, per the checkpoint), `motion::keys::DROP_IN` (`motion/keys.rs:189`), `make_room`, `data::progress::seam` with four named stages (`data/progress.rs:50, 405`, N stages supported) | an **arc path option on `motion::shared`**, an additive option on W-Flow's `shared.rs` (D7), and a squash-bounce `Keys` constant |
| **Odometer**: rolling digits for counts | `motion::spring` per wheel (`motion/spring.rs:13`) | `apps/facet/src/motion/odometer.rs`. Generic: the Library count, the status count and D-Marks' Rider wheels all use it (offered to the marks lane as the one authority). |

Marks (the marks lane builds them): `ecosystem_mark` with `quiet` (my request, now a ruling), `license_mark`, `version` (Rider in the hero, Baseline in rows), `dep_link`. I plan against `marks.js`'s API (`Nudox-Design-System/v4/marks/marks.js:120, 317, 447, 552`). Until they land, rows use `paint::gem` and plain mono version text. Tests assert text, so the swap changes no assertion.

### 3.2 Desktop

- **Routes** (`apps/desktop/src/navigation/route.rs`, W-Shell's area): add
  - `Route::Find(FindRoute { query })`, shown as `nudox://find?q=parse%20toml`;
  - `Route::Compare(CompareRoute { packages, incumbent, preview })`, shown as `nudox://compare/toml,toml_edit,basic-toml`;
  - `Route::Tree(ProjectId)`, shown as `nudox://backend/tree`.

  `Address::full()` (`shell/thread.rs:274-282`) learns the three spellings. These are one-line additive edits under D7.
- **Page keys** (`apps/desktop/src/model/pages/key.rs:41-54`, W-Facts' area): `PageKey::{Find, Compare, Tree, Judge}`, one line each. My models live in a **new** `apps/desktop/src/model/browse/` directory, and the mapping in a **new** `apps/desktop/src/runtime/browse_mapping.rs`, so I touch neither `pages/*` nor `page_mapping.rs` beyond the key lines.
- **Reads**: new compose functions following `compose_package` (`apps/desktop/src/runtime/reads.rs:913-935`, `engine.surface(SurfaceCommand::…)`). `reads.rs` has no listed owner, so I `git diff` it before each edit.
- **Bodies**: `apps/desktop/src/shell/bodies/{find,compare,tree}.rs`. These are thin adapters from `model::browse` to `facet::browse`. The body files are new and mine. The registrations in `bodies/mod.rs` and `region.rs` (W-Shell's) are one-line D7 edits.
- **⌘K** (ruling Q1): Ask stays the quick list and gains a last row, "all answers as a page ↵", which opens `Route::Find` (`apps/desktop/src/shell/ask.rs`, W-Shell's). A D7 edit.
- **Shelf Library**: the prototype's shelf lists roles with counts ("Library · 79 direct · 1,189"). It is `shell/shelf.rs` (W-Shell) on `facet::chrome::shelf` rows (`chrome/shelf.rs:45, 356`). I provide `model::browse::TreeModel`. Rendering the roles in the shelf is more than one line, so W-Shell does it, or the lead widens D7 for it.

---

## 4. Slices

Each slice goes from the real index to a real page, has content-asserting tests and one mutation that bites, and is checkpointed with stills at 1440 / 760 / 480, 100 % and 200 %.

**S1 · Your tree.**
- Backend:
  - `cargo_requirement_matches`;
  - Cargo.lock parse and `cargo metadata` reader;
  - `library::browse::{tree, roles}`;
  - the advisory fixes (§1.2) and `advisory-refresh`;
  - `project-tree`;
  - present words.
- CLI: `backend project-tree backend` prints roles, the unmaintained line and "Here twice".
- GUI: LibraryPage, `Route::Tree`, the shelf Library model.
- Output:
  - "Your 44 packages lean on 75 others directly, and 884 in all";
  - "bincode 1.3.3 is unmaintained ›" with its why-path;
  - "60 crates are here twice" (host);
  - roles with evidence on hover;
  - `toml` twice, with both paths and "Moving yours to 1.1.5 drops a copy".
- Uses and "N places" are not in S1: row trailing zones show the members ("for desktop, engine, local-service and advisory") until S2 brings uses.

**S2 · Find: words, "you already have this", std.**
- Backend:
  - the §1.4a extractor, with `package-api` and `first_sentence`;
  - by-path uses and `package-uses`;
  - std through the extractor;
  - `library::browse::find` words mode plus the verdict rule;
  - `find`.
- The tree page gains its "N places" trailing zone.
- GUI: FindPage, AnswerRow, FLIP re-rank, the ⌘K last row, `Route::Find`.
- Output: `parse toml` → "You already have this. from_str from toml — your code calls it in N places." and `read a file into a string` → std `fs::read_to_string` "in the standard library".
- Shape, like-X and code modes follow in S2b once D1 (the shared speller) is settled. They are cheap once shapes exist.

**S3 · Judge.**
- Backend: `release-history`, closure cost, `judge`; then `release-diff` (lazy, last 12, cached, structural extraction), shared with J3's upgrade-lens data.
- GUI: JudgeCard through W-Float's peek option, the comb (marks' Baseline, or `version_comb` until then).
- Output:
  - care = "yours · N places" / "already in your tree" / "adds 2 crates: toml_writer, serde_spanned" / "unmaintained" / "deprecated";
  - claimed churn;
  - measured churn once S3b lands;
  - ⌥ x-ray.

**S4 · Compare with incumbent uses.**
- Backend: equivalence (§1.6), `caps` detectors, `compare`, the adoption preview.
- GUI: CapabilityLedger, AdoptionPreview, `Route::Compare`.
- Output: the verdict sentence from real cells. Under ruling 1, "toml_edit has an equivalent for all nine" can only appear if the shapes say so. My expectation from the ruling is that `as_table` and `as_array` read differently and `Value` is not a rename.

**S5 · Adopt.**
- Backend: `dependency-plan`, `dependency-add` (format-preserving write, workspace table if `[workspace.dependencies]` exists, which it does here, `Cargo.toml:24`), `package-progress`.
- GUI: the diff sheet before writing, the adopt drop, the Odometer, the four-stage seam.
- Output: the exact two hunks, then after confirmation: "80 direct · 885 in all", and the seam fetch → seal, with real stage states.

---

## 5. Tests (content, never counts; one mutation per slice)

- **S1 seam (hermetic, fast).** A pinned fixture captured from this repo on 2026-09-27: `Cargo.lock` (325 KB) plus `cargo metadata` JSON trimmed to the fields used (target < 2 MB, per the fixtures memory), plus the two real RUSTSEC-2025-0141 files (CC0-1.0, per their `database_specific.license`) under `crates/library/browse/fixtures/tree-2026-09-27/`. Assertions are on rendered present text:
  - `why("toml","0.8.23") == "desktop → toml 0.8.23"`;
  - `why("toml","1.1.5+spec-1.1.0") == "frontend-rust → ra_ap_project_model 0.0.341 → toml 1.1.5"`;
  - the health line contains `"bincode 1.3.3 is unmaintained"` and the why-path through `syntect 5.3.0`;
  - `toml`'s role evidence contains `"encoding"` and `"used by advisory, desktop, engine, local-service"`;
  - `ra_ap_hir`'s evidence names its cohort peer;
  - the platform line is "884 build here … 309 more for other platforms".
- **S1 advisory unit tests** on the real bytes:
  - the Markdown form → `Unmaintained`, summary "Bincode is unmaintained", `range_matches(…,"1.3.3") == true`;
  - the OSV form → the same three facts. Both forms must agree, which is a cross-check.
- **S1 end to end.** A real `backend-locald` runs `project-tree` on this repository root through the desktop runtime path, in the `content_truth.rs` style (`apps/desktop/tests/content_truth.rs:214`). It asserts the same two why strings on the `TreeModel` and on the rendered LibraryPage text (`shell/tests.rs:435` style, `said.iter().any(|line| line == expected)`).
- **S1 mutation.** Revert the `informational` read in the RustSec parser. The seam test must panic quoting `"bincode 1.3.3 is unmaintained"`, and the advisory test must panic on `Unmaintained`. I run it in one Bash command with `trap` restore and `touch`, twice, with identical output.
- **S2.** The existing `frontends/rust/fixtures/toml_pin` project, which pins `toml = "0.8.23"` and calls `toml::from_str` once (`frontends/rust/fixtures/toml_pin/src/lib.rs:16-18`). The harness roots gain `basic-toml-0.1.10` and `toml_edit-0.22.27` from the cargo registry cache. Both are unpacked on this machine (**checked**), and they are added the way toml-0.8.23 already is (`apps/desktop/src/harness.rs:97-112, 158-166`).
  - `find("parse toml")` → verdict `Yours`, row 0 = `toml` / `from_str`, the present text "You already have this. from_str from toml — your code calls it in 1 place."
  - `find("read a file into a string")` → verdict `Std`, row 0 = `fs::read_to_string`.
  - The mutation: drop noun exclusion, so `str::parse` enters and the test panics naming it.
- **S3.**
  - `release-history toml` from a pinned `.cache` file copy: `0.8.23` has `published` = the index's `pubtime` string, and `yanked` is false.
  - `release-diff smallvec 1.16.0 1.16.1` on real sources must show the real changed paths, which is the lead's zero-diff defect as a test.
  - The mutation: stop erasing parameter names, and a rename-only pair must turn "changed".
- **S4.** A compare of toml vs toml_edit vs basic-toml on the fixture's real uses.
  - Assert the `as_table` cell reads differently, with the words `(Item) → maybe Table`, and no line says "only in name".
  - The mutation: equate shapes by name only, and the preview must claim "only in name", which the test catches. This is ruling 1's defect, as a test.
- **S5.**
  - `dependency-plan` over a temp copy of a workspace with `[workspace.dependencies]` asserts both hunks' exact text.
  - `dependency-add` with a stale digest refuses.
  - The mutation: skip the digest check, and the stale write must be caught.
- **Journeys.** A new J7 "browse": ⌘K `parse toml` → "all answers as a page ↵" → the Find route → hover basic-toml → Judge → compare → the preview. Checkpoints are the texts above. It reports `BLOCKED (data)` for any reply not yet served, per gui-plan §3 item 10.

---

## 6. What I would defer, and why

- **Cousins in other ecosystems.** Nothing from npm/PyPI/Go/Java/.NET/C++ is indexed here (prototype `sources.cousins`). A hand-written table in the product is the "invented phrase beats a real list" defect the marks rulings forbid (marks RULINGS 7). The fold stays out until another ecosystem's API is in the index.
- **Downloads under ⌥.** A crates.io API dependency for one secondary number. gui-plan house rule 6: usage is secondary.
- **Network registry search** (crates.io `?q=` for packages this machine has never seen). Find v1 ranks what the index has read, plus your tree, plus std. The honest footer is "searched N packages this machine has read". The network lane waits for the registry's offline policy.
- **OSV zip ingestion.** A local advisory-db directory plus the per-file forms cover RustSec. The zip needs a new product dependency (`zip` is a workspace dep, `Cargo.toml` `[workspace.dependencies]`, but not in advisory's closure).
- **Compiler-lane std, and compiler-lane per-release diffs.** Too slow per judge. The structural lane plus the `pub use` fold is the `releases.mjs` reference, and it is labelled.
- **Feature-aware closure** beyond default features ("with `serde` on"). The ledger says "default features".
- **Role rename and merge** (ruling 5).
- **"N lit in the graph" door counts.** The link is cheap; the count needs the graph lane's query.

---

## 7. Decisions

The lead's leanings (message of 14:45) are recorded against each decision, with what each makes concrete. Final rulings come after review.

- **D1. One speller for shapes.** *Lead leans: a gpui-free crate, re-exported by facet.*
  - **The crate.** A new core crate, `crates/words` (`backend-words`), registered in `docs/architecture/package-dag.json` as core, ordered before `backend-library`, and bumping `target_packages` 30 → 31.
  - **What moves.** It takes `TypeExpr`, `parse`, `Scope`/`Vocabulary`/`spell` and `Piece` from `apps/facet/src/semantics/types.rs` (1,152 lines).
    - `gpui::SharedString` becomes `Arc<str>`, and `Target::Node(NodeId)` stays a plain `u32`. Three lines mention graph node ids.
    - facet keeps a thin `semantics::types` that re-exports it and converts to `SharedString` at the element boundary, so its 24 call sites in facet and desktop don't change.
  - **Order and ownership.** It is one move with no behaviour change, proven by facet's existing semantics goldens (`apps/facet/src/semantics/tests/*.golden`), which must pass byte-identically. It lands before S2b (shape search) and S4 (equivalence). The move touches W-Anatomy's file, so I do it only with the lead's go-ahead and a `git diff` guard.
- **D2. Uses tiering.** Ship "by path" (tier B, §1.5) in S2 for every project, labelled "at least N places · matched by path", and treat "resolved" (tier A) as an upgrade.
  - The alternative is to wait for tier A. For the owner's repo that also needs workspace members to compile (D9), so Browse would have no uses on its own codebase for a long time.
  - I recommend shipping tier B now. §1.6 says what it can and cannot claim.
- **D3. Cargo as the tree authority.** *Lead leans: Cargo metadata, with a lockfile fallback.* §1.1 as written.
  - `TreeSource::Cargo{host}` or `TreeSource::Lockfile` is in the reply.
  - With the lockfile fallback, the page says "read from Cargo.lock: every platform, no licenses or categories", because metadata facts are absent and roles fall to evidence "not read".
- **D4. One Cargo index decoder or two.** *Lead leans: a separate read-only `CargoIndexRow` for now.* §1.3 as written. The acquisition decoder and its fact identities (`ecosystem_decoders.rs:221-283, 1645-1681`) stay untouched, and a merge is noted for later.
- **D5. Type coverage in equivalence.** *Lead leans: project-wide.* §1.6 as written, with the tier interaction: under tier B, a type is never "only in name".
- **D6. `toml_edit`.** *Lead leans: fine, it's already in the tree.*
  - **Measured on the host graph:** `toml_edit 0.22.27` is already built with `display, parse, serde`, because `toml 0.8.23` depends on it.
  - So `backend-local-service` gets `toml_edit = "=0.22.27"` and adds no crate and no second copy. The workspace table gets one line, like its `toml = "0.8.23"` neighbour (`Cargo.toml:24` onwards).
- **D7. Edits outside my files.** *Lead leans: tight, additive one-line edits with a `git diff` guard.* The whole list:
  - `apps/desktop/src/navigation/route.rs` (W-Shell): three `Route` variants and their `RouteKey` / `RouteDepth` arms.
  - `apps/desktop/src/shell/thread.rs` (W-Shell): the `Address` spellings for find, compare and tree.
  - `apps/desktop/src/shell/bodies/mod.rs` and `region.rs` (W-Shell): registering the three bodies. The body files themselves are new, and mine.
  - `apps/desktop/src/shell/ask.rs` (W-Shell): the "all answers as a page ↵" row.
  - `apps/desktop/src/model/pages/key.rs` (W-Facts): four `PageKey` variants and their `family()` arms.
  - `apps/facet/src/overlay/peek.rs` (W-Float): a `PackagePeek` builder option for the judge card's extra lines. Content only: no new float kind.
  - `apps/facet/src/motion/shared.rs` (W-Flow): an `.arc(lift)` path option.
  - `apps/facet/src/lib.rs`: `pub mod browse;`.
  - The `crates/library` registry and surface files (house pattern, §2.1) are shared rather than owned by a lane, and W-Facts edits them for DTO 8. I edit them additively, after W-Facts' wire change lands or with a diff check.

  Before each edit I run `git diff <file>`. If another lane has uncommitted hunks in the lines I touch, I stop and report.
- **D8. Two Rust-frontend changes** (§1.5, tier A): package-origin foreign references with canonical paths, and resolved re-export targets as `Reexports` links. They also serve gui-plan §8.5 ("uses … from path expressions, imports plus bare names, and turbofish. Method calls on values need type inference and are not yet covered"): the compiler lane *does* resolve method calls on values; the lowering discards the result.
  - Who owns `frontends/rust` and `crates/engine/src/driver/lower/rust.rs` now? W-Facts touches `lower.rs`/`rust.rs` for F1 staging (their PLAN §3). I would do this after their Phase 1 lands, in a separate change.
- **D9. Workspace members without a compiler publication** (§0.3) is an engine-level gap, and it blocks resolved uses, relations and references on the owner's own code everywhere, not only in Browse. I flag it and do not plan it.

---

## 8. Prototype defects the implementation must not copy

1. Ruling 1: false "only in name". Tested in S4.
2. Ruling 2: sticky header fragments. The plate plus a lint.
3. Ruling 3: the boxed care line.
4. `tree-why-1440.png`: inline mono spans lose their surrounding spaces ("Moving yours to1.1.5drops a copy"), and the hovered row's role evidence overlaps the neighbouring column ("role: encoding · …" under strum's versions). Sentences with identifiers are one `StyledText` run (gui-plan §7 "Text"), and the evidence line wraps inside its column.
5. `find-code-1440.png`: the doc sentence shows raw Markdown backticks ("of type `T`"). `first_sentence` turns inline code into code runs.
6. `adopt-1440.png`: the shelf name clipped to "basic-t" mid-reveal is fine in motion, but it must settle to the full name. Settle == fresh (gui-plan §3.4) covers this.
7. Counts sourced by grep and presented as places. Every count names its tier (§1.5).
8. All-platform counts presented as "your tree" (1,189). The host count leads, per ruling 3.
