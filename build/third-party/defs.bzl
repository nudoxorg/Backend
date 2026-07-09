load("@prelude//rust:cargo_package.bzl", "cargo")
load("@prelude//rust:cargo_buildscript.bzl", "buildscript_run")

# The Cargo.lock → registry translation flattens target-specific dependencies
# (`[target.'cfg(windows)'.dependencies]`, `[target.'cfg(target_os = "linux")'.dependencies]`,
# etc.) into the unconditional `deps` list. Crates that only compile on their
# respective platform will fail on other hosts. Restore Cargo's cfg-gating by
# wrapping those deps in a `select()`.

def _is_windows_dep(dep):
    label = dep[1:] if dep.startswith(":") else dep
    return (
        label.startswith("windows-") or
        label.startswith("windows_") or
        label.startswith("winapi-") or
        label.startswith("uv_windows-")
        # NB: deliberately excludes "winnow" (a parser crate, not Windows).
    )

def _is_linux_dep(dep):
    label = dep[1:] if dep.startswith(":") else dep
    return (
        label.startswith("inotify-") or
        label.startswith("inotify_sys-") or
        label.startswith("libredox-")
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
    """Normalize a Cargo `links` name or metadata key the way Cargo builds DEP_* vars.

    Cargo uppercases the token and replaces every non-ASCII-alphanumeric character
    with `_`. Example: `aws_lc_0_42_0` + `include` → `DEP_AWS_LC_0_42_0_INCLUDE`.
    """
    out = ""
    for i in range(len(s)):
        c = s[i]
        if (c >= "a" and c <= "z") or (c >= "A" and c <= "Z") or (c >= "0" and c <= "9"):
            out += c.upper()
        else:
            out += "_"
    return out

def _dep_env_from_import_links(import_links):
    """Synthesize DEP_<LINKS>_* env vars for a buildscript that depends on `links` crates.

    The stock buck2-prelude `buildscript_run` does not implement Cargo's
    `package.links` metadata propagation. Without it, crates like aws-lc-rs
    panic with `missing DEP_AWS_LC_ include` because they never see the
    include path exported by aws-lc-sys.

    We approximate the common -sys crate convention: headers are staged under
    the dependency buildscript's OUT_DIR/include, and ROOT is OUT_DIR itself.
    Location macros create the necessary action edges so OUT_DIR is materialised
    before this buildscript runs.
    """
    env = {}
    for links_name, dep_label in import_links.items():
        token = _cargo_dep_token(links_name)
        out_dir = "$(location :" + dep_label + "-build-script-run[out_dir])"
        env["DEP_" + token + "_INCLUDE"] = out_dir + "/include"
        env["DEP_" + token + "_ROOT"] = out_dir
    return env

def _registry_crate(name, version, sha256, edition, label, alias, deps, features, build_script, proc_macro, build_script_root = "build.rs", lib_root = "src/lib.rs", crate_name = None, named_deps = {}, extra_env = {}, links = None, import_links = {}, patch = None, patch_strip = 1):
    archive_name = name + "-" + version + ".crate"
    crate_name = crate_name if crate_name else name.replace("-", "_")
    if patch != None:
        native.genrule(
            name = label + "-src",
            srcs = [":" + archive_name, ":" + patch],
            bash = (
                "set -euo pipefail\n" +
                "mkdir -p \"$OUT\"\n" +
                "cp -R \"$(location :" + archive_name + ")/.\" \"$OUT/\"\n" +
                "/usr/bin/patch -p" + str(patch_strip) + " -d \"$OUT\" < \"$(location :" + patch + ")\""
            ),
            out = label + "_patched",
            visibility = [],
        )
        src_ref = ":" + label + "-src"
        crate_root = label + "_patched/" + lib_root
    else:
        src_ref = ":" + archive_name
        crate_root = archive_name + "/" + lib_root
    deps = _gate_platform_deps(deps)

    # Split version into parts for CARGO_PKG_VERSION_* env vars
    _ver_parts = version.split(".")
    _ver_major = _ver_parts[0] if len(_ver_parts) > 0 else "0"
    _ver_minor = _ver_parts[1] if len(_ver_parts) > 1 else "0"
    _ver_patch_pre = _ver_parts[2] if len(_ver_parts) > 2 else "0"
    # Patch may include pre-release suffix like "0+wasi-snapshot-preview1"
    _ver_patch = _ver_patch_pre.split("+")[0].split("-")[0]

    pkg_env = {
        "CARGO_PKG_VERSION": version,
        "CARGO_PKG_NAME": name,
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
        "CARGO_CRATE_NAME": crate_name,
        # Some proc macros (e.g. wasm-bindgen) read CARGO_MANIFEST_DIR at expansion time.
        # Point it at the extracted crate archive directory which contains Cargo.toml.
        "CARGO_MANIFEST_DIR": "$(location :" + archive_name + ")",
    }

    native.http_archive(
        name = archive_name,
        sha256 = sha256,
        strip_prefix = name + "-" + version,
        urls = ["https://static.crates.io/crates/" + name + "/" + version + "/download"],
        visibility = [],
    )

    if build_script:
        cargo.rust_binary(
            name = label + "-build-script-build",
            srcs = [src_ref],
            crate = "build_script_build",
            crate_root = archive_name + "/" + build_script_root,
            edition = edition,
            features = features,
            visibility = [],
            deps = deps,
            named_deps = named_deps,
            env = pkg_env,
        )
        cargo.rust_library(
            name = label,
            srcs = [src_ref],
            crate = crate_name,
            crate_root = crate_root,
            edition = edition,
            features = features,
            proc_macro = proc_macro,
            visibility = ["PUBLIC"],
            deps = deps,
            named_deps = named_deps,
            env = dict(pkg_env, **{
                "OUT_DIR": "$(location :" + label + "-build-script-run[out_dir])",
            }),
            rustc_flags = ["@$(location :" + label + "-build-script-run[rustc_flags])"],
        )
        # Stock buck2-prelude `buildscript_run` has no `links` / `import_links`
        # attrs (passing them fails analysis). Propagate Cargo DEP_* metadata
        # ourselves via env: import_links → DEP_<LINKS>_{INCLUDE,ROOT} pointing
        # at each dependency's buildscript OUT_DIR (see _dep_env_from_import_links).
        bs_env = {
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
            "CARGO_CRATE_NAME": crate_name,
            "OPT_LEVEL": "2",
            "DEBUG": "false",
            "PROFILE": "release",
            "NUM_JOBS": "1",
            "PATH": "/nix/store/80bnpqsff7ijx09jf08arj5pvc7m0vhp-NuNuShell-dir/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        }
        bs_env.update(_dep_env_from_import_links(import_links))
        if links:
            bs_env["CARGO_MANIFEST_LINKS"] = links
        # extra_env last so package-specific overrides win.
        bs_env.update(extra_env)
        buildscript_run(
            name = label + "-build-script-run",
            package_name = name,
            buildscript_rule = ":" + label + "-build-script-build",
            features = features,
            version = version,
            rustc_link_lib = True,
            rustc_link_search = True,
            env = bs_env,
        )
    else:
        cargo.rust_library(
            name = label,
            srcs = [src_ref],
            crate = crate_name,
            crate_root = crate_root,
            edition = edition,
            features = features,
            proc_macro = proc_macro,
            visibility = ["PUBLIC"],
            deps = deps,
            named_deps = named_deps,
            env = pkg_env,
        )

    if alias:
        native.alias(
            name = crate_name,
            actual = ":" + label,
            visibility = ["PUBLIC"],
        )

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
    deps = _gate_platform_deps(deps)
    bs_deps = _gate_platform_deps(build_script_deps if build_script_deps != None else deps)

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
                "PATH": "/nix/store/80bnpqsff7ijx09jf08arj5pvc7m0vhp-NuNuShell-dir/bin:/usr/bin:/bin:/usr/sbin:/sbin",
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

def _patch_files(files):
    for f in files:
        parts = f.split("/")
        native.export_file(
            name = parts[-1],
            src = f,
            visibility = ["PUBLIC"],
        )

def third_party(registry, git_repos, patches):
    _patch_files(patches)
    for entry in registry:
        _registry_crate(**entry)
    for repo in git_repos:
        _git_repo(**repo)
