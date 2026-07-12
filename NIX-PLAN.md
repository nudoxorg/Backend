# NIX-PLAN — Nix as a sixth producer language

Research date: 2026-07-08. Sources: live FlakeHub API probing, snix rustdoc (built
2026-05-18), cachix/snix `canon`, nixdoc v3.1.0, noogle (pasta/pesto), RFC 145,
CppNix 2.24 `:doc`, and a full map of `workspace/compiler`.

## 0. Goal and positioning

Add Nix as a producer: FlakeHub-registry acquisition → hermetic evaluation +
static analysis → max-resolution IR → existing graph/emit pipeline, plus a Nix
render backend. The bar is "exceed every existing Nix doc tool", and the research
shows exactly where each one stops:

| Capability | nix-doc | nixdoc | noogle (pasta/pesto) | flake-info | **us** |
|---|---|---|---|---|---|
| Doc comments (RFC 145 + legacy) | partial | yes | yes | no | yes |
| Evaluation-backed (aliases, real structure) | no | no | yes (patched Nix) | eval, no docs | yes (in-process snix, unpatched) |
| Lambda formals w/ defaults + required flags | no | partial | no | no | yes (rnix defaults + eval required-flags) |
| Parsed type signatures (`::` grammar → type AST) | no | raw string | raw string | no | **yes — first ever** |
| Builtins with docs | no | no | yes | no | yes (free via `Builtin::documentation()`) |
| Packages (meta.*) | no | no | no | yes | yes |
| NixOS options (type/default/example/declared-in) | no | no | no | yes | yes |
| Flake outputs inventory (devShells, templates, overlays…) | no | no | no | partial | yes |
| One unified model | — | — | — | — | yes (our IR) |

Nothing existing is worth vendoring: nix-doc is dead (2022, rnix 0.8, LGPL),
nixdoc and snix are GPL-3.0 (algorithms fine, code-porting not), pesto's tricks
are ~200 lines we can do better in-process. We vendor **snix** (the evaluator)
and clean-room the rest.

## 1. Key research facts (what the plan is built on)

### snix / tvix
- tvix was forked to **snix** 2025-03-16; snix is where development happens
  (canon @ 2026-07-06). Use snix, not tvix. Sources: `git.snix.dev/snix/snix`
  (behind Anubis anti-bot — fetch archives via the **github.com/cachix/snix**
  mirror). **GPL-3.0-only.** Never published to crates.io (`snix-eval 0.0.0-pre`
  is a name reservation) → git vendoring required, which matches our Buck flow.
- `snix-eval` API (verified): `Evaluation::builder(Rc<dyn EvalIO>)` /
  `builder_pure()` → `.mode(EvalMode::Strict)`, `.disable_import()` /
  `.enable_import()`, `.nix_path(..)`, `.env(..)`, `.with_globals(..)`,
  `.build()`. `evaluate(code, location) -> EvaluationResult { value:
  Option<Value>, errors, warnings, expr: Option<rnix::ast::Expr> }` — **the rnix
  AST is handed back with every evaluation**, which is the whole fusion story.
  Results are `!Send` (single-threaded eval; parallelize per-package).
- `Value` (17 variants): `Null/Bool/Integer/Float/String(NixString)/Path/
  Attrs(NixAttrs)/List(NixList)/Closure(Rc<Closure>)/Builtin(Builtin)/Thunk(..)`
  + internal leakage (`Catchable`, `Blueprint`, …). `EvalMode::Strict`
  deep-forces the top-level result (calling `force()` from outside the VM is
  awkward — it lives inside the genawaiter machinery).
- `Closure → Rc<Lambda>`; `Lambda { chunk, name: Option<SmolStr>, formals:
  Option<Formals>, param_name }`; `Formals { arguments: BTreeMap<NixString,
  bool /*required*/>, ellipsis: bool, span: Span, name: Option<String> /*args@*/ }`;
  `Chunk::first_span()/get_span(CodeIdx) -> codemap::Span`; `SourceCode` wraps a
  `codemap::CodeMap` (span → file/line/col). **All these fields are
  `pub(crate)`** → we carry a small visibility patch (precedent:
  `build/third-party/patches/deno-doc/`).
- Formals carry only a required-flag, **not default expressions** — defaults come
  from the static layer (rnix `PatEntry` has the default expr).
- Doc comments are NOT in the evaluator (no `Lambda.documentation`) — we extract
  them ourselves from the AST (see §5).
- Builtins: `pure_builtins()` yields `Builtin`s with `.name()` and
  `.documentation() -> Option<&'static str>` (docs collected from Rust `///` by
  the `#[builtins]` macro in `snix-eval-builtin-macros`) — a free, high-quality
  builtins reference.
- `EvalIO` trait (5 methods: `path_exists/open/file_type/read_dir/import_path`)
  is the hermeticity boundary; `DummyIO` denies everything, we implement a
  whitelist-FS over the materialized source tree + inputs.
- Skip `snix-glue`/castore/store entirely (derivation building, protobuf build
  deps) — pure eval is enough for docs.
- Pins to respect: `rnix 0.11.0`, `rowan 0.15`, `codemap 0.1.3`, `smol_str 0.2.2`.

### Doc-comment ecosystem
- **RFC 145**: `/** CommonMark */` attaches to the immediately following
  documentable node (binding > lambda; only whitespace/non-doc comments may
  intervene). nixdoc's sections convention: `# Arguments`, `# Type` (fenced code
  block), `# Examples` (`:::{.example}` divs).
- **Legacy convention** (most of the ecosystem still): `/* Description …
  Type: f :: a -> b   Example: … */` + `# argname` line comments on formals.
- **Extraction algorithm** (nix-doc/nixdoc/pesto all agree, and CppNix 2.24 does
  the same via its `positionToDocComment` map): from a node, walk
  `prev_sibling_or_token()` skipping whitespace, take the first comment token;
  fall back to the parent `AttrpathValue`. ~50 lines over rowan; clean-room it.
- **Alias unification** (noogle's key trick): entries whose lambdas share a
  source position are the same function → `lib.mapAttrs` ≡
  `lib.attrsets.mapAttrs`. Pasta needs a patched Nix (`builtins.lambdaMeta`);
  in-process we get it directly from `closure.lambda.chunk.first_span()`.
- **The `::` type-signature convention has no parser anywhere** — nixdoc stores
  `fn_type: Option<String>`, pesto captures the code block verbatim. Grammar
  (recoverable by recursive descent):
  `sig ::= ident ' :: ' type; type ::= atom (' -> ' type)?; atom ::= '(' type ')'
  | '[' type ']' | '{' fields '}' | typevar | typename`, vocabulary
  `String/Int/Float/Bool/Null/Path/Any/AttrSet/Derivation/Module/Option a`,
  lowercase type variables, records `{ name :: T; ... }` / `{ ... :: T }`,
  nullable written both `T?` and `Null | T`.

### FlakeHub (all verified live 2026-07-08, anonymous access)
- `GET api.flakehub.com/flakes` → **all** public flakes in one response (865
  today; small — fallback matters). `GET /f/{org}/{project}` → description,
  rendered readme, `spdx_identifier`, `repo_url`, labels, and an **evaluated
  flake-schema `outputs` tree** (shallow for huge flakes).
- `GET /version/{org}/{project}/{constraint}` → single best release with
  `download_url`, `revision`, `commit_count`, `yanked_at` — the resolver
  primitive. `GET /f/{org}/{project}/releases?offset=N` paginates at a fixed 50.
- Tarball: `GET /f/{org}/{project}/{constraint}.tar.gz` → 307 → pinned URL
  (comes back in a `Link: rel="immutable"` header — store this) → 307 →
  time-limited CloudFront-signed URL → gzip. Plain HTTP, no auth, no Nix client.
  **No NAR hash served** — SHA-256 the bytes ourselves.
- Versions are Cargo-semver `X.Y.Z+rev-{sha}`; nixpkgs scheme `0.{YYMM}.{commit-count}`
  (`0.2411.*` = nixos-24.11; `0.1.*` = unstable). Mirrored flakes (nixpkgs!)
  report `repo_url = DeterminateSystems/flakehub-mirror` — remap to real upstream.
- Fallbacks beyond FlakeHub: `github:owner/repo` tarballs
  (`github.com/{o}/{r}/archive/{rev}.tar.gz`) via our existing gix `vcs.rs`
  machinery, and the official registry
  (`channels.nixos.org/flake-registry.json`) for indirect names.

### Our extension points (verified against the tree; mid-migration caveat: the
directory is `workspace/compiler/compile/` while `lib.rs` publishes it as
`languages` — follow whatever naming holds when implementation starts)
- Producers: `compile/{python,rust,typescript,go,java}` + shared `vcs.rs`.
  Oracle pattern = Go/Java (subprocess → one JSON → serde mirror → lower);
  in-process = Python (pyrefly), TS (deno_doc).
- IR: `Entry` (Module/RecordType/UnionType/TraitDef/SumType/Function/TypeAlias/
  Constant/Variable/Field/…), `Symbol<T> { name, path: NudoxPath, aliases:
  Option<HashSet<Vec<String>>>, visibility, documentation: Option<String>, inner }`,
  `Function { input_parameters, output_parameters, type_links, attributes,
  generics, overloads, … }`, `Type` (26 variants incl. `FunctionPointer`,
  `RecordLiteral`, `Slice`, `Union`, `GenericParam`, `Any`, `Variadic`).
- Enums to extend in `workspace/heart/`: `Language` (ecosystem.rs),
  `Toolchain`, `PackageVersion`, `RegistryOrigin` (identity/package.rs).
- Dispatch: `generate/surface.rs::collect` match; `generate/cst.rs` +
  `treesitter.rs` (arborium grammars); `error.rs::GenerateError`.
- Render: `render/backend.rs` `Backend` trait (doc_comment/ty/record/sum/
  function/interface → `Doc<Annotation>`), zero-sized backends in `render/emit/`.
- Build: Buck2-only. crates.io deps via `tools/add-crate.py` → `registry.bzl`;
  git deps via `git.bzl` `GIT` list (archive + per-crate `subdir`/`deps`/optional
  `patch`); patches in `build/third-party/patches/<name>/`.

## 2. Architecture decision

**In-process, hybrid static+dynamic.** A `compile/nix/` producer that links
vendored `snix-eval` directly (like Python/pyrefly and TS/deno_doc — and
consistent with the rustdoc-in-process direction), running two cooperating
layers over the same `SourceCode`:

1. **Static layer** — parse every `.nix` file with rnix (the same 0.11 snix
   pins). Yields: binding tree (attrpath → expr), doc comments (RFC 145 +
   legacy), lambda formals **with default expressions**, `args@`/ellipsis,
   spans. This layer alone already exceeds nixdoc and works with zero
   evaluation (no inputs needed).
2. **Dynamic layer** — hermetically evaluate the flake's outputs with snix-eval.
   Yields: the *real* output tree (through re-exports, `//` merges,
   `callPackage`, functors), alias groups by shared lambda span, required-flags
   for formals, package `meta`, options, builtins.
3. **Fusion** — runtime `Closure` → `lambda.chunk.first_span()` → `SourceCode`
   file+offset → rnix node at that byte range → static record (doc comment,
   defaults, signature). This is pesto's pipeline collapsed into one process
   with no patched Nix and no JSON hop.

Why not the Go/Java oracle-subprocess pattern: there is no external tool that
produces anything close (that's the point of this plan), and the evaluator is
Rust — a subprocess would only add a JSON round-trip the repo is actively
removing elsewhere.

Two consequences to manage, not avoid:
- **License**: snix-eval is GPL-3.0-only, linked into the compiler binary. For a
  server-side binary that is never conveyed, GPL imposes no obligations; it
  forecloses *distributing* the combined binary under a permissive license
  later. Decision to confirm with the user before Phase 0; the escape hatch (an
  isolated GPL `nix-oracle` subprocess speaking the Go/Java JSON convention) is
  a refactor of the entry point, not of the producer, since all lowering stays
  in our code either way.
- **Isolation**: evaluating arbitrary Nix can loop/OOM (nix-eval-jobs exists
  because of this). `!Send` results already force per-package eval threads;
  give each eval a wall-clock timeout + `catch_unwind`, wrap every risky forcing
  in `builtins.tryEval` on the Nix side, and keep "worker subprocess with
  memory-limit restarts" as the Phase 7 hardening move if real-world flakes
  demand it.

## 3. Phase 0 — Vendor snix (build-system groundwork)

1. `build/third-party/git.bzl`: one `GIT` archive entry for snix pinned to a
   `canon` rev, fetched from the cachix/snix GitHub mirror (git.snix.dev is
   behind Anubis): `{ archive_name: "snix", urls: [github archive tarball],
   sha256, strip_prefix, crates: [snix-eval (subdir eval), snix-eval-builtin-macros
   (subdir eval/builtin-macros, proc_macro)] }`. Do NOT pull glue/castore/store
   (protobuf toolchain, irrelevant).
2. Enumerate snix-eval's transitive crates.io deps with the `/tmp` scratch-
   workspace trick (cargo metadata on a throwaway Cargo.toml, as done for
   gix/deno) and `add-crate.py` them into `registry.bzl`. Expected additions:
   `rnix 0.11.0`, `rowan 0.15`, `codemap 0.1.3`, `genawaiter`, `imbl` (or
   whatever snix pins for persistent maps), `smol_str 0.2.2` (likely already
   present via other deps), `lexical-core`/`data-encoding`/`dirs` as pulled.
3. `build/third-party/patches/snix/snix-eval-<rev>.patch`: widen visibility —
   `Lambda.{name,formals,param_name}`, `Formals.{arguments,ellipsis,span,name}`,
   `Closure.lambda`, `Chunk::{first_span,get_span}` already pub — plus a
   `SourceCode` accessor for file/line resolution if missing. Keep the patch
   additive (new `pub fn` accessors preferred over changing field visibility)
   so rebases across snix bumps stay trivial.
4. Smoke target: a unit test that evaluates `builtins.mapAttrs (_: v: v + 1)
   { a = 1; }` with `builder_pure()` + `EvalMode::Strict` and asserts on the
   `Value::Attrs`, plus one that reads `EvaluationResult.expr` spans. Exit
   criterion: `buck2 build //workspace/compiler:compiler` with snix linked.

## 4. Phase 1 — Plumbing (heart + dispatch skeleton)

- `workspace/heart/ecosystem.rs`: add `Language::Nix` (strum lowercase →
  `"nix"`); `Toolchain::Nix { evaluator: Version }` (the snix rev/version).
- `workspace/heart/identity/package.rs`: `PackageVersion::Nix(semver::Version)`
  — FlakeHub is Cargo-semver, keep the `+rev-{sha}` build metadata (semver
  crate parses/preserves it; remember it's ignored in ordering, which is fine
  because FlakeHub patch numbers are monotonic commit counts).
  `RegistryOrigin::FlakeHub` (a first-class variant, not `Custom` — it has
  bespoke resolution semantics).
- Update every exhaustive `match` the compiler errors on (`From<&Toolchain>`,
  `TryFrom<(Language, &str)>`, etc.).
- `compile/mod.rs`: `pub mod nix;`. New module skeleton
  `compile/nix/{mod.rs, error.rs, context.rs, syntax.rs, docs.rs, sig.rs,
  eval.rs, walker.rs, options.rs, builtins.rs, item.rs, types.rs, function.rs,
  package.rs, traversal.rs}` (roles in §5–§9).
- `generate/surface.rs`: `Language::Nix => { let index =
  languages::nix::lower_package(&input.root)?; Ok(Ir::from_entries(...)) }` +
  the `PackageVersion::Nix` arm in the version match.
- `generate/cst.rs` + `treesitter.rs`: check whether `arborium-nix` exists (the
  registry already uses the `arborium-<lang>` per-grammar pattern via
  `arborium-rust`); if yes, wire `arborium::get_language("nix")` for CST
  extraction, else `Language::Nix => CstSet::default()` for now.
- `error.rs`: `GenerateError::LowerNix(#[from] nix::NixError)`.

Exit criterion: `surface::collect` on a fixture flake returns an (empty) Index.

## 5. Phase 2 — Static producer (already beats nixdoc at exit)

`syntax.rs` — parse each `.nix` file once (`rnix::Root::parse`), build a
per-file table: every `AttrpathValue` binding and every `Lambda`, keyed by byte
span, with:
- attrpath (as `Vec<String>`, handling dynamic attrs as opaque),
- for lambdas: `Param::IdentParam` name, or `Pattern` entries — **name, default
  expression (pretty-printed source slice), ellipsis, `args@` bind** — and the
  full curried chain (successive `Lambda` bodies) flattened into an ordered
  parameter list with a `curried` attribute.

`docs.rs` — clean-room comment extraction (algorithm shared by nix-doc/nixdoc/
pesto/CppNix; do not port GPL code):
- backward `prev_sibling_or_token()` scan (skip whitespace and non-doc
  comments), parent-`AttrpathValue` fallback, binding-beats-lambda precedence
  per RFC 145;
- RFC 145 `/**` bodies: dedent, then split nixdoc-convention sections
  (`# Arguments` → per-param docs, `# Type` → first fenced code block,
  `# Examples`); everything recombines into markdown for
  `Symbol.documentation`, with the Type block routed to `sig.rs`;
- legacy `/* … Type: … Example: … */` state machine + `# argname` formal
  comments;
- `#`-run line comments immediately above a binding as a last-resort doc.

`sig.rs` — **the `::` type-signature parser** (first of its kind; the moat):
recursive descent over the §1 grammar → `ir::ty::Type`:
- `->` chains → `FunctionPointer` (right-assoc),
- `[T]` → `Slice`, `(T)` → grouping, tuples don't exist in the convention,
- `{ name :: T; ... }` / `{ ... :: T }` → `RecordLiteral` (open records get a
  rest marker via `Variadic`-typed field or record attribute),
- bare lowercase idents → `GenericParam`, known names → `Primitive`/
  `TypeReference` (`String/Int/Float/Bool/Null/Path/Any/AttrSet/Derivation/
  Module/Option a`),
- `A | B` and `T?` → `Union` (normalize `T?` to `Union[T, Null]`).
Parse failures degrade to leaving the raw string in documentation only — never
block ingest on the informal grammar.

`item.rs`/`function.rs`/`types.rs` — lower the static table to IR (mapping in
§10) for the no-eval path.

Exit criterion: fixture with RFC-145 + legacy + curried + pattern-default
functions round-trips to IR with parsed signatures; parity test against
nixdoc's JSON output on a copy of `nixpkgs/lib/attrsets.nix` (we must extract a
superset).

## 6. Phase 3 — FlakeHub traversal (`traversal.rs`)

Mirror the Python PyPI shape (`resolve_and_fetch_sdist`):
- `resolve(org, project, constraint) -> Release`: `GET
  /version/{org}/{project}/{constraint}` (percent-encode segments — `*` →
  `%2A`); surface `download_url`, `version`, `revision`, `yanked_at` (reject
  yanked unless pinned exactly).
- `fetch(release, workspace) -> PathBuf`: follow the redirect chain with
  redirects enabled, stream to disk, SHA-256 the bytes (no NAR hash exists),
  record the `Link rel="immutable"` pinned URL for provenance, extract
  (`flate2` + `tar`, both already vendored), honor `source_subdirectory`.
- `enumerate() -> Vec<FlakeId>`: `GET /flakes` (one call, ~865 entries) for the
  ingest loop; `GET /f/{org}/{project}` for description/readme/license/
  repo-url/labels — feed `spdx_identifier` and description straight into
  package metadata, and keep the evaluated `outputs` schema tree as a
  cross-check for our own evaluation (never as the source of truth — it's
  shallow on big flakes). Remap `mirrored: true` repo URLs
  (flakehub-mirror → real upstream, e.g. NixOS/nixpkgs).
- Fallbacks, in order: (1) FlakeHub; (2) `github:owner/repo` flakerefs —
  archive tarball by rev, or reuse `compile/vcs.rs`
  (`open_or_clone_repository` + `find_commit_for_version` keyed on git tags);
  (3) indirect names via `channels.nixos.org/flake-registry.json` (~50 curated
  mappings). Non-flake Nix repos (no `flake.nix`) still get the static layer.
- Be polite: no documented rate limits, no API versioning — treat `fh` CLI
  response shapes as the de-facto contract and keep the serde mirrors
  `#[serde(default)]`-heavy like the Go oracle mirror.

**Input materialization** (the genuinely hard sub-problem): evaluating outputs
requires the flake's inputs. Parse `flake.lock` (plain JSON, locked node graph
with `narHash`/`rev`/`lastModified`): materialize each locked input —
FlakeHub-hosted when possible, else GitHub archive by locked `rev` — into
`workspace/inputs/<name>/`, recursively (the lock is already a full closure;
bounded by its node count). No lock file → static-only mode + warning. Cache
materialized inputs content-addressed by locked `narHash`/rev so nixpkgs is
fetched once, not per-flake.

Exit criterion: `nixos/nixpkgs/0.2505.*` and a small tagged flake resolve,
fetch, verify, and extract offline-reproducibly (record fixtures for tests).

## 7. Phase 4 — Evaluation layer (the "deeper introspection")

`eval.rs`:
- `DocsIO: EvalIO` — whitelist filesystem serving exactly: the extracted flake
  tree + materialized `inputs/` (path-rewritten), read-only, no absolute-path
  escape, `import_path` = identity/no-op (no store). Keep `enable_import()` on
  — `import` is how flakes are structured — hermeticity comes from `DocsIO`,
  not from `disable_import()`.
- **Flake-outputs shim** (what pasta is for noogle, but ours handles inputs):
  a small embedded Nix expression, flake-compat-style fixed point:
  `let flake = import <root>/flake.nix; inputs = { nixpkgs = import-shim …; };
  self = flake.outputs (inputs // { inherit self; }); in self` — with each
  materialized input exposed as `{ outPath, rev, narHash, lastModified, … } //
  outputs` the way real flake inputs look. Port the *semantics* of
  edolstra/flake-compat (it's the reference for this trick), not the code.
- One `Evaluation` per package: `builder(Rc<DocsIO>)`, `EvalMode::Lazy`
  (Strict would force all of nixpkgs; we force selectively), shared
  `SourceCode` kept for span resolution, wall-clock timeout wrapping the whole
  producer call.

`walker.rs` — the traversal that replaces pasta's `genericClosure`, but in Rust
against `Value` directly:
- walk `outputs.{packages,legacyPackages,devShells,apps,checks,formatter,
  overlays,nixosModules,homeModules,templates,lib,…}.<system>.…` with per-node
  forcing wrapped so catchables/eval errors degrade to a documented
  "unevaluable" leaf, never abort the package;
- stop-descend heuristics: attrset with `type = "derivation"` → package leaf
  (harvest `meta.description/longDescription/mainProgram/license/maintainers/
  platforms/homepage/position` — same field set flake-info proved out);
  `_type = "option"` → option leaf (§8); `recurseForDerivations` respected;
  depth + node-count budgets (nixpkgs `legacyPackages` is 120k nodes — gate
  full-nixpkgs enumeration behind config, default to declared `packages` +
  `lib` + modules);
- `Value::Closure` leaf → `lambda.chunk.first_span()` → **fusion** with the
  static table (doc comment, defaults, declared signature) + runtime facts
  (required flags, ellipsis, `args@`, functor/partial detection via applied
  count where recoverable);
- **alias pass**: group every reachable closure by (file, span); each group =
  one canonical `Symbol` whose `aliases: HashSet<Vec<String>>` gets every
  attrpath that reached it (noogle's shared-position trick, no patched Nix);
- record every reached attrpath's *position* (snix implements
  `unsafeGetAttrPos`-equivalent span data on bindings; where absent, fall back
  to the static binding table) so `declared_in` is populated for everything.

`builtins.rs` — synthesize a standing `nix-builtins` package (one Module, one
`Entry::Function` per builtin) from `pure_builtins()`: `.name()`,
`.documentation()`, arity → parameters. Ships in Phase 4 for free and is
immediately the best builtins reference available anywhere.

Exit criteria: a fixture flake with re-exports produces unified alias sets;
`nixpkgs.lib` (via the flake) yields ≥ noogle's function count for `lib.*`,
each with position, docs where they exist, and parsed signatures where declared.

## 8. Phase 5 — NixOS module & options extraction (`options.rs`)

The single highest-value target for real users, and no doc-search tool unifies
it with functions:
- for each `nixosModules.*`/`homeModules.*` output, evaluate
  `lib.evalModules { modules = [ the-module {options/config stubs} ]; }` using
  the materialized nixpkgs input's `lib` (the shim already provides it); walk
  the resulting option tree exactly like `lib.nixosOptionsDoc` does;
- per option: name path, `type.description` (and structured lowering of common
  types: `types.bool/str/int/enum/listOf/attrsOf/nullOr/submodule` → our
  `Type` — enums → `Union` of literal `TypeReference`s, `nullOr` → `Union` with
  Null, `submodule` → nested `RecordType`), `default`+`defaultText`
  (`literalExpression` unwrapped), `example`, `description` (CommonMark),
  `readOnly`, `visible`, **declaration positions** (module system tracks
  `declarations` — file paths);
- lower each option to `Entry::Field`/`Variable` under a `RecordType` per
  module, documentation = description + rendered default/example blocks;
- guard rails: options whose defaults force packages get `defaultText` only
  (never force `default` thunks blindly — wrap in `tryEval`, budget-limited).

Exit criterion: a fixture module produces options.json-equivalent (validated
against `nixosOptionsDoc` output on the same module) with structured types.

## 9. IR mapping (`item.rs` / `types.rs` / `function.rs`)

| Nix construct | IR |
|---|---|
| Flake | root `Entry::Module`; documentation = flake `description` + readme summary |
| Output category (`packages`, `devShells`, `lib`, …) | nested `Module` (per-system collapsed: identical-across-systems members deduped, `for_systems` recorded as an attribute) |
| Namespace attrset (e.g. `lib.attrsets`) | `Module` with `members` |
| Lambda (incl. curried chain) | `Entry::Function`: ordered `input_parameters` (name, type from declared sig ∩ observed, `default_value` from rnix, required flag from eval, ellipsis → trailing `Variadic` param), `output_parameters` from sig tail; `curried`/`args@` as `attributes`; partial applications (`countApplied>0`) noted in attributes |
| Structured data attrset | `RecordType` (fields typed by observed value types) |
| Derivation | `Entry::Constant` typed `TypeReference("Derivation")` via alias; documentation = meta sections (description, longDescription, mainProgram, license, platforms, maintainers) in deterministic markdown |
| NixOS option | `Field` under module `RecordType` (§8) |
| NixOS module | `RecordType` + `Module` hybrid: options record + docs |
| Builtin | `Function` in synthetic `nix-builtins` package |
| Alias/re-export | canonical Symbol + `Symbol.aliases` (never duplicate entries) |
| Type variables in sigs | `GenericParam` (scoped per signature) |
| overlays / templates / apps / checks | `Module` leaves with docs (template `description`+`welcomeText`; app `program`) |

`NudoxPath`: attrpath components as path segments
(`Local("lib/attrsets/mapAttrs")` following the producer convention); symbols
in *inputs* → `NudoxPath::External { dependency: input-flake-id }` — giving
cross-flake `type_links` (e.g. a devShell referencing `nixpkgs#mkShell`) for
free through the existing graph emit. Visibility: everything reachable from
outputs = `Public`; static-only bindings not reachable from outputs =
`Private` (this distinction is itself beyond every existing tool).

## 10. Phase 6 — Render backend (`render/emit/nix.rs`)

Add `Nix` to the render-side `Language` enum + `backend()` dispatch; zero-sized
`pub struct Nix;` implementing `Backend`:
- `doc_comment` → RFC-145 `/** … */` block;
- `function` → `name = { a, b ? default, ... }: …` for record-pattern params,
  `name = a: b: …` for curried; annotate with a `# Type` sig rendered from the
  IR type (round-tripping our parsed signatures back to the convention);
- `record` → attrset literal with `field = <type>;` pseudo-typed comments;
- `sum` → no native construct: render as commented `Union` contract (`# one of:
  …`), matching how the Java backend documents unrepresentables in sections;
- `interface` → attrset contract with required attr names (module-style);
- `ty` → the `::` convention (arrows right-assoc, `[T]`, `{ f :: T; … }`) — the
  inverse of `sig.rs`; property-test `render(parse(s)) ≈ s` on a signature
  corpus harvested from nixpkgs lib.

This also makes Nix a render *target* for the other five languages' IR
(cross-language signature display), consistent with how render currently works.

## 11. Testing strategy

- **Fixtures** (`tests/fixtures/nix/`): (a) `lib-style` — RFC-145 + legacy +
  curried + pattern-defaults + aliases via `inherit`; (b) `mini-flake` — full
  flake.nix/flake.lock with one input (a second fixture flake, materialized
  offline), packages + devShell + nixosModule + template; (c) `options-module`;
  (d) recorded FlakeHub HTTP fixtures (resolve + tarball) — network-gated live
  tests like Python's traversal tests.
- **Parity/superiority tests**: nixdoc-JSON superset on a vendored
  `attrsets.nix` snapshot; `nixosOptionsDoc` equivalence on the options
  fixture; alias-unification against noogle's published data for 20 sampled
  `lib` functions.
- **Property tests**: sig parser round-trip via the render backend; doc-comment
  scanner against randomized whitespace/comment interleavings.
- **Eval-safety tests**: infinite recursion, `throw`/`abort` mid-tree, missing
  input, absolute-path escape attempt through `DocsIO` — all must degrade to
  documented gaps, never panic or hang past the timeout.
- Snapshot IR tests per the Go/Java convention (goldens per fixture).

## 12. Phase order & exit criteria (recap)

| Phase | Delivers | Exit criterion |
|---|---|---|
| 0 | snix vendored under Buck + visibility patch | eval smoke test builds & passes |
| 1 | heart enums + dispatch skeleton | `collect()` returns empty Index for fixture |
| 2 | static producer: rnix + docs + **sig parser** | superset-of-nixdoc parity test |
| 3 | FlakeHub traversal + fallbacks + input materialization | nixpkgs + tagged flake fetch reproducibly |
| 4 | hermetic eval + walker + alias fusion + builtins pkg | lib.* ≥ noogle coverage w/ positions |
| 5 | NixOS options | nixosOptionsDoc-equivalent structured output |
| 6 | render backend | sig round-trip property test |
| 7 | hardening: budgets, timeouts, worker isolation if needed; full-nixpkgs enumeration decision | soak on top-50 FlakeHub flakes, zero hangs |

Phases 2 and 3 are independent after 1 (parallelizable); 4 needs 2+3; 5 needs 4;
6 needs only the IR (parallel to 3–5).

## 13. Risks & open decisions

1. **GPL-3.0 (snix)** — fine for a non-conveyed server; blocks permissive
   distribution of the linked binary. Confirm before Phase 0; subprocess-oracle
   escape hatch documented in §2. nixdoc is also GPL: algorithms clean-roomed,
   no code ported.
2. **snix API flux** — no crates.io releases by design; pin one canon rev in
   git.bzl, keep the patch additive, budget a re-pin per quarter.
3. **Input closure cost** — nixpkgs is a 48 MB tarball per lock; content-
   addressed input cache (§6) is mandatory, not an optimization.
4. **nixpkgs-scale enumeration** — 120k packages via eval is a nix-eval-jobs-
   sized problem; deliberately out of scope until Phase 7 (declared `packages`,
   `lib`, options, and FlakeHub's shallow outputs tree cover the real use
   cases first).
5. **FlakeHub API is unversioned/undocumented** — shapes verified live
   2026-07-08; defensive serde mirrors; `fh` CLI source is the de-facto spec.
6. **FlakeHub coverage (865 flakes)** — the fallback chain (§6) is load-bearing,
   not optional; most real flakes resolve via `github:`.
7. **`!Send` eval** — per-package thread + timeout; no sharing `Value`s across
   evaluations without shared `GlobalsMap`/`SourceCode` (documented snix
   constraint).
8. **arborium-nix existence** — unverified; CST extraction degrades gracefully
   to `CstSet::default()` if absent.
9. **Dynamic attrnames / deeply lazy idioms** (`${...}` keys, functors,
   `callPackage` w/ overrides) — evaluation handles most; residual gaps degrade
   to static-layer records with an attribute marking partial fidelity.
