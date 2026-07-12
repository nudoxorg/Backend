# Pinned to nightly toolchain commit (see flake.nix fenix-complete).
# The tarball is large (~300 MB); sha256 must be filled in after downloading:
#   nix-prefetch-url --unpack \
#     https://github.com/rust-lang/rust/archive/<LIBRUSTDOC_COMMIT>.tar.gz
# For everyday builds the driver is compiled via the cargo genrule in
# workspace/compiler/BUCK which reads vendor/librustdoc/ directly; the Buck
# http_archive entry here is provided for completeness / future hermetic builds.
LIBRUSTDOC_COMMIT = "f46ec5218fe7829ac18323b5ee0b409a63169f27"

PYREFLY_REV = "3e17a690edbde7d1f2341864d6f1c6523ba207c5"
RUFF_REV = "db5aa0a5f1b92cb91d910bf0866a967554dd94f5"
TERMINUS_REV = "4fefb0434f65baa847af5af9fc00175b3e4d827b"
LSP_TYPES_REV = "395d6bfcd6c3696a64cfe9cd93b86f981fb85112"

# deno_doc is built from source (not the crates.io registry) so we can apply
# patches/deno-doc/deno_doc-0.202.0.patch — it exposes the `params` module and
# drops an unreachable debug_assert. Deps/features mirror the 0.202.0 manifest.
DENO_DOC_REV = "0.202.0"

# rustdoc-types is vendored from the rust-lang git source (not the crates.io
# registry alias) so its FORMAT_VERSION is pinned to exactly match the nightly
# rustdoc our Rust producer shells out to. The pinned toolchain (see flake.nix
# fenix-complete, currently 1.98.0-nightly) emits rustdoc JSON at
# FORMAT_VERSION 60; crates.io rustdoc-types 0.56 targets FORMAT_VERSION 56, a
# 4-version drift that `serde_json::from_slice` previously tolerated only by
# luck. v0.60.0 == FORMAT_VERSION 60. The crate is fully `pub` and needs no
# patch to expose internals (unlike deno_doc); building from git rather than
# the registry keeps every `rustdoc-json-types`-derived type in one place and
# lets us bump the tag in lockstep with the toolchain. Default features only
# (no `rkyv_0_8`, no `rustc-hash`) — deps are just serde + serde_derive.
RUSTDOC_TYPES_REV = "v0.60.0"

# OXC monorepo — the TypeScript producer's parse/semantic/isolated-declarations
# substrate (replaces deno_doc; see OXC-PLAN.md). The oxc repo's *git tags*
# track applications (oxlint_v*, editor releases), NOT the library crate
# versions, so we CANNOT pin by tag. Instead we pin the exact commit SHA of the
# lockstep 0.139.0 crates release.
#
#   Commit:            69b2dfc1810e6ad9d5508ae9a20bc773ba5aa18d
#   PR/title:          release(crates): oxc v0.139.0 (#24218)
#   Date:              2026-07-06
#   Workspace version: every crates/oxc_* Cargo.toml has version = "0.139.0"
#                      (verified in [workspace.dependencies] at this SHA).
#
# Found by walking the Cargo.toml commit history via the GitHub API
# (`/repos/oxc-project/oxc/commits?path=Cargo.toml`) and taking the newest
# `release(crates): oxc v0.139.0` commit. edition 2024, MSRV 1.94.0, no build.rs
# in any crate (generated AST code is checked in under src/generated/).
OXC_REV = "69b2dfc1810e6ad9d5508ae9a20bc773ba5aa18d"

# tsz TypeScript compiler — vendored from git main (NOT crates.io 0.1.9).
# Git main has substantially higher TS conformance than 0.1.9; we pin the
# exact commit used for the OXC-PLAN §tsz oracle integration.
#
#   Commit:     dff7690987e3183847545feebe9c45ab6b83a006
#   Branch:     main
#   Date:       2026-07-12 (pinned for OXC-PLAN §tsz integration)
#   Workspace:  version = "0.1.48"
#   Edition:    2024 (all crates use workspace.package.edition = "2024")
#
# Wasm decision: wasm-bindgen and serde-wasm-bindgen are HARD (unconditional)
# deps in tsz-core, tsz-scanner, and tsz-parser — they are not behind any
# feature flag. The existing registry.bzl already carries wasm_bindgen-0_2 and
# serde_wasm_bindgen-0_6 (added by the prior agent). We keep them: native
# builds link wasm_bindgen fine (it compiles to stubs for non-wasm targets).
# tsz-wasm, tsz-cli, tsz-website, and conformance/ are NOT vendored here.
#
# build_script: tsz-common has a build.rs that only installs git hooks
# when a .git directory is present; it is a no-op in hermetic Buck2 builds
# (bails out immediately on CI=true or missing .git). Mark build_script:True
# so Buck does not silently skip it if the Starlark layer requires it, but
# it will produce no cargo metadata that affects compilation.
TSZ_REV = "dff7690987e3183847545feebe9c45ab6b83a006"

# oxc_resolver — ESM/CJS/`.d.ts` module resolution (resolve_dts). Separate repo
# `oxc-project/oxc-resolver`; its tags DO track the crate version, so pin the
# `v11.23.0` tag. Crate lives at the repo root (src/lib.rs). edition 2024.
OXC_RESOLVER_REV = "v11.23.0"

GIT = [
    # librustdoc is NOT fetched via http_archive (that would download ~300 MB of
    # rust-lang/rust). Instead, `scripts/vendor-librustdoc.sh` sparse-clones just
    # src/librustdoc into build/third-party/vendor/librustdoc/, applies the patch,
    # and a local_only genrule in build/third-party/BUCK builds it there via cargo.
    # See build/third-party/BUCK for the `librustdoc` genrule target.
    {
        "archive_name": "pyrefly-repo",
        "urls": ["https://github.com/facebook/pyrefly/archive/" + PYREFLY_REV + ".tar.gz"],
        "sha256": "3356755ba96cfc1cd31925f95ad12937974b7aa7eacdf8bcb8d99fa14cc1d020",
        "strip_prefix": "pyrefly-" + PYREFLY_REV,
        "crates": [
            {
                "name": "pyrefly_derive",
                "subdir": "crates/pyrefly_derive",
                "edition": "2024",
                "proc_macro": True,
                "deps": [":proc_macro2-1", ":quote-1", ":syn-2"],
            },
            {
                "name": "pyrefly_util",
                "subdir": "crates/pyrefly_util",
                "edition": "2024",
                "deps": [
                    ":anstream-0_6",
                    ":anyhow-1",
                    ":append_only_vec-0_1",
                    ":argfile-0_2",
                    ":compact_str-0_9",
                    ":dupe-0_9",
                    ":equivalent-1",
                    ":fxhash-0_2",
                    ":glob-0_3",
                    ":hashbrown-0_17",
                    ":human_bytes-0_4",
                    ":ignore-0_4",
                    ":index_vec-0_1",
                    ":itertools-0_15",
                    ":lock_free_hashtable-0_1",
                    ":lsp_types",
                    ":memory_stats-1",
                    ":notify-8",
                    ":parse_display-0_8",
                    ":path_absolutize-3",
                    ":pathdiff-0_2",
                    ":pyrefly_derive",
                    ":rayon-1",
                    ":ruff_notebook",
                    ":ruff_python_ast",
                    ":ruff_source_file",
                    ":ruff_text_size",
                    ":serde-1",
                    ":serde_json-1",
                    ":starlark_map-0_14",
                    ":static_assertions-1",
                    ":static_interner-0_1",
                    ":strsim-0_11",
                    ":tempfile-3",
                    ":thin_vec-0_2",
                    ":tracing-0_1",
                    ":tracing_subscriber-0_3",
                    ":uuid-1",
                    ":vec1-1",
                    ":watchman_client-0_9",
                    ":yansi-1",
                ],
            },
            {
                "name": "pyrefly_python",
                "subdir": "crates/pyrefly_python",
                "edition": "2024",
                "deps": [
                    ":anyhow-1",
                    ":clap-4",
                    ":dupe-0_9",
                    ":enum_iterator-2",
                    ":equivalent-1",
                    ":itertools-0_15",
                    ":lsp_types",
                    ":parse_display-0_8",
                    ":pathdiff-0_2",
                    ":pyrefly_util",
                    ":regex-1",
                    ":ruff_notebook",
                    ":ruff_python_ast",
                    ":ruff_python_parser",
                    ":ruff_text_size",
                    ":serde-1",
                    ":serde_json-1",
                    ":starlark_map-0_14",
                    ":static_interner-0_1",
                    ":tempfile-3",
                    ":thin_vec-0_2",
                    ":thiserror-2",
                    ":tracing-0_1",
                ],
            },
            {
                "name": "pyrefly_build",
                "subdir": "crates/pyrefly_build",
                "edition": "2024",
                "deps": [
                    ":anyhow-1",
                    ":dupe-0_9",
                    ":itertools-0_15",
                    ":pyrefly_python",
                    ":pyrefly_util",
                    ":regex-1",
                    ":serde-1",
                    ":serde_json-1",
                    ":starlark_map-0_14",
                    ":static_interner-0_1",
                    ":tempfile-3",
                    ":tracing-0_1",
                    ":vec1-1",
                    ":which-4",
                ],
            },
            {
                "name": "pyrefly_types",
                "subdir": "crates/pyrefly_types",
                "edition": "2024",
                "deps": [
                    ":compact_str-0_9",
                    ":dupe-0_9",
                    ":itertools-0_15",
                    ":num_bigint-0_4",
                    ":num_traits-0_2",
                    ":parse_display-0_8",
                    ":pyrefly_derive",
                    ":pyrefly_python",
                    ":pyrefly_util",
                    ":ruff_python_ast",
                    ":ruff_text_size",
                    ":starlark_map-0_14",
                    ":static_assertions-1",
                    ":vec1-1",
                ],
            },
            {
                "name": "pyrefly_config",
                "subdir": "crates/pyrefly_config",
                "edition": "2024",
                "deps": [
                    ":anyhow-1",
                    ":clap-4",
                    ":configparser-3",
                    ":convert_case-0_11",
                    ":derivative-2",
                    ":dupe-0_9",
                    ":enum_iterator-2",
                    ":itertools-0_15",
                    ":parse_display-0_8",
                    ":pyrefly_build",
                    ":pyrefly_python",
                    ":pyrefly_util",
                    ":regex-1",
                    ":regex_syntax-0_8",
                    ":serde-1",
                    ":serde_json-1",
                    ":serde_jsonrc-0_1",
                    ":serde_with-3",
                    ":starlark_map-0_14",
                    ":thiserror-2",
                    ":toml-1",
                    ":toml_edit-0_25",
                    ":tracing-0_1",
                    ":walkdir-2",
                    ":which-4",
                    ":yansi-1",
                ],
            },
            {
                "name": "pyrefly_graph",
                "subdir": "crates/pyrefly_graph",
                "edition": "2024",
                "version": "1.1.1",
                "deps": [
                    ":dupe-0_9",
                    ":pyrefly_util",
                    ":starlark_map-0_14",
                ],
            },
            {
                "name": "pyrefly_bundled",
                "subdir": "crates/pyrefly_bundled",
                "edition": "2024",
                "version": "1.1.1",
                "build_script": True,
                "build_script_deps": [
                    ":sha2-0_10",
                    ":tar-0_4",
                    ":zstd-0_13",
                ],
                "deps": [
                    ":anyhow-1",
                    ":starlark_map-0_14",
                    ":tar-0_4",
                    ":zstd-0_13",
                ],
            },
            {
                "name": "tsp_types",
                "subdir": "crates/tsp_types",
                "edition": "2024",
                "version": "1.1.1",
                "deps": [
                    ":lsp_server-0_7",
                    ":serde-1",
                    ":serde_json-1",
                    ":serde_repr-0_1",
                ],
            },
            {
                "name": "pyrefly",
                "subdir": "pyrefly",
                "edition": "2024",
                "version": "1.1.1",
                "crate_root_suffix": "lib/lib.rs",
                "deps": [
                    ":anstream-0_6",
                    ":anyhow-1",
                    ":arc_swap-1",
                    ":blake3-1",
                    ":capnp-0_25",
                    ":clap-4",
                    ":crossbeam_channel-0_5",
                    ":dashmap-6",
                    ":dupe-0_9",
                    ":enum_iterator-2",
                    ":faster_hex-0_6",
                    ":fuzzy_matcher-0_3",
                    ":fxhash-0_2",
                    ":indicatif-0_18",
                    ":itertools-0_15",
                    ":lsp_server-0_7",
                    ":lsp_types",
                    ":num_traits-0_2",
                    ":parse_display-0_8",
                    ":paste-1",
                    ":percent_encoding-2",
                    ":pyrefly_build",
                    ":pyrefly_bundled",
                    ":pyrefly_config",
                    ":pyrefly_derive",
                    ":pyrefly_graph",
                    ":pyrefly_python",
                    ":pyrefly_types",
                    ":pyrefly_util",
                    ":rayon-1",
                    ":regex-1",
                    ":ruff_annotate_snippets",
                    ":ruff_notebook",
                    ":ruff_python_ast",
                    ":ruff_python_parser",
                    ":ruff_source_file",
                    ":ruff_text_size",
                    ":serde-1",
                    ":serde_json-1",
                    ":serde_repr-0_1",
                    ":starlark_map-0_14",
                    ":static_assertions-1",
                    ":tempfile-3",
                    ":thin_vec-0_2",
                    ":tokio-1",
                    ":toml-1",
                    ":tracing-0_1",
                    ":tsp_types",
                    ":uuid-1",
                    ":vec1-1",
                    ":web_time-1",
                    ":xxhash_rust-0_8",
                    ":yansi-1",
                ],
            },
        ],
    },
    {
        "archive_name": "ruff-repo",
        "urls": ["https://github.com/astral-sh/ruff/archive/" + RUFF_REV + ".tar.gz"],
        "sha256": "8f4e600a1b71abce71f49ae3e1438e404fa0e668d755721532b91bcdf4bb6e2f",
        "strip_prefix": "ruff-" + RUFF_REV,
        "crates": [
            {
                "name": "ruff_text_size",
                "subdir": "crates/ruff_text_size",
                "edition": "2024",
                "features": ["get-size", "serde"],
                "deps": [":get_size2", ":serde-1"],
            },
            {
                "name": "ruff_source_file",
                "subdir": "crates/ruff_source_file",
                "edition": "2024",
                "features": ["get-size", "serde"],
                "deps": [":ruff_text_size", ":get_size2", ":memchr", ":serde-1"],
            },
            {
                "name": "ruff_python_trivia",
                "subdir": "crates/ruff_python_trivia",
                "edition": "2024",
                "deps": [
                    ":ruff_source_file",
                    ":ruff_text_size",
                    ":itertools-0_14",
                    ":unicode_ident",
                ],
            },
            {
                "name": "ruff_annotate_snippets",
                "subdir": "crates/ruff_annotate_snippets",
                "edition": "2024",
                "deps": [":anstyle", ":memchr", ":unicode_width"],
            },
            {
                "name": "ruff_diagnostics",
                "subdir": "crates/ruff_diagnostics",
                "edition": "2024",
                "deps": [":ruff_text_size", ":get_size2", ":is_macro"],
            },
            {
                "name": "ruff_macros",
                "subdir": "crates/ruff_macros",
                "edition": "2024",
                "proc_macro": True,
                "deps": [
                    ":ruff_python_trivia",
                    ":heck-0_5",
                    ":itertools-0_14",
                    ":proc_macro2",
                    ":quote",
                    ":regex",
                    ":syn-2",
                ],
            },
            {
                "name": "ruff_cache",
                "subdir": "crates/ruff_cache",
                "edition": "2024",
                "deps": [
                    ":filetime",
                    ":glob",
                    ":globset",
                    ":itertools-0_14",
                    ":regex",
                    ":seahash",
                    ":ruff_macros",
                ],
            },
            {
                "name": "ruff_python_ast",
                "subdir": "crates/ruff_python_ast",
                "edition": "2024",
                "deps": [
                    ":ruff_python_trivia",
                    ":ruff_source_file",
                    ":ruff_text_size",
                    ":ruff_cache",
                    ":ruff_macros",
                    ":aho_corasick",
                    ":bitflags-2",
                    ":compact_str-0_9",
                    ":get_size2",
                    ":is_macro",
                    ":memchr",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":thiserror-2",
                    ":thin_vec-0_2",
                ],
                "features": [
                    "cache",
                    "get-size",
                    "serde",
                    "compact_str/serde",
                    "thin-vec/serde",
                    "ruff_text_size/serde",
                ],
            },
            {
                "name": "ruff_python_parser",
                "subdir": "crates/ruff_python_parser",
                "edition": "2024",
                "deps": [
                    ":ruff_python_ast",
                    ":ruff_python_trivia",
                    ":ruff_text_size",
                    ":bitflags-2",
                    ":bstr",
                    ":compact_str-0_9",
                    ":get_size2",
                    ":memchr",
                    ":rustc_hash-2",
                    ":static_assertions",
                    ":thin_vec",
                    ":unicode_ident",
                    ":unicode_normalization",
                    ":unicode_names2",
                ],
            },
            {
                "name": "ruff_notebook",
                "subdir": "crates/ruff_notebook",
                "edition": "2024",
                "deps": [
                    ":ruff_diagnostics",
                    ":ruff_source_file",
                    ":ruff_text_size",
                    ":anyhow",
                    ":itertools-0_14",
                    ":rand-0_10",
                    ":serde",
                    ":serde_json",
                    ":serde_with",
                    ":thiserror-2",
                    ":uuid-1",
                ],
            },
        ],
    },
    {
        "archive_name": "terminusdb-rs-repo",
        "urls": ["https://github.com/ParapluOU/terminusdb-rs/archive/" + TERMINUS_REV + ".tar.gz"],
        "sha256": "b46576a8fb3be1ffab03a244084caf52766fc9e1d1bcf3e7d2b83076e0cd359e",
        "strip_prefix": "terminusdb-rs-" + TERMINUS_REV,
        "crates": [
            {
                "name": "terminusdb_schema",
                "subdir": "crates/schema",
                "edition": "2018",
                "patch": "remove-rocket.patch",
                "patch_strip": 6,
                "deps": [
                    ":serde",
                    ":serde_json",
                    ":itertools",
                    ":tempfile",
                    ":decimal_rs",
                    ":chrono",
                    ":enum_derive",
                    ":custom_derive",
                    ":exec_time",
                    ":rayon",
                    ":glob",
                    ":hashable",
                    ":enum_variant_macros",
                    ":pseudonym",
                    ":anyhow",
                    ":typestate",
                    ":uuid",
                    ":tap",
                    ":sha2",
                    ":serde_canonical_json",
                    ":pretty_assertions",
                    ":refined",
                    ":urlencoding",
                    ":tracing",
                ],
            },
            {
                "name": "terminusdb_schema_derive",
                "subdir": "crates/schema/derive",
                "edition": "2021",
                "proc_macro": True,
                "deps": [
                    ":darling",
                    ":syn-2",
                    ":proc_macro2",
                    ":quote",
                    ":heck",
                    ":regex",
                    ":itertools",
                    ":serde",
                    ":serde_json",
                    ":anyhow",
                    ":tracing",
                    ":tracing_subscriber",
                    ":multihash",
                    ":regexm",
                    ":chrono",
                    ":uuid",
                    ":terminusdb_schema",
                ],
            },
            {
                "name": "typestate",
                "subdir": "crates/typestate",
                "edition": "2018",
            },
        ],
    },
    {
        "archive_name": "lsp-types-repo",
        "urls": ["https://github.com/yangdanny97/lsp-types/archive/" + LSP_TYPES_REV + ".tar.gz"],
        "sha256": "2c9984223652831a3a49e689bd649ca5ed7898b764747890f2655c1ca52272e8",
        "strip_prefix": "lsp-types-" + LSP_TYPES_REV,
        "crates": [
            {"name": "lsp_types", "subdir": "", "deps": [":bitflags-1", ":serde-1", ":serde_json-1", ":serde_repr-0_1", ":url-2"]},
        ],
    },
    {
        "archive_name": "rustdoc-types-repo",
        "urls": ["https://github.com/rust-lang/rustdoc-types/archive/refs/tags/" + RUSTDOC_TYPES_REV + ".tar.gz"],
        "sha256": "29d0c94bef23df7f9d3e162692c67511fbedccf080153daae3bac64cdf99d5dc",
        "strip_prefix": "rustdoc-types-0.60.0",
        "crates": [
            {
                "name": "rustdoc_types",
                "subdir": "",
                "edition": "2024",
                "deps": [":serde-1", ":serde_derive-1"],
            },
        ],
    },
    {
        "archive_name": "deno-doc-repo",
        "urls": ["https://github.com/denoland/deno_doc/archive/refs/tags/" + DENO_DOC_REV + ".tar.gz"],
        "sha256": "3a94adbbbabdd40959655e8a90a849b6603bfb30d8f15d1954e410a20074d6c8",
        "strip_prefix": "deno_doc-" + DENO_DOC_REV,
        "crates": [
            {
                "name": "deno_doc",
                "subdir": "",
                "edition": "2024",
                "patch": "deno_doc-0.202.0.patch",
                "patch_strip": 1,
                "features": ["comrak", "rust"],
                "deps": [
                    ":anyhow-1",
                    ":cfg_if-1",
                    ":comrak-0_29",
                    ":deno_ast-0_53",
                    ":deno_graph-0_110",
                    ":deno_path_util-0_6",
                    ":deno_terminal-0_2_3",
                    ":handlebars-6",
                    ":html_escape-0_2",
                    ":indexmap-2",
                    ":itoa-1",
                    ":js_sys-0_3",
                    ":lazy_static-1",
                    ":percent_encoding-2",
                    ":regex-1",
                    ":serde-1",
                    ":serde_json-1",
                    ":serde_wasm_bindgen-0_6",
                    ":similar-2",
                    ":termcolor-1",
                    ":url-2",
                    ":wasm_bindgen-0_2",
                ],
            },
        ],
    },
    {
        "archive_name": "snix-repo",
        "urls": ["https://github.com/cachix/snix/archive/50b41ae7e05b423a7525b4dda53d372a71931506.tar.gz"],
        "sha256": "c53e20b429f5d4e7d5daad4bbfe49ac8b21c7e7c8f5e2946422ef1755dd5df96",
        "strip_prefix": "snix-50b41ae7e05b423a7525b4dda53d372a71931506",
        "crates": [
            {
                "name": "snix_eval",
                "subdir": "snix/eval",
                "edition": "2024",
                "patch": "snix-eval-50b41ae.patch",
                "patch_strip": 1,
                "named_deps": {
                    "builtin_macros": ":snix_eval_builtin_macros",
                },
                "env": {
                    # snix's `llvm_triple_to_nix_double` expects an LLVM/rustc
                    # triple (e.g. aarch64-apple-darwin), not a Nix double.
                    "SNIX_CURRENT_SYSTEM": "aarch64-apple-darwin",
                },
                "deps": [
                    ":bstr-1",
                    ":bytes-1",
                    ":codemap-0_1",
                    ":codemap_diagnostic-0_1",
                    ":data_encoding-2",
                    ":dirs-4",
                    ":genawaiter-0_99",
                    ":hashbrown-0_15",
                    ":itertools-0_12",
                    ":lexical_core-0_8",
                    ":md_5-0_10",
                    ":os_str_bytes-6",
                    ":path_clean-0_1",
                    ":regex-1",
                    ":rnix-0_11",
                    ":rowan-0_15",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":serde_json-1",
                    ":sha1-0_10",
                    ":sha2-0_10",
                    ":smol_str-0_2",
                    ":tabwriter-1",
                    ":thiserror-2",
                    ":toml-0_6",
                    ":vu128-1",
                ],
            },
            {
                "name": "snix_eval_builtin_macros",
                "subdir": "snix/eval/builtin-macros",
                "edition": "2024",
                "proc_macro": True,
                "deps": [
                    ":proc_macro2-1",
                    ":quote-1",
                    ":syn-1",
                ],
            },
        ],
    },
    # ── OXC monorepo (workspace version 0.139.0 @ commit OXC_REV) ──────────────
    # All crates share one tarball and reference each other as `:oxc_*` labels;
    # registry deps use `:<label>` from registry.bzl. No build.rs anywhere.
    # Feature flags per OXC-PLAN §2:
    #   oxc_allocator: `bitset` (required by oxc_semantic)
    #   oxc_ast:       NO `serialize`
    #   oxc_semantic:  default + `jsdoc` (pulls oxc_jsdoc); NO `cfg`
    #   oxc_syntax:    `to_js_string` (required by isolated_declarations; pulls
    #                  dragonbox_ecma)
    #   oxc_parser:    default (`regular_expression`)
    {
        "archive_name": "oxc-repo",
        "urls": ["https://github.com/oxc-project/oxc/archive/" + OXC_REV + ".tar.gz"],
        "sha256": "9481262a54bcfbf67feb66eaf0af15f1382dd7fb0701d9018ac383b1eab25567",
        "strip_prefix": "oxc-" + OXC_REV,
        "crates": [
            {
                "name": "oxc_ast_macros",
                "subdir": "crates/oxc_ast_macros",
                "edition": "2024",
                "version": "0.139.0",
                "proc_macro": True,
                "deps": [
                    ":phf-0_14",
                    ":proc_macro2-1",
                    ":quote-1",
                    ":syn-2",
                ],
            },
            {
                "name": "oxc_estree",
                "subdir": "crates/oxc_estree",
                "edition": "2024",
                "version": "0.139.0",
                # default features only (serialize disabled) → no deps.
                "deps": [],
            },
            {
                "name": "oxc_data_structures",
                "subdir": "crates/oxc_data_structures",
                "edition": "2024",
                "version": "0.139.0",
                # Superset of features requested across consumers: assert_unchecked,
                # code_buffer, fieldless_enum, inline_string, slice_iter, stack.
                # `rope` (→ ropey) is NOT enabled by any consumer here.
                "features": [
                    "assert_unchecked",
                    "code_buffer",
                    "fieldless_enum",
                    "inline_string",
                    "slice_iter",
                    "stack",
                    "string_ext",
                ],
                "deps": [],
            },
            {
                "name": "oxc_allocator",
                "subdir": "crates/oxc_allocator",
                "edition": "2024",
                "version": "0.139.0",
                "features": ["bitset"],
                "deps": [
                    ":oxc_data_structures",
                    ":allocator_api2-0_2",
                    ":hashbrown-0_17",
                    ":rustc_hash-2",
                ],
            },
            {
                "name": "oxc_str",
                "subdir": "crates/oxc_str",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_allocator",
                    ":oxc_estree",
                    ":compact_str-0_9",
                    ":hashbrown-0_17",
                ],
            },
            {
                "name": "oxc_span",
                "subdir": "crates/oxc_span",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast_macros",
                    ":oxc_estree",
                    ":oxc_str",
                    ":compact_str-0_9",
                    ":oxc_miette-3",
                ],
            },
            {
                "name": "oxc_diagnostics",
                "subdir": "crates/oxc_diagnostics",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":cow_utils-0_1",
                    ":oxc_miette-3",
                    ":percent_encoding-2",
                ],
            },
            {
                "name": "oxc_syntax",
                "subdir": "crates/oxc_syntax",
                "edition": "2024",
                "version": "0.139.0",
                "features": ["to_js_string"],
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast_macros",
                    ":oxc_estree",
                    ":oxc_index-5",
                    ":oxc_span",
                    ":oxc_str",
                    ":bitflags-2",
                    ":cow_utils-0_1",
                    ":nonmax-0_5",
                    ":phf-0_14",
                    ":unicode_id_start-1",
                    ":dragonbox_ecma-0_1",
                ],
            },
            {
                "name": "oxc_regular_expression",
                "subdir": "crates/oxc_regular_expression",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast_macros",
                    ":oxc_diagnostics",
                    ":oxc_span",
                    ":oxc_str",
                    ":bitflags-2",
                    ":phf-0_14",
                    ":rustc_hash-2",
                    ":unicode_id_start-1",
                ],
            },
            {
                "name": "oxc_ast",
                "subdir": "crates/oxc_ast",
                "edition": "2024",
                "version": "0.139.0",
                # NO `serialize` (default features = []).
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast_macros",
                    ":oxc_data_structures",
                    ":oxc_diagnostics",
                    ":oxc_estree",
                    ":oxc_regular_expression",
                    ":oxc_span",
                    ":oxc_str",
                    ":oxc_syntax",
                    ":bitflags-2",
                ],
            },
            {
                "name": "oxc_ast_visit",
                "subdir": "crates/oxc_ast_visit",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast",
                    ":oxc_span",
                    ":oxc_syntax",
                ],
            },
            {
                "name": "oxc_ecmascript",
                "subdir": "crates/oxc_ecmascript",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast",
                    ":oxc_regular_expression",
                    ":oxc_span",
                    ":oxc_syntax",
                    ":cow_utils-0_1",
                    ":num_bigint-0_5",
                    ":num_traits-0_2",
                    ":smallvec-1",
                ],
            },
            {
                "name": "oxc_jsdoc",
                "subdir": "crates/oxc_jsdoc",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_ast",
                    ":oxc_span",
                    ":rustc_hash-2",
                ],
            },
            {
                "name": "oxc_parser",
                "subdir": "crates/oxc_parser",
                "edition": "2024",
                "version": "0.139.0",
                # default feature `regular_expression` (→ dep:oxc_regular_expression).
                "features": ["default", "regular_expression"],
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast",
                    ":oxc_data_structures",
                    ":oxc_diagnostics",
                    ":oxc_ecmascript",
                    ":oxc_regular_expression",
                    ":oxc_span",
                    ":oxc_str",
                    ":oxc_syntax",
                    ":bitflags-2",
                    ":cow_utils-0_1",
                    ":num_bigint-0_5",
                    ":num_traits-0_2",
                    ":rustc_hash-2",
                    ":seq_macro-0_3",
                    ":memchr-2",
                ],
            },
            {
                "name": "oxc_semantic",
                "subdir": "crates/oxc_semantic",
                "edition": "2024",
                "version": "0.139.0",
                # default + jsdoc; NO cfg (so no oxc_cfg/petgraph).
                "features": ["jsdoc"],
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast",
                    ":oxc_ast_visit",
                    ":oxc_data_structures",
                    ":oxc_diagnostics",
                    ":oxc_ecmascript",
                    ":oxc_index-5",
                    ":oxc_jsdoc",
                    ":oxc_span",
                    ":oxc_str",
                    ":oxc_syntax",
                    ":itertools-0_15",
                    ":memchr-2",
                    ":rustc_hash-2",
                    ":self_cell-1",
                    ":smallvec-1",
                ],
            },
            {
                "name": "oxc_isolated_declarations",
                "subdir": "crates/oxc_isolated_declarations",
                "edition": "2024",
                "version": "0.139.0",
                "deps": [
                    ":oxc_allocator",
                    ":oxc_ast",
                    ":oxc_ast_visit",
                    ":oxc_diagnostics",
                    ":oxc_ecmascript",
                    ":oxc_span",
                    ":oxc_str",
                    ":oxc_syntax",
                    ":bitflags-2",
                    ":rustc_hash-2",
                ],
            },
        ],
    },
    # ── oxc_resolver (tag v11.23.0) ───────────────────────────────────────────
    # Crate lives at the repo root (src/lib.rs). Default features only (no
    # yarn_pnp/pnp, no document-features). Platform-gated deps (windows) are
    # gated by _gate_platform_deps in defs.bzl; simd-json/self_cell are
    # little-endian-only in Cargo but all our targets are little-endian, so they
    # are listed unconditionally. rustix is macos/linux-only in Cargo but not in
    # defs.bzl's gate lists — harmless on our darwin/linux targets.
    {
        "archive_name": "oxc-resolver-repo",
        "urls": ["https://github.com/oxc-project/oxc-resolver/archive/refs/tags/" + OXC_RESOLVER_REV + ".tar.gz"],
        "sha256": "ea0f2aa63b4cc6ee706f56996ed68dde0dc29601bba76a43bb127e8add6b5473",
        "strip_prefix": "oxc-resolver-" + OXC_RESOLVER_REV.lstrip("v"),
        "crates": [
            {
                "name": "oxc_resolver",
                "subdir": "",
                "edition": "2024",
                "version": "11.23.0",
                "deps": [
                    ":cfg_if-1",
                    ":compact_str-0_9",
                    ":fast_glob-1",
                    ":indexmap-2",
                    ":json_strip_comments-3",
                    ":memchr-2",
                    ":nodejs_built_in_modules-1",
                    ":once_cell-1",
                    ":dashmap-6",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":serde_json-1",
                    ":simdutf8-0_1",
                    ":thiserror-2",
                    ":tracing-0_1",
                    ":percent_encoding-2",
                    ":rustix-1",
                    ":simd_json-0_17",
                    ":self_cell-1",
                    ":windows-0_62",
                ],
            },
        ],
    },
    # ── tsz TypeScript compiler monorepo (git main @ TSZ_REV) ────────────────
    # Replaces the prior crates.io 0.1.9 registry entries (tsz_*-0_1 labels).
    # Library crates only; tsz-cli, tsz-wasm, tsz-website, conformance/ skipped.
    # Label scheme: tsz_core, tsz_common, tsz_scanner, tsz_parser, tsz_binder,
    #               tsz_solver, tsz_lowering, tsz_checker, tsz_emitter, tsz_lsp.
    # Intra-workspace deps reference each other as :tsz_* (no version suffix).
    # External deps use existing registry.bzl labels.
    {
        "archive_name": "tsz-repo",
        "urls": ["https://github.com/tsz-org/tsz/archive/" + TSZ_REV + ".tar.gz"],
        "sha256": "7180b171df202b2dd376b97899382ae85f547cf861a76e381955ae50f5ab21bc",
        "strip_prefix": "tsz-" + TSZ_REV,
        "crates": [
            # tsz-common: foundation types, shared utilities.
            # Has build.rs (installs git hooks — no-op in hermetic/CI builds).
            # Hard deps: serde, serde_json, rustc-hash, memchr, web-time.
            # No wasm deps here.
            {
                "name": "tsz_common",
                "subdir": "crates/tsz-common",
                "edition": "2024",
                "build_script": True,
                "deps": [
                    ":memchr-2",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":serde_json-1",
                    ":web_time-1",
                ],
            },
            # tsz-scanner: TypeScript tokenizer.
            # Hard deps: tsz-common, serde, unicode-ident, wasm-bindgen (unconditional).
            {
                "name": "tsz_scanner",
                "subdir": "crates/tsz-scanner",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":serde-1",
                    ":unicode_ident-1",
                    ":wasm_bindgen-0_2",
                ],
            },
            # tsz-parser: AST / parse tree types.
            # Hard deps: tsz-common, tsz-scanner, rustc-hash, serde, tracing,
            #            wasm-bindgen (unconditional).
            {
                "name": "tsz_parser",
                "subdir": "crates/tsz-parser",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":tracing-0_1",
                    ":wasm_bindgen-0_2",
                ],
            },
            # tsz-binder: name resolution / symbol binding.
            # Hard deps: tsz-common, tsz-scanner, tsz-parser, rustc-hash, serde,
            #            serde_json, tracing, smallvec, stacker (stack-growth).
            {
                "name": "tsz_binder",
                "subdir": "crates/tsz-binder",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_parser",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":serde_json-1",
                    ":smallvec-1",
                    ":stacker-0_1",
                    ":tracing-0_1",
                ],
            },
            # tsz-solver: type inference / constraint solving.
            # Hard deps: tsz-common, tsz-scanner, tsz-binder, rustc-hash, ena,
            #            bitflags, tracing, smallvec, fixedbitset, dashmap,
            #            serde, stacker.
            {
                "name": "tsz_solver",
                "subdir": "crates/tsz-solver",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_binder",
                    ":bitflags-2",
                    ":dashmap-6",
                    ":ena-0_14",
                    ":fixedbitset-0_5",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":smallvec-1",
                    ":stacker-0_1",
                    ":tracing-0_1",
                ],
            },
            # tsz-lowering: AST-to-type lowering bridge.
            # Hard deps: tsz-common, tsz-scanner, tsz-parser, tsz-solver,
            #            tsz-binder, indexmap, rustc-hash, tracing.
            {
                "name": "tsz_lowering",
                "subdir": "crates/tsz-lowering",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_parser",
                    ":tsz_binder",
                    ":tsz_solver",
                    ":indexmap-2",
                    ":rustc_hash-2",
                    ":tracing-0_1",
                ],
            },
            # tsz-checker: type checker.
            # Hard deps: tsz-common, tsz-scanner, tsz-parser, tsz-binder,
            #            tsz-solver, tsz-lowering, rustc-hash, tracing,
            #            smallvec, serde_json, dashmap, web-time, stacker.
            {
                "name": "tsz_checker",
                "subdir": "crates/tsz-checker",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_parser",
                    ":tsz_binder",
                    ":tsz_solver",
                    ":tsz_lowering",
                    ":dashmap-6",
                    ":rustc_hash-2",
                    ":serde_json-1",
                    ":smallvec-1",
                    ":stacker-0_1",
                    ":tracing-0_1",
                    ":web_time-1",
                ],
            },
            # tsz-emitter: TS→JS emitter / transforms.
            # Hard deps: tsz-common, tsz-scanner, tsz-parser, tsz-binder,
            #            tsz-solver, rustc-hash, memchr, serde_json, tracing.
            # NOTE: tsz-emitter does NOT depend on tsz-checker or tsz-lowering.
            # Features: default = ["dts"] (dts = [] — no extra code paths).
            {
                "name": "tsz_emitter",
                "subdir": "crates/tsz-emitter",
                "edition": "2024",
                "features": ["dts"],
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_parser",
                    ":tsz_binder",
                    ":tsz_solver",
                    ":memchr-2",
                    ":rustc_hash-2",
                    ":serde_json-1",
                    ":tracing-0_1",
                ],
            },
            # tsz-lsp: LSP server implementation.
            # Hard deps: tsz-common, tsz-scanner, tsz-parser, tsz-binder,
            #            tsz-solver, tsz-checker, rustc-hash, serde, serde_json,
            #            json5, tracing, web-time, globset, regex, stacker.
            # walkdir is cfg(not(target_arch = "wasm32")) — included for native.
            {
                "name": "tsz_lsp",
                "subdir": "crates/tsz-lsp",
                "edition": "2024",
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_parser",
                    ":tsz_binder",
                    ":tsz_solver",
                    ":tsz_checker",
                    ":globset-0_4",
                    ":json5-1",
                    ":regex-1",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":serde_json-1",
                    ":stacker-0_1",
                    ":tracing-0_1",
                    ":walkdir-2",
                    ":web_time-1",
                ],
            },
            # tsz-core: umbrella crate (re-exports full pipeline).
            # Hard deps: all sub-crates + anyhow, wasm-bindgen, serde,
            #            serde_json, serde-wasm-bindgen (all unconditional),
            #            rustc-hash, indexmap, rayon, tracing, once_cell,
            #            bincode (features: serde).
            # Features: default = ["dts"] (pulls tsz-emitter/dts).
            # NOTE: tsz-core does NOT depend on bitflags, dashmap, ena,
            #       fixedbitset, memchr, or smallvec directly — those are
            #       pulled in by sub-crates. The 0.1.9 registry entry listed
            #       them as direct deps; main's Cargo.toml does not.
            {
                "name": "tsz_core",
                "subdir": "crates/tsz-core",
                "edition": "2024",
                "features": ["dts"],
                "deps": [
                    ":tsz_common",
                    ":tsz_scanner",
                    ":tsz_parser",
                    ":tsz_binder",
                    ":tsz_solver",
                    ":tsz_lowering",
                    ":tsz_checker",
                    ":tsz_emitter",
                    ":tsz_lsp",
                    ":anyhow-1",
                    ":bincode-2",
                    ":indexmap-2",
                    ":once_cell-1",
                    ":rayon-1",
                    ":rustc_hash-2",
                    ":serde-1",
                    ":serde_json-1",
                    ":serde_wasm_bindgen-0_6",
                    ":tracing-0_1",
                    ":wasm_bindgen-0_2",
                ],
            },
        ],
    },
]
