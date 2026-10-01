# D-Browse: checkpoint

> Copied verbatim from the lead's scratch folder (`$S/wave4/browse/CHECKPOINT.md`) into
> `docs/architecture/briefs/package-browsing/` by the W-Browse finisher, 2026-09-27. Every
> path below that starts `v4/` or `Nudox-Design-System/` is a real repository path already
> committed; this document has no `$S/...` references of its own.

**Working prototype:** `http://127.0.0.1:47811/v4/browse/Browse.html`. You can type in it, hover, press ⌥/⌘, compare, drop a column, and add.

**Files** (under `Nudox-Design-System/v4/`; everything is `git add`-ed and nothing is committed):
- `browse/Browse.html`, `browse.css`, `browse.js`.
- `browse/build_data.py` → `data.json` (2.7 MB; runs offline in about 10 s). Add `--net` to refresh `cache/net.json`.
- `browse/snap.sh` takes one still; `browse/stills.sh` renders every board.
- `shots/browse/*.png`.

**Concept:** the one you approved, in `CONCEPT.md`. The query is the page, compare anchors on the incumbent, and the graph is only a door. All three of your later ideas are in: adoption preview, why-in-tree plus duplicates, and search by pasted code.

## URL (every still is reproducible)

**Find:**
- `?q=` accepts four kinds of query:
  - words: `parse toml`
  - a shape: `text -> Value`
  - a package you know: `like serde_json`
  - pasted code: `serde_json::from_str::<Config>(&s)?`

**Other views:**
- `?compare=a,b,c` opens compare.
- `&preview=<cand>` shows your code rewritten with that candidate.
- `&row=<n>` unfolds one row's signatures.
- `?view=tree` opens your tree.
- `&why=<crate>` shows its path from your code.

**Selection and state:**
- `?hover=<pkg>` selects a row or column.
- `?alt=1` holds ⌥; `?cmd=1` holds ⌘.
- `?w=` sets the window width; `?zoom=2` gives 200 % text; `?scroll=<id>` scrolls to an element.

**Motion, frozen at `t` ms:**
- `?add=<pkg>&t=` shows the adopt motion.
- `?to=<q>&t=` shows the FLIP re-rank.
- `?fly=1&t=` shows the ask field turning into the hero.

## Shots (`v4/shots/browse/`)

| Board | Files | What it shows |
|---|---|---|
| Find, empty | `find-empty-{1440,1100,760,480}` | The box teaches with four rows: a need, a shape, a package you know, a pasted line. |
| Find, words | `find-words-{1440,1100,760,480}`, `find-words-200pct` | **"You already have this."** `toml::from_str` is already called in 4 places, so it ranks first. Then basic-toml ("adds only itself") and toml_edit ("already in your tree"). Cousins follow: tomllib from the standard library comes first, plus 4 folded. |
| Find, "no package needed" | `find-std-1440` | `read a file into a string` → **std** `fs::read_to_string`, from real std sources. |
| Find, shape | `find-shape-{…}` | `text -> Value` → `toml::from_str::<Value>`. A generic `T: Deserialize` can *be* Value because toml's Value implements Deserialize (read from the scanned impls). |
| Find, like | `find-like-{…}` | `like serde_json` → kin ranked by **shared items**, not keywords ("shares from_str and to_string +19"). |
| Find, pasted code | `find-code-{1440,760}` | Your line rewritten per package, ranked by how little changes. The changed words are underlined. |
| Find, HTTP | `find-http-1440` | ureq is yours, so it answers first. reqwest "adds 10 crates"; attohttpc adds 3. |
| Judge | `judge-{1440,1100,760,480}`, `judge-200pct` | A card beside the row (≥ 1100 reader) or inline under it. It holds: why care (tree, cost or yours), the D-Marks comb *as* the version, one serif sentence, the answering signature, and "Switching from toml: covers one of the nine things you use (4 places of 80) · see your code ›". |
| Judge, ⌥ | `judge-alt-1440` | Churn by version numbers, measured where known, API size, MSRV, local dependents, RustSec status, and **downloads, the only place they appear**. Also two of your lines rewritten. |
| Judge, ⌘ | `judge-cmd-1440` | Key feet show only while ⌘ is held. |
| Judge, deprecated | `judge-yaml-1440` | serde_yaml: deprecated, as its own version string says, plus "adds 2 crates". |
| Compare | `compare-{1440,1100,760,480}`, `compare-200pct` | **The verdict sentence** is the hero: "toml_edit has an equivalent for all nine things your code does with toml; as_table and as_array read differently. basic-toml covers one of the nine." It has three bands: *what your code does with toml today* (real `uses`, empty where there's no equivalent), *what else it can do*, and *what it costs you*. |
| Compare, a row open | `compare-row-1440` | Hovering a row unfolds every cell's signature, e.g. "reads differently: (Item) → maybe Table". Hovering a column lifts it, with × to drop it. |
| Compare, costs and preview | `compare-costs-1440`, `compare-preview-{1440,760}` | "What it costs you": adds, license mark, breaking releases per year with "0 slips, measured", API size, MSRV, advisories. **"Your code, with basic-toml"**: 2 of your 77 lines change only in name, and 75 have no equivalent. Before and after are shown per file and line. |
| Compare, other families | `compare-http-1440`, `compare-errors-1440` | **ureq vs reqwest vs attohttpc**: reqwest covers 5 of the 17 things your code does with ureq (40 of 77 places); `Agent` maps to `blocking::Client`. **thiserror vs anyhow vs miette vs snafu**: none has an equivalent for `#[derive(Error)]`, and equivalence is kind-aware, so a derive never matches a struct of the same name. The capability rows carry that comparison. |
| Tree | `tree-{1440,1100,760,480}`, `tree-200pct` | "Your 44 packages lean on 79 others directly, and 1,189 in all." It flags **bincode 1.3.3 as unmaintained** (RUSTSEC-2025-0141, real) and notes 114 crates are here twice. Roles are derived. Transitive crates are a quiet "and N that come with them". |
| Tree, hover | `tree-hover-1440` | The judge card, plus "In *talks to the network* because web-programming::http-client · used by …". |
| Tree, why and twice | `tree-why-1440` | toml: `desktop → toml 0.8.23` and `frontend-rust → ra_ap_project_model → toml 1.1.5`. "Moving yours to 1.1.5 drops a copy · 4 of your 80 uses touch from_str, whose signature only gained a lifetime." Measured from releases.json. |
| Adopt | `adopt-lift-1440` (t=300), `adopt-{1440,1100,760,480}` (t=700), `adopt-index-1440` (t=2300) | **Lift:** the gem arcs off the card. **Drop:** the shelf row opens, the gem lands with squash and bounce, and the name reveals by clip. The Library and status count roll like an odometer (1,189 → 1,190). **Indexing:** a slim four-stage seam (fetch · unpack · read · seal) runs under the new row, and the card reads "added · reading 20 public items 3 of 4". At 760 the target is the spine; at 480 it is the status count. |
| Hand | `find-hand-1440` | ⌘-click or C gathers candidates at the window foot ("compare three ›"). This is §2's *in hand*. |
| Motion | `flip-1440` (t=55), `fly-1440` (t=70) | FLIP re-rank from `parse` to `parse toml`: rows travel and nothing blinks. The titlebar ask field turns into the hero; results rise 4 px in rank order and the card unfurls by clip. |

## Real vs unknown

**Real, from this machine:**
- **Packages.** 33 crates in 9 families, plus a std subset.
  - The std subset is 750 items from `rust-src` in `/nix/store`: fs, env, thread, mpsc, time, process, net, io, str, string, collections and num.
  - The 7,746 public items come from our own scan of the unpacked sources. It follows `pub use`, globs, crate-level re-exports (clap → clap_builder, smol → async-*) and proc-macro derives (thiserror → `#[derive(Error)]`).
  - The API is read at the latest unpacked version. Five crates differ from the registry's latest (bincode 2.0.1 vs 3.0.0, fancy-regex, ureq, attohttpc, pest); the card says "read at X".
- **Cost.**
  - Dependency closures are resolved with default features and cfg evaluated for aarch64-apple-darwin, from the registry index cache.
  - They are compared with Cargo.lock by semver bucket, so a "second copy" is flagged.
  - Zero deps were unresolved.
- **Your tree.** Taken from Cargo.lock: 1,189 external crates, 79 direct, your pins, 114 duplicates, and why-paths found by BFS from your packages.
- **Roles.** Derived by voting over each dependency's crates.io categories and keywords, and a telling description, × which of your packages use it. The evidence is recorded per dep (`roleWhy`). Six with no evidence land in **other** (turso, trustfall, trustfall_core, derive_more, fearless_simd, opentelemetry_sdk).
- **Stability, as claimed.** Every version's `pubtime` and yanked flag from the index gives breaking releases per year (last 3 y) and the last breaking release.
- **Stability, as measured.** Only toml and smallvec have real API diffs (`graph/releases.json`); there are 0 semver slips in the measured pairs.
- **Advisories.** RustSec `advisory-db` via `gh api` (read-only), cached in `cache/net.json`. That covers the 33 candidates plus your direct deps, 92 of 1,189 lockfile crates in all. **bincode 1.3.3 in your tree is marked unmaintained.**
- **Downloads.** crates.io API, fetched 2026-09-26, shown only under ⌥.
- **Your uses.**
  - toml and smallvec use the index-resolved uses from releases.json.
  - Every other direct dependency uses **text search**: qualified paths plus imported names, with comments and strings stripped, and local shadowing skipped. The source is labelled.

**Approximate, and labelled as such:**
- **Equivalence** across candidates is matched by name *within the same kind*: the same function, the same method on the value type, or the family's main type (SmallVec ↔ ArrayVec ↔ TinyVec, Agent ↔ Client ↔ Session). A derive only answers a derive.
- "**Reads differently**" means the shapes-in-words differ.
- **Rewrites** are textual: "matched by name; review before you switch".
- **Capability rows** are detectors over the API (name and kind patterns). For fancy-regex, "look-around" comes from its own docs.

**Unknown, and shown as unknown:**
- Measured churn for 31 of 33 packages ("API diffs not measured yet").
- MSRV where it isn't declared.
- Advisories for the ~1,100 lockfile crates not fetched ("advisories checked for 92 of 1,189").

**Hand-written, and marked in data.json:**
- The **cousins**: npm, PyPI, Go, Java, .NET and C++ counterparts for 7 families.
- Nothing from those ecosystems is indexed on this machine.

## What the implementation lane needs

**From the index (backend):**
1. **Public API per package, with display paths.** The index already knows items. Browse also needs:
   - shortest public path after re-exports;
   - first doc sentence;
   - **shape in words** (`in: [text], out: "your type or fails"`, bound-aware: `T: Deserialize` → *your type*, `R: Read` → *reader*);
   - trait-impl facts (which types implement Deserialize, FromStr, Display).
2. **Resolved uses of every direct dependency** as `(item path, file, line, line text)`. world.json already has `uses` and `calls` edges into serde_json, toml and smallvec, so this should replace my text search for all deps.
3. **Cost against the lockfile.** Closure resolution with features and target cfg, diffed with Cargo.lock by semver bucket, plus "second copy" detection.
4. **Tree facts:** your pins, direct deps per member with kind, why-paths, duplicates, and role evidence (categories × members).
5. **Release data:**
   - the version list with pubtime and yanked;
   - API diffs per adjacent pair, run across *all* candidates (releases.mjs today covers only toml and smallvec);
   - `semverSlip`.
6. **Advisories.** `crates/advisory` already parses RustSec. Browse needs "affects version X" per package and a whole-tree health line.
7. **Other ecosystems' APIs**, for real cousins. Until then the table stays hand-written.

**In `apps/facet` today (reuse):**
- `motion::flow`: FLIP on layout epochs, for the re-rank and for dropping a column.
- `motion::shared`: keyed element morph, for ask field → hero and card gem → shelf row.
- `data::progress`: the seam and gem progress, which *is* the four-stage indexing indicator.
- `controls::comb`: VersionComb; the card uses D-Marks' `versionComb`, which grows from it.
- `overlay::peek`: the judge card is a taller peek.
- `data::door`: links with rested hover.
- `chrome::shelf` and `chrome::titlebar`.
- `controls::field`: the query.
- `paint::gem`.

**New:**
- `FindPage`: hero query, reading line, verdict block, answer rows, cousins fold, graph door.
- `AnswerRow`: mark · name · answering item · one reason.
- `JudgeCard`: care line, comb, sentence, signature, switch line, ⌥ x-ray, ⌘ key foot.
- `CapabilityLedger`: bands, row unfold, column lift, drop with FLIP, "+ a fourth".
- `AdoptionPreview`: before and after line pairs, a candidate picker, "no equivalent for X".
- `LibraryPage`: roles in two columns, transitive fold, why-line, "Here twice" with the upgrade sentence.
- The **adopt drop**: an arc flight with squash and bounce, plus make-room. `motion::shared` glides straight, so the arc and bounce are new.
- An **Odometer** for counts. D-Marks has rolling version digits; facet needs a generic one.
- The **Hand** (gathered candidates at the window foot) is §2's "in hand", so the owning lane should build it.

**Search** needs ranking logic in the backend, specified in `browse.js`:
- words, via synonyms, nouns and coverage;
- noun exclusion, so `parse toml` never answers `str::parse`;
- a shape matcher, including the Deserialize-generic rule;
- like-X by shared-item overlap;
- code paste ranked by change size;
- the verdict rule: std or yours within 72 % of the top score leads.

## Requests to D-Marks
- `versionComb` in compare headers at 200–220 px gets dense for 120+ versions. A compact variant (e.g. `opts.width < 240` → band) would help.
- `ecosystemMark` rows are all the same stone in the tree view. That's fine, but a `quiet` option (no hover card for rows whose ecosystem is implied) would cut hover noise there.

## Open questions
1. **Where Find lives.** Is it its own reader route (`nudox://find?q=`), with ⌘K Ask offering "all answers as a page ↵"? Or does Ask grow into it? The prototype assumes the route.
2. **What "add" writes.** Which of your 44 packages gets the dependency? I'd default to the member you're reading, or the role's main user, and offer a menu. And do we run `cargo add` or write the workspace table?
3. **The tree count** (1,189) includes Windows- and Linux-only crates in the lockfile. Should it count only what builds for this host?
4. **Measured churn for every candidate** means running the release diff across every version pair; toml alone has 120 versions. Do we cap it at the last N releases, or measure lazily on hover?
5. **Role vocabulary.** Twelve derived roles plus "other". Should the owner be able to rename or merge them per project?

## Summary
1. `Browse.html` is a working, integrated browsing prototype in the house frame (titlebar ask, Library shelf, reader, status count). It is built on real data from this machine by a re-runnable script.
2. **Find:** the query is the hero, and it reads words, shapes, "like X" and pasted code. Rows are mark · name · the answering item · one reason, and they re-rank with FLIP.
3. **"You need no package"** comes first when true: std's `fs::read_to_string`, or toml's `from_str`, which you already call 4 times.
4. **Judge:** why care (in your tree, marginal cost, deprecated or unmaintained), the comb as the version, one sentence, the signature, and a one-line "switching from what you use". Depth waits under ⌥, and downloads appear only there.
5. **Compare** anchors on the incumbent. Rows are what your code does today (real uses), then what else it can do, then what it costs you. Cells are items; missing ones are honest empty space.
6. **Adoption preview:** your own 77 toml lines rewritten for basic-toml (2 change only in name, 75 have no equivalent) or for toml_edit (all 77 map).
7. **Your tree** is grouped by derived role, with the evidence on hover, plus why-paths and duplicates. For example, "Moving toml to 1.1.5 drops a copy; only from_str gained a lifetime."
8. **Real finds in this codebase:** bincode 1.3.3 is unmaintained (RustSec), 114 crates are duplicated, and reqwest would add 10 crates where attohttpc adds 3.
9. **Adopt:** the gem arcs off the card and drops into its role with a bounce, the list makes room, the count rolls, and a slim four-stage seam indexes. Every frame can be frozen with `?t=`.
10. **Still approximate:** name-matched equivalence and rewrites, measured churn for only 2 of 33 packages, and hand-written cousins. The implementation lane needs resolved uses, shapes, and diffs from the index.
