load("@prelude//rust:cargo_package.bzl", "cargo")
load("@prelude//rust:cargo_buildscript.bzl", "buildscript_run")

# PATH injected into build script environments.
# Set by the Nix devshell in its ignored `.buckconfig.local`. Falls back to
# system paths for non-Nix hosts.
_BUILDSCRIPT_PATH = read_config("build", "devshell_bin", "/usr/bin:/bin:/usr/sbin:/sbin")

# The Cargo.lock → registry translation flattens target-specific dependencies
# into the unconditional `deps` list. Restore Cargo's cfg-gating with select().

def _is_windows_dep(dep):
    label = dep[1:] if dep.startswith(":") else dep
    return (
        label.startswith("windows-") or
        label.startswith("windows_") or
        label.startswith("winapi-") or
        label.startswith("uv_windows-") or
        label.startswith("miow-")
        # NB: deliberately excludes "winnow" (a parser crate, not Windows).
    )

def _is_linux_dep(dep):
    label = dep[1:] if dep.startswith(":") else dep
    return (
        label.startswith("inotify-") or
        label.startswith("inotify_sys-") or
        label.startswith("libredox-") or
        label.startswith("perf_event-") or
        label.startswith("perf_event_")
    )

def _is_macos_dep(dep):
    label = dep[1:] if dep.startswith(":") else dep
    return (
        label.startswith("fsevent_sys-") or
        label.startswith("fsevent-") or
        label.startswith("kqueue-") or
        label.startswith("core_foundation-") or
        label.startswith("core_foundation_sys-")
    )

def _gate_platform_deps(deps):
    windows = [d for d in deps if _is_windows_dep(d)]
    linux = [d for d in deps if _is_linux_dep(d)]
    macos = [d for d in deps if _is_macos_dep(d)]
    portable = [d for d in deps if not _is_windows_dep(d) and not _is_linux_dep(d) and not _is_macos_dep(d)]

    if not windows and not linux and not macos:
        return deps

    result = portable
    if windows:
        result = result + select({
            "prelude//os/constraints:windows": windows,
            "DEFAULT": [],
        })
    if linux:
        result = result + select({
            "prelude//os/constraints:linux": linux,
            "DEFAULT": [],
        })
    if macos:
        result = result + select({
            "prelude//os/constraints:macos": macos,
            "DEFAULT": [],
        })
    return result

def _cargo_dep_token(s):
    """Normalize a Cargo `links` name or metadata key the way Cargo builds DEP_* vars."""
    out = ""
    for i in range(len(s)):
        c = s[i]
        if (c >= "a" and c <= "z") or (c >= "A" and c <= "Z") or (c >= "0" and c <= "9"):
            out += c.upper()
        else:
            out += "_"
    return out

def _dep_env_from_import_links(import_links):
    """Synthesize DEP_<LINKS>_* env vars for a buildscript that depends on `links` crates."""
    env = {}
    for links_name, dep_label in import_links.items():
        token = _cargo_dep_token(links_name)
        out_dir = "$(location :" + dep_label + "-build-script-run[out_dir])"
        env["DEP_" + token + "_INCLUDE"] = out_dir + "/include"
        env["DEP_" + token + "_ROOT"] = out_dir
    return env

def _git_crate(archive_name, crate_name, subdir, edition, deps, features, proc_macro, patch, patch_strip, env, rustc_flags, crate_root_suffix, named_deps, build_script = False, build_script_root = "build.rs", build_script_deps = None, version = "0.0.0"):
    normalized = crate_name.replace("-", "_")

    if patch != None:
        native.genrule(
            name = normalized + "-src",
            srcs = [":" + archive_name, ":" + patch],
            bash = (
                "mkdir -p \"$OUT\"\n" +
                "cp -R $(location :" + archive_name + ")/" + subdir + "/. \"$OUT\"/\n" +
                "/usr/bin/patch -p" + str(patch_strip) + " -d \"$OUT\" < $(location :" + patch + ")"
            ),
            out = normalized,
            visibility = [],
        )
    elif subdir:
        native.genrule(
            name = normalized + "-src",
            srcs = [":" + archive_name],
            bash = "mkdir -p \"$OUT\" && cp -R $(location :" + archive_name + ")/" + subdir + "/. \"$OUT\"/",
            out = normalized,
            visibility = [],
        )
    else:
        native.genrule(
            name = normalized + "-src",
            srcs = [":" + archive_name],
            bash = "mkdir -p \"$OUT\" && cp -R $(location :" + archive_name + ")/. \"$OUT\"/",
            out = normalized,
            visibility = [],
        )

    src_ref = ":" + normalized + "-src"
    # Gate `bs_deps` from the *ungated* dep list — `_gate_platform_deps` returns
    # a `select()` when platform-specific deps are present, and calling it again
    # on that Select fails (`Operation (iter) not supported on type Select`).
    bs_deps = _gate_platform_deps(build_script_deps if build_script_deps != None else deps)
    deps = _gate_platform_deps(deps)

    _ver_parts = version.split(".")
    _ver_major = _ver_parts[0] if len(_ver_parts) > 0 else "0"
    _ver_minor = _ver_parts[1] if len(_ver_parts) > 1 else "0"
    _ver_patch_pre = _ver_parts[2] if len(_ver_parts) > 2 else "0"
    _ver_patch = _ver_patch_pre.split("+")[0].split("-")[0]

    pkg_env = dict({
        "CARGO_PKG_VERSION": version,
        "CARGO_PKG_NAME": crate_name,
        "CARGO_PKG_VERSION_MAJOR": _ver_major,
        "CARGO_PKG_VERSION_MINOR": _ver_minor,
        "CARGO_PKG_VERSION_PATCH": _ver_patch,
        "CARGO_PKG_VERSION_PRE": "",
        "CARGO_PKG_AUTHORS": "",
        "CARGO_PKG_DESCRIPTION": "",
        "CARGO_PKG_HOMEPAGE": "",
        "CARGO_PKG_REPOSITORY": "",
        "CARGO_PKG_LICENSE": "",
        "CARGO_PKG_LICENSE_FILE": "",
        "CARGO_PKG_README": "",
        "CARGO_CRATE_NAME": normalized,
        "CARGO_MANIFEST_DIR": "$(location " + src_ref + ")",
    }, **env)

    if build_script:
        cargo.rust_binary(
            name = normalized + "-build-script-build",
            srcs = [src_ref],
            crate = "build_script_build",
            crate_root = normalized + "/" + build_script_root,
            edition = edition,
            features = features,
            visibility = [],
            deps = bs_deps,
            named_deps = named_deps,
            env = pkg_env,
        )
        buildscript_run(
            name = normalized + "-build-script-run",
            package_name = crate_name,
            buildscript_rule = ":" + normalized + "-build-script-build",
            manifest_dir = src_ref,
            features = features,
            version = version,
            rustc_link_lib = True,
            rustc_link_search = True,
            env = dict({
                "CARGO_PKG_VERSION_MAJOR": _ver_major,
                "CARGO_PKG_VERSION_MINOR": _ver_minor,
                "CARGO_PKG_VERSION_PATCH": _ver_patch,
                "CARGO_PKG_VERSION_PRE": "",
                "CARGO_PKG_AUTHORS": "",
                "CARGO_PKG_DESCRIPTION": "",
                "CARGO_PKG_HOMEPAGE": "",
                "CARGO_PKG_REPOSITORY": "",
                "CARGO_PKG_LICENSE": "",
                "CARGO_PKG_LICENSE_FILE": "",
                "CARGO_PKG_README": "",
                "CARGO_CRATE_NAME": normalized,
                "OPT_LEVEL": "2",
                "DEBUG": "false",
                "PROFILE": "release",
                "NUM_JOBS": "1",
                "PATH": _BUILDSCRIPT_PATH,
            }, **env),
        )
        cargo.rust_library(
            name = normalized,
            srcs = [src_ref],
            crate_root = normalized + "/" + crate_root_suffix,
            crate = normalized,
            edition = edition,
            features = features,
            proc_macro = proc_macro,
            visibility = ["PUBLIC"],
            deps = deps,
            named_deps = named_deps,
            env = dict(pkg_env, **{
                "OUT_DIR": "$(location :" + normalized + "-build-script-run[out_dir])",
            }),
            rustc_flags = ["@$(location :" + normalized + "-build-script-run[rustc_flags])"],
        )
    else:
        cargo.rust_library(
            name = normalized,
            srcs = [src_ref],
            crate_root = normalized + "/" + crate_root_suffix,
            crate = normalized,
            edition = edition,
            features = features,
            proc_macro = proc_macro,
            visibility = ["PUBLIC"],
            deps = deps,
            named_deps = named_deps,
            env = pkg_env,
            rustc_flags = rustc_flags,
        )

def _git_repo(archive_name, urls, strip_prefix, sha256, crates):
    native.http_archive(
        name = archive_name,
        urls = urls,
        sha256 = sha256,
        strip_prefix = strip_prefix,
        visibility = [],
    )
    for c in crates:
        _git_crate(
            archive_name = archive_name,
            crate_name = c["name"],
            subdir = c.get("subdir", ""),
            edition = c.get("edition", "2021"),
            deps = c.get("deps", []),
            features = c.get("features", []),
            proc_macro = c.get("proc_macro", False),
            patch = c.get("patch", None),
            patch_strip = c.get("patch_strip", 1),
            env = c.get("env", {}),
            rustc_flags = c.get("rustc_flags", []),
            crate_root_suffix = c.get("crate_root_suffix", "src/lib.rs"),
            named_deps = c.get("named_deps", {}),
            build_script = c.get("build_script", False),
            build_script_root = c.get("build_script_root", "build.rs"),
            build_script_deps = c.get("build_script_deps", None),
            version = c.get("version", "0.0.0"),
        )

def setup_all_git_repos(repos):
    for repo in repos:
        _git_repo(**repo)
