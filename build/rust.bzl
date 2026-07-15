_TP = "//build/third-party"

# After the streamlining consolidation:
#   - `version` + `telemetry` folded into `heart` (heart::version / heart::telemetry).
#   - `caching` + `cas` folded into `heart` as `heart::cache` (the stampede cache
#     and the content-addressed Tiered build store).
#   - `runtime` folded into `registry`; `protocol` folded into `registry` for the
#     Cargo build (registry::protocol) and into `compiler` for the Buck build
#     (compiler::protocol — the daemon's byte-identical copy).
#   - `sandbox` moved under compiler/.
#   - `server` is Cargo-only (no BUCK target).
_MEMBERS = {
    "heart":            "//workspace/heart:heart",
    "ir":               "//workspace/ir:ir",
    "compiler":         "//workspace/compiler:compiler",
    "registry":         "//workspace/registry:registry",
    "sandbox":          "//workspace/compiler/sandbox:sandbox",
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

def _dirname(path):
    """POSIX parent directory of an absolute path string."""
    cleaned = path.rstrip("/") if path else ""
    i = cleaned.rfind("/")
    if i <= 0:
        return ""
    return cleaned[:i]

def nudox_toolchain_test_env():
    """Env for sealed-producer integration tests under Buck.

    Buck hermetic test runs do not inherit the devshell's ``NUDOX_TOOLCHAIN_PATH``.
    Sealed producers install a fixed PATH (``/usr/bin:/bin:/nix/var/nix/...``)
    and only prepend dirs from ``ToolchainSet::from_env()`` (see
    ``sandbox/toolchains.rs`` + ``compile/isolate.rs``). Without this env, live
    Go/Java/C# oracles fail with ``go``/``javadoc``/``dotnet`` not found.

    Derives bin dirs from the NIX-GENERATED ``.buckconfig`` sections written by
    the flake devshell (``[go] go_binary``, ``[java] java_home``,
    ``[csharp] dotnet``). Empty when those keys are unset (non-Nix hosts).

    Shared by Go/Java/C# snap agents — keep go + java + csharp bins together.
    """
    # Prefer root-cell config (NIX-GENERATED block lives on the root .buckconfig).
    path_dirs = []
    go_bin = read_root_config("go", "go_binary", "")
    if go_bin:
        d = _dirname(go_bin)
        if d:
            path_dirs.append(d)
    java_home = read_root_config("java", "java_home", "")
    if java_home:
        path_dirs.append(java_home.rstrip("/") + "/bin")
    # csharp.dotnet is an absolute path to the `dotnet` binary (not its dir).
    dotnet = read_root_config("csharp", "dotnet", "")
    if dotnet:
        d = _dirname(dotnet)
        if d:
            path_dirs.append(d)

    # De-dupe while preserving order (go/dotnet may share a bin dir in theory).
    seen = {}
    unique = []
    for d in path_dirs:
        if d not in seen:
            seen[d] = True
            unique.append(d)

    env = {}
    if unique:
        env["NUDOX_TOOLCHAIN_PATH"] = ":".join(unique)
    # JAVA_HOME is required by some JDK tools beyond bare PATH (e.g. javadoc).
    if java_home:
        env["NUDOX_TOOLCHAIN_JAVA_HOME"] = java_home
        env["JAVA_HOME"] = java_home
    return env

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
    # Data fixtures + insta goldens must live in the compile-time source tree
    # so CARGO_MANIFEST_DIR lookups succeed. Also ship them as resources for
    # sandboxed/RE runs that re-materialize them.
    fixture_files = native.glob(["tests/fixtures/**"])
    snapshot_files = native.glob(["tests/snapshots/**"])
    data_files = fixture_files + snapshot_files
    test_resources = dict(resources) if resources else {}
    for f in data_files:
        if f not in test_resources:
            test_resources[f] = f
    for src in native.glob(["tests/*.rs"]):
        stem = src.removeprefix("tests/").removesuffix(".rs")
        test_kw = dict(kw)
        test_kw["env"] = test_env
        if test_resources:
            test_kw["resources"] = test_resources
        native.rust_test(
            name = prefix + "-" + stem,
            # Fixture/snapshot data rides along in srcs so it lands under __srcs
            # and is reachable via env!("CARGO_MANIFEST_DIR")/tests/{fixtures,snapshots}/….
            srcs = [src] + shared_rs + data_files,
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