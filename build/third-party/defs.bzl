load("@prelude//rust:cargo_package.bzl", "cargo")
load("@prelude//rust:cargo_buildscript.bzl", "buildscript_run")

def _registry_crate(name, version, sha256, edition, label, alias, deps, features, build_script, proc_macro, build_script_root = "build.rs", lib_root = "src/lib.rs", named_deps = {}, extra_env = {}):
    archive_name = name + "-" + version + ".crate"
    src_ref = ":" + archive_name
    crate_name = name.replace("-", "_")

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
            env = pkg_env,
        )
        cargo.rust_library(
            name = label,
            srcs = [src_ref],
            crate = crate_name,
            crate_root = archive_name + "/" + lib_root,
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
        buildscript_run(
            name = label + "-build-script-run",
            package_name = name,
            buildscript_rule = ":" + label + "-build-script-build",
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
                "CARGO_CRATE_NAME": crate_name,
                "OPT_LEVEL": "2",
                "DEBUG": "false",
                "PROFILE": "release",
                "NUM_JOBS": "1",
                "PATH": "/nix/store/80bnpqsff7ijx09jf08arj5pvc7m0vhp-NuNuShell-dir/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            }, **extra_env),
        )
    else:
        cargo.rust_library(
            name = label,
            srcs = [src_ref],
            crate = crate_name,
            crate_root = archive_name + "/" + lib_root,
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

def _git_crate(archive_name, crate_name, subdir, edition, deps, features, proc_macro, patch):
    normalized = crate_name.replace("-", "_")

    if patch != None:
        native.genrule(
            name = normalized + "-src",
            srcs = [":" + archive_name, ":" + patch],
            bash = (
                "mkdir -p \"$OUT\"\n" +
                "cp -R $(location :" + archive_name + ")/" + subdir + "/. \"$OUT\"/\n" +
                "cd \"$OUT\"\n" +
                "patch -p1 < $(location :" + patch + ")"
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

    cargo.rust_library(
        name = normalized,
        srcs = [":" + normalized + "-src"],
        crate_root = normalized + "/src/lib.rs",
        crate = normalized,
        edition = edition,
        features = features,
        proc_macro = proc_macro,
        visibility = ["PUBLIC"],
        deps = deps,
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
