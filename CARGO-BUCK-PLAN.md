# Cargo-like tooling for Buck2 — design plan

Goal: drive our Buck2 build the way `cargo` drives a workspace — one tool with
`add`, `update`, `check`, `test` (and `build`, `new`) that work both **globally**
(whole workspace) and **per sub-crate**, plus a reconciler so that editing a
`BUCK` file and running `update` teaches the registry about newly-referenced
crates. Do as much as possible in Python; push everything that *can* be native
into Buck itself so we delete commands rather than add them.

---

## 1. What exists today (the substrate we build on)

Third-party (crates.io + git) layer, all under `build/third-party/`:

- `registry.bzl` — `REGISTRY = [ {name, version, sha256, edition, label, alias,
  deps, features, build_script, proc_macro, (named_deps, lib_root, crate_name,
  extra_env, build_script_root)} ]`. Pure data, `exec()`-able from Python.
- `git.bzl` — `GIT = [ {archive_name, urls, sha256, strip_prefix, crates:[…]} ]`
  for source-built repos (pyrefly, ruff, terminusdb, lsp-types).
- `defs.bzl` — `third_party()` expands each entry to `http_archive` +
  `cargo.rust_library`/`rust_binary` + `native.alias(name = crate_name)` for the
  human-friendly label. `_gate_platform_deps()` re-adds cfg() gating.
- `tools/gen-registry.py` — the crown jewel: fetches every `Cargo.toml`
  (disk-cached in `.registry-cache/`), does version-aware fixed-point **feature
  propagation** across the whole graph, activates optional deps, resolves
  sub-dep version conflicts, preserves manual fields, rewrites `registry.bzl`.
- `tools/add-crate.py` — fetches a crate + transitive deps from crates.io,
  detects edition/proc-macro/build.rs/lib-root, inserts sorted entries.

First-party layer:

- `build/rust.bzl` — `rust_crate`, `rust_bin`, `rust_tests`, and the `deps()`
  helper: `deps(crates=[…], members=[…], raw=[…])`. `crates=[…]` are alias names
  into `//build/third-party:NAME`; `members=[…]` map through the `_MEMBERS`
  dict to workspace targets; `raw=[…]` are escape-hatch labels
  (e.g. `//build/third-party:http-1` for a version-pinned target).
- `workspace/*/BUCK` — each member calls `rust_crate(deps = deps(members=…,
  crates=…))`. `server` also shows `rust_tests` + `rust_bin`.
- root `BUCK` — top-level `alias()`es (`//:server`, `//:registry`, …).

**Key facts that shape the design**

1. `registry.bzl`, `git.bzl`, and every `BUCK` file are Starlark = restricted
   Python. We already `exec()` `registry.bzl`. We can `exec()` `BUCK` files too,
   under a stub environment, to harvest what they reference. This is the hinge
   for "notice a new BUCK entry."
2. Buck's parse/analysis phase is **hermetic**: no network, no arbitrary Python.
   So anything needing crates.io (fetch `Cargo.toml`, sha256, version resolve)
   **cannot** be a pure-Starlark rule — it must be an out-of-band step we invoke
   via `buck2 run`. Everything that only needs data already in the registry
   (referential integrity, feature sanity) **can** be native Starlark and should
   be, so `buck2 build` itself becomes the checker.
3. The valid set of `crates=[…]` names = `{crate_name of REGISTRY entries where
   alias == True}` ∪ `{normalized name of every GIT crate}`. Both are already in
   files we can load from Starlark and from Python.

---

## 2. Target UX

One entrypoint, `nudox` (thin wrapper) → `build/third-party/tools/crates.py`,
exposed three ways so there is no "separate command" to remember:

```
# via buck (canonical, hermetic-ish, discoverable in `buck2 targets //...`)
buck2 run //:add    -- serde --features derive --to server
buck2 run //:update                       # refresh + reconcile everything
buck2 run //:check  -- --offline          # fast lint w/o buck analysis
buck2 run //:new    -- workspace/foo

# native buck for the compile/test verbs (no wrapper needed)
buck2 build //...                         # global check == compile
buck2 build //workspace/registry/...      # per-crate check
buck2 test  //...                         # global test
buck2 test  //workspace/server/...        # per-crate test

# bare CLI for humans / pre-commit (same code path)
nudox add serde --to server
nudox update
nudox check
```

Subcommand semantics, global vs per-crate:

| verb    | global                             | per sub-crate (`<member>`)                  | engine |
|---------|------------------------------------|---------------------------------------------|--------|
| `add`   | ensure dep in registry             | + append to that member's `crates=[…]`      | python (net) |
| `new`   | scaffold member, wire `_MEMBERS`   | —                                           | python |
| `update`| gen-registry + reconcile + gc      | reconcile only that member                  | python (net) |
| `check` | `buck2 build //...` + offline lint | `buck2 build //workspace/<m>/...` + lint    | starlark + python |
| `test`  | `buck2 test //...`                 | `buck2 test //workspace/<m>/...`            | native buck |
| `build` | `buck2 build //...`                | `buck2 build //workspace/<m>/...`           | native buck |

---

## 3. Architecture: split the code into a shared library

Today `gen-registry.py` and `add-crate.py` duplicate: tarball fetch, `Cargo.toml`
parse, semver, Starlark emit, `load_registry`. Refactor into a package so all
verbs share one implementation (this is itself part of "remove separate
commands"):

```
build/third-party/tools/
  crates.py            # NEW: argparse dispatcher — the single entrypoint
  nudox/               # NEW package (the shared library)
    __init__.py
    registry.py        # load/emit registry.bzl + git.bzl (from gen-registry)
    cratesio.py        # fetch tarball, parse Cargo.toml, sha256, cache
    semver.py          # satisfies_cargo_requirement, _parse_semver
    starlark.py        # format_* emitters  +  a Starlark *reader* (see §5)
    features.py        # propagate_feature_activations & friends
    resolve.py         # dep-label resolution, sub-dep conflict fixer
    buckfiles.py       # NEW: harvest crates=[]/members=[] from workspace BUCK
    commands/
      add.py  new.py  update.py  check.py  run.py   # verbs
  gen-registry.py      # kept as `crates.py update --registry` shim (compat)
  add-crate.py         # kept as `crates.py add` shim (compat)
```

`add` and `gen-registry` **unify**: `add` seeds skeleton entries for the new
crate + missing transitive deps (today's `collect_raw`/`build_entries`), then
runs the full feature-propagation pass over the whole registry so the new crate
gets correct features/deps the same way a full regen would. One code path, no
drift between "added" and "regenerated" crates.

---

## 4. Native Buck integration (the IDEAL: fold verbs into buck)

### 4a. `check` (referential integrity) becomes analysis-time `fail()`

Move the "does this crate exist?" check out of a separate command and into the
build itself. In `build/rust.bzl`:

```starlark
load("//build/third-party:registry.bzl", "REGISTRY")
load("//build/third-party:git.bzl", "GIT")

_TP_ALIASES = {e["crate_name"] if e.get("crate_name") else e["name"].replace("-", "_"): True
               for e in REGISTRY if e["alias"]}
_GIT_TARGETS = {c["name"].replace("-", "_"): True for repo in GIT for c in repo["crates"]}
_VALID = dict(_TP_ALIASES, **_GIT_TARGETS)

def crate(n):
    if n not in _VALID:
        fail("unknown third-party crate '{}'. Add it with:  buck2 run //:add -- {}".format(n, n))
    return _TP + ":" + n

def _valid_member(m):
    if m not in _MEMBERS:
        fail("unknown workspace member '{}'. Known: {}".format(m, sorted(_MEMBERS)))
    return _MEMBERS[m]
```

Now `buck2 build //...` **is** the consistency check: a typo'd or unvendored
crate fails at parse with a precise, actionable message pointing at `//:add`.
This deletes an entire class of "separate check command" — the whole
referential dimension is native. (Cost: `rust.bzl` now `load()`s the ~400-entry
registry; it's pure data, parses in ms, and Buck caches it.)

### 4b. Verbs as runnable Buck targets

`build/third-party/tools/BUCK` (new):

```starlark
load("@prelude//python_bootstrap:python_bootstrap.bzl", "python_bootstrap_binary")

python_bootstrap_binary(
    name = "crates",
    main = "crates.py",
    srcs = glob(["crates.py", "nudox/**/*.py"]),
    visibility = ["PUBLIC"],
)
```

root `BUCK` gets thin `command_alias`es so the verbs are first-class targets:

```starlark
command_alias(name = "add",    exe = "//build/third-party/tools:crates", args = ["add"])
command_alias(name = "update", exe = "//build/third-party/tools:crates", args = ["update"])
command_alias(name = "check",  exe = "//build/third-party/tools:crates", args = ["check"])
command_alias(name = "new",    exe = "//build/third-party/tools:crates", args = ["new"])
```

`buck2 run //:add -- serde` etc. `test`/`build` need no alias — they are the
native verbs already, and 4a made `check` mostly native too. So the *only*
genuinely-new commands are `add`/`update`/`new` (network + scaffolding), which
provably cannot be pure Buck.

### 4c. `check` (deep) as a BXL script — optional, native graph query

For a check that inspects the *resolved* graph (not just the registry data),
add `build/third-party/tools/check.bxl`. BXL is Buck's native
graph-introspection language; it runs inside buck2, no network:

```python
def _check(ctx):
    rust = ctx.uquery().kind("rust_(library|binary|test)", "//workspace/...")
    # assert every dep label resolves; flag registry entries with no rdeps (gc
    # candidates); diff BUCK-referenced crates vs registry alias set.
    ...
check = bxl_main(impl = _check, cli_args = {})
```

Invoked as `buck2 bxl //build/third-party/tools:check.bxl:check`. This is the
"tie into native buck" answer for graph-level checks; the network-bound
reconcile still lives in Python (§5).

---

## 5. The reconciler: "edit a BUCK file, run update, registry catches up"

This is the headline feature. `buckfiles.py` reads the workspace **without a
working build** by `exec()`-ing each `workspace/*/BUCK` under a stub environment
that captures every `crates=[…]` / `members=[…]` / `raw=[…]` reference:

```python
# buckfiles.py — Starlark emulation harness
def harvest(buck_path):
    referenced = {"crates": set(), "members": set(), "raw": set()}
    def _deps(crates=(), members=(), raw=()):
        referenced["crates"].update(crates); referenced["members"].update(members)
        referenced["raw"].update(raw); return []
    class _Native:                      # fake `native`
        glob = staticmethod(lambda *a, **k: [])
        def __getattr__(self, _): return lambda *a, **k: None
    env = {
        "load": lambda *a, **k: None,   # load() is a no-op; we inject the names
        "deps": _deps, "crate": lambda n: n, "workspace": lambda n: n,
        "rust_crate": lambda **k: None, "rust_bin": lambda **k: None,
        "rust_tests": lambda **k: None, "native": _Native(),
        "select": lambda d: [], "glob": lambda *a, **k: [],
    }
    exec(compile(Path(buck_path).read_text(), buck_path, "exec"), env)
    return referenced
```

(Reuses exactly the `exec`-the-Starlark trick already trusted for
`registry.bzl`. If a BUCK file ever grows constructs the stub can't model, fall
back to `buck2 uquery` for that file — but the harness handles today's files.)

`update` flow (`commands/update.py`):

1. **Harvest**: union `crates=[…]` across all `workspace/*/BUCK` → `referenced`.
2. **Diff** vs registry alias set ∪ git-crate names → `missing`, and
   (with `--prune`) compute `unused`.
3. **Add missing**: for each name in `missing`, run the `add` seeding path
   (fetch from crates.io + pull transitive deps). Fails loudly with the crate
   name if crates.io 404s (probably a typo in the BUCK file).
4. **Refresh**: run full `gen-registry` feature propagation over everything
   (respects `FEATURES_PINNED`, manual `named_deps`, git exclusions).
5. **GC (`--prune`)**: compute the reachable closure from BUCK-referenced roots
   through registry `deps` + git `deps`; entries outside the closure are dead
   vendored crates → drop them (or list them without `--prune`). This is the
   inverse of add and keeps `.registry-cache/` and `registry.bzl` from rotting.
6. **Report**: print a cargo-style summary (`Adding foo v1.2`, `Removing bar
   (unused)`, `Updating baz v1.0 -> v1.1`).

Per-member: `nudox update <member>` harvests only that BUCK file (still adds to
the shared registry — the registry is workspace-global, like a Cargo.lock).

**Symmetry with `add`**: `nudox add serde --to server` does the registry-side
work *and* edits `workspace/server/BUCK` to insert `"serde"` into its
`crates=[…]` (alpha-ordered, via the Starlark emitter reused as a small
list-patcher). So `add` writes the BUCK entry and `update` reads BUCK entries —
two directions over the same relation, guaranteed consistent by `check`/4a.

---

## 6. `new` — scaffold a first-party crate

`nudox new workspace/foo [--bin] [--deps a,b,c]`:

1. `mkdir workspace/foo`, write `lib.rs` (or `main.rs` with `--bin`).
2. Write `workspace/foo/BUCK` from a template calling `rust_crate`/`rust_bin`.
3. Patch `build/rust.bzl` `_MEMBERS` to add `"foo": "//workspace/foo:foo"`.
4. Patch root `BUCK` to add the `alias(name="foo", …)`.
5. `buck2 build //workspace/foo/...` to prove it wires up.

Steps 3–4 are surgical string edits guarded by anchor comments we add to
`rust.bzl`/`BUCK` (e.g. `# nudox:members`) so insertion is deterministic.

---

## 7. `check` / `test` / `build` verbs (the native-buck mapping)

A tiny Python shim maps member → target pattern and execs buck, so `nudox test
server` and `buck2 test //workspace/server/...` are identical:

```python
PATTERNS = {m: "//" + path + "/..." for m, path in _member_paths().items()}
def run_verb(verb, member):        # verb in {build, test}
    pat = PATTERNS[member] if member else "//..."
    os.execvp("buck2", ["buck2", verb, pat])
```

`check` = `build` of the pattern (compile is the type-check) **plus** the fast
offline lint from §4a/§8 for referential problems that don't need a full
compile. `--offline` skips buck entirely (pre-commit-friendly).

No re-implementation of build/test scheduling: we lean 100% on `buck2
build`/`buck2 test` and their native per-directory target patterns. That is the
"remove a separate command" ideal — the compile/test verbs *are* buck.

---

## 8. Guardrails: pre-commit + CI

- `.pre-commit-config.yaml` gains a `nudox check --offline` hook: exec the BUCK
  files + `registry.bzl`, assert every referenced crate resolves and the
  registry has no dangling `:label` deps — before commit, no network, sub-second.
- CI runs `buck2 build //...` (native check, §4a), `buck2 test //...`, and
  `nudox update --check` (a dry-run that fails if the registry is out of sync
  with the BUCK files — i.e. someone added a crate to a BUCK file but didn't run
  `update`). This makes the reconciler's invariant enforceable.

---

## 9. Phasing

1. **Refactor** the two scripts into the `nudox/` package + `crates.py`
   dispatcher; keep `gen-registry.py`/`add-crate.py` as shims. No behavior
   change — pure de-dup. (Verify with a `--dry-run` regen producing byte-identical `registry.bzl`.)
2. **Native check (4a)**: `fail()` on unknown crate/member in `rust.bzl`. Highest
   value, smallest change; instantly makes `buck2 build` the checker.
3. **`buckfiles.harvest` + `update` reconciler (§5)** incl. add-missing; then
   `--prune` GC.
4. **`add --to <member>`** BUCK-patching (§5 symmetry) and **`new`** (§6).
5. **Buck targets/aliases (4b)** + `nudox test/build/check` shims (§7).
6. **BXL deep check (4c)** + pre-commit/CI wiring (§8). Optional polish.

Each phase is independently shippable and leaves the tree buildable.

---

## 10. Open questions / risks

- **`command_alias` availability** at our buck2 pin (2026-07-01) — it's a native
  rule; confirm during phase 5, else fall back to `buck2 run
  //build/third-party/tools:crates -- add`.
- **BUCK files that outgrow the exec-harness** (macros we don't stub) — mitigate
  with a `buck2 uquery` fallback path in `buckfiles.py`.
- **Prune safety** — never drop a crate reachable from any BUCK root through the
  registry/git dep closure; default to *listing* unused, require `--prune` to
  delete, and keep it behind CI review.
- **Version-pinned duplicates** (`http-1`) are referenced via `raw=[…]`, not
  `crates=[…]`; the harvester must record `raw` labels too so prune doesn't reap
  them.
- **`rust.bzl` loading the registry** adds a parse-time dep from every workspace
  BUCK onto `registry.bzl`; acceptable (pure data, cached) but worth confirming
  no cycle with `third-party/BUCK`.
