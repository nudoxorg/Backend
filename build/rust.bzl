load("//build/third-party:registry.bzl", "REGISTRY")
load("//build/third-party:git.bzl", "GIT")

_TP = "//build/third-party"

_MEMBERS = {
    "heart":            "//workspace/heart:heart",
    "ir":               "//workspace/compiler/intermediate-representation:ir",
    "compiler":         "//workspace/compiler:compiler",
    "registry":         "//workspace/registry:registry",
    "runtime":          "//workspace/runtime:runtime",
    "server":           "//workspace/server:server-lib",
    "caching":          "//workspace/util/caching:caching",
    "sandbox":          "//workspace/util/sandbox:sandbox",
    "cas":              "//workspace/cas:cas",
    "version":          "//workspace/version:version",
    # nudox:members
}

_VALID_CRATES = dict(
    [
        ((e["crate_name"] if e.get("crate_name") else e["name"].replace("-", "_")), True)
        for e in REGISTRY
        if e["alias"]
    ] + [
        (c["name"].replace("-", "_"), True)
        for repo in GIT
        for c in repo["crates"]
    ],
)

_VALID_LABELS = dict([(e["label"], True) for e in REGISTRY])

def _crate_target(n):
    if n not in _VALID_CRATES:
        fail("unknown third-party crate '{}'. Vendor it with:  buck2 run //:add -- {}".format(n, n))
    return _TP + ":" + n

def _label_target(label):
    if label not in _VALID_LABELS:
        fail("unknown registry label '{}'. Vendor the crate with:  buck2 run //:add -- <crate>".format(label))
    return _TP + ":" + label

def _resolve_crate_dep(name):
    """Map a crate alias or registry label to a buck target label."""
    if name in _VALID_CRATES:
        return _crate_target(name)
    if name in _VALID_LABELS:
        return _label_target(name)
    fail("unknown crate '{}'. Use an alias (e.g. \"sqlx\") or registry label (e.g. \"futures-0_3\"). Vendor with:  buck2 run //:add -- <crate>".format(name))

def crate(name, features = None, pin_only = False):
    """Declare a third-party crate in a ``deps([...])`` list.

    One call covers the dep edge and optional feature flags — no duplicate
    ``"sqlx"`` string alongside ``crate("sqlx", features = [...])``::

        crate("sqlx", features = ["postgres", "runtime-tokio-rustls"])
        crate("futures-0_3", features = ["std"])   # registry label, not alias
        crate("deno_error", features = ["serde"], pin_only = True)  # features only

    ``name`` is either a vendored alias (``"sqlx"``) or a registry label
    (``"futures-0_3"``, ``"http-1"``) when the alias points at another version.
    """
    if not pin_only and name not in _VALID_CRATES and name not in _VALID_LABELS:
        fail("unknown crate '{}'. Vendor it with:  buck2 run //:add -- <crate>".format(name))
    entry = {"crate": name}
    if features != None:
        entry["features"] = features
    if pin_only:
        entry["pin_only"] = True
    return entry

def _member_target(m):
    if m not in _MEMBERS:
        fail("unknown workspace member '{}'. Known members: {}".format(m, sorted(_MEMBERS.keys())))
    return _MEMBERS[m]

def member(name):
    """Workspace member spec entry for ``deps([...])``."""
    if name not in _MEMBERS:
        fail("unknown workspace member '{}'. Known members: {}".format(name, sorted(_MEMBERS.keys())))
    return {"member": name}

def target(label):
    """Same-package or relative buck target (e.g. test crate → library under test)."""
    return {"target": label}

def dep_pins(pins):
    """Per-edge version pins consumed by ``gen-registry``."""
    return {"pins": pins}

def workspace(n):
    return _member_target(n)

def _parse_dep_entry(entry, crate_targets, members, local_targets, features, dep_pins):
    t = type(entry)
    if t == type(""):
        crate_targets.append(_resolve_crate_dep(entry))
        return

    if t != type({}):
        fail("deps entry must be a string or dict, got {}".format(t))

    if entry.get("member") != None:
        members.append(entry["member"])
        return

    if entry.get("target") != None:
        local_targets.append(entry["target"])
        return

    if entry.get("raw") != None:
        local_targets.append(entry["raw"])
        return

    if entry.get("pins") != None:
        for consumer, edges in entry["pins"].items():
            dep_pins.setdefault(consumer, {}).update(edges)
        return

    crate_name = entry.get("crate")
    if crate_name != None:
        feat_list = entry.get("features")
        if feat_list:
            features.setdefault(crate_name, []).extend(feat_list)
        if not entry.get("pin_only"):
            crate_targets.append(_resolve_crate_dep(crate_name))
        return

    if entry.get("features") != None:
        for crate_name, feat_list in entry["features"].items():
            features.setdefault(crate_name, []).extend(feat_list)
        return

    fail("deps entry must be from crate(), member(), target(), or dep_pins() — got {}".format(entry))

def deps(spec = None, crates = None, members = None, raw = None, features = None, dep_pins = None):
    """Declare dependencies as a cargo.toml-shaped list.

    Use ``crate()`` for every third-party dep (with optional ``features``).
    Use ``member()`` for workspace crates.  Use ``target()`` for same-package
    edges (tests → library).  Versions live in ``registry.bzl``::

        deps([
            crate("sqlx", features = ["postgres", "runtime-tokio-rustls"]),
            crate("futures-0_3", features = ["std"]),
            member("heart"),
            target(":registry"),
        ])

    Legacy kwargs (``crates``, ``members``, ``raw``, ``features``, ``dep_pins``)
    are still accepted and merged with ``spec``.
    """
    crate_targets = [_crate_target(c) for c in (crates or [])]
    _members = list(members or [])
    local_targets = list(raw or [])
    _features = {}
    _dep_pins = {}

    for crate_name, feat_list in (features or {}).items():
        _features.setdefault(crate_name, []).extend(feat_list)
    for consumer, edges in (dep_pins or {}).items():
        _dep_pins.setdefault(consumer, {}).update(edges)

    if spec != None:
        for entry in spec:
            _parse_dep_entry(entry, crate_targets, _members, local_targets, _features, _dep_pins)

    return crate_targets + [_member_target(m) for m in _members] + local_targets

# Workspace-only lints (Phase 0 guardrail). Scoped to our crates via the
# rules below rather than the toolchain so third-party vendored crates stay
# quiet. `unreachable_pub` surfaces `pub` items that are never reachable across
# a crate boundary — the zero-tooling way to find cross-crate dead surface under
# Buck2 (cargo-machete/udeps don't run here). `unused` catches dead code, unused
# imports/vars.
#
# Kept at WARN, not DENY, deliberately: a chunk of the sandbox crate
# (seccomp/landlock/cgroup helpers) is `#[cfg(target_os = "linux")]`-conditional,
# so on a macOS dev machine it reads as dead and a hard `-Dunused` would fail the
# local build for code that is live on Linux. Phase 5's deny promotion therefore
# belongs in CI (Linux), where the cfg-gated code is compiled in — flip these to
# `-D` in the CI build config, not here. The cross-platform dead code these
# lints surfaced (unused imports, orphaned speculative methods) has been swept;
# what remains at WARN is the platform-conditional set and the pub→pub(crate)
# visibility chore.
WORKSPACE_LINTS = ["-Wunreachable_pub", "-Wunused"]

def _with_workspace_lints(kw):
    merged = list(WORKSPACE_LINTS)
    merged.extend(kw.get("rustc_flags", []))
    out = dict(kw)
    out["rustc_flags"] = merged
    return out

def rust_crate(name, deps = [], crate_root = "lib.rs", edition = "2024",
               srcs = None, features = None, visibility = None, **kw):
    kw = _with_workspace_lints(kw)
    native.rust_library(
        name = name,
        srcs = srcs if srcs != None else native.glob(["**/*.rs"]),
        crate_root = crate_root,
        edition = edition,
        deps = deps,
        features = features,
        visibility = visibility or ["PUBLIC"],
        **kw
    )

def rust_tests(prefix, deps = [], edition = "2024", env = None, resources = None, **kw):
    """One rust_test target per `tests/*.rs` integration-test crate.

    Each file is its own crate (cargo's layout); `tests/common/` rides along as
    the shared fixture module.

    Sets ``CARGO_MANIFEST_DIR`` to ``"."`` so ``env!("CARGO_MANIFEST_DIR")``
    resolves to the package's symlinked source root (the same layout cargo
    uses). Fixture trees under ``tests/fixtures/`` are included in ``srcs`` so
    they materialize under that root and can be read at runtime.
    """
    shared_rs = native.glob(["tests/common/**/*.rs", "tests/support/**/*.rs"])
    # Package source root is already the crate root; "." is correct (NOT
    # package_name(), which would nest as __srcs/<package_name>/...).
    test_env = {"CARGO_MANIFEST_DIR": "."}
    if env:
        test_env.update(env)
    # Data fixtures (Cargo.toml trees, sample sources, …) must live in the
    # compile-time source tree so CARGO_MANIFEST_DIR lookups succeed. Also
    # ship them as resources for sandboxed/RE runs that re-materialize them.
    fixture_files = native.glob(["tests/fixtures/**"])
    test_resources = dict(resources) if resources else {}
    if fixture_files and "tests/fixtures" not in test_resources:
        for f in fixture_files:
            test_resources[f] = f
    for src in native.glob(["tests/*.rs"]):
        stem = src.removeprefix("tests/").removesuffix(".rs")
        test_kw = dict(kw)
        test_kw["env"] = test_env
        if test_resources:
            test_kw["resources"] = test_resources
        native.rust_test(
            name = prefix + "-" + stem,
            # Fixture data rides along in srcs so it lands under __srcs and is
            # reachable via env!("CARGO_MANIFEST_DIR")/tests/fixtures/….
            srcs = [src] + shared_rs + fixture_files,
            crate_root = src,
            edition = edition,
            deps = deps,
            **test_kw
        )

def rust_bin(name, srcs, crate_root, deps = [], edition = "2024", visibility = None, **kw):
    kw = _with_workspace_lints(kw)
    native.rust_binary(
        name = name,
        srcs = srcs,
        crate_root = crate_root,
        edition = edition,
        deps = deps,
        visibility = visibility or ["PUBLIC"],
        **kw
    )