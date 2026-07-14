_TP = "//build/third-party"

_MEMBERS = {
    "heart":            "//workspace/heart:heart",
    "ir":               "//workspace/ir:ir",
    "protocol":         "//workspace/protocol:protocol",
    "compiler":         "//workspace/compiler:compiler",
    "registry":         "//workspace/registry:registry",
    "runtime":          "//workspace/runtime:runtime",
    "server":           "//workspace/server:server-lib",
    "caching":          "//workspace/util/caching:caching",
    "sandbox":          "//workspace/util/sandbox:sandbox",
    "cas":              "//workspace/cas:cas",
    "version":          "//workspace/version:version",
    "telemetry":        "//workspace/telemetry:telemetry",
}

def crate(name):
    """Declare a third-party crate dep. Returns the Buck target label."""
    return _TP + ":" + name

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

def workspace(n):
    return _member_target(n)

def _parse_dep_entry(entry, crate_targets, members, local_targets):
    t = type(entry)
    if t == type(""):
        crate_targets.append(entry)
        return

    if t != type({}):
        fail("deps entry must be a string (from crate()) or dict (from member()/target()), got {}".format(t))

    if entry.get("member") != None:
        members.append(entry["member"])
        return

    if entry.get("target") != None:
        local_targets.append(entry["target"])
        return

    if entry.get("raw") != None:
        local_targets.append(entry["raw"])
        return

    fail("deps entry must be from crate(), member(), or target() — got {}".format(entry))

def deps(spec = None, crates = None, members = None, raw = None):
    """Declare dependencies as a list.

    Use ``crate()`` for every third-party dep, ``member()`` for workspace crates,
    and ``target()`` for same-package edges (e.g. tests → library)::

        deps([
            crate("sqlx"),
            crate("futures-0_3"),
            member("heart"),
            target(":registry"),
        ])
    """
    crate_targets = [_TP + ":" + c for c in (crates or [])]
    _members = list(members or [])
    local_targets = list(raw or [])

    if spec != None:
        for entry in spec:
            _parse_dep_entry(entry, crate_targets, _members, local_targets)

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