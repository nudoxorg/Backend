alias(name = "heart",    actual = "//workspace/heart:heart",     visibility = ["PUBLIC"])
alias(name = "ir",       actual = "//workspace/ir:ir",           visibility = ["PUBLIC"])
alias(name = "registry", actual = "//workspace/registry:registry", visibility = ["PUBLIC"])
alias(name = "compiler", actual = "//workspace/compiler:compiler", visibility = ["PUBLIC"])
alias(name = "sandbox",  actual = "//workspace/compiler/sandbox:sandbox", visibility = ["PUBLIC"])

# ── Smoke test suite ─────────────────────────────────────────────────────────
# Aggregates every rust_tests()-generated integration test target across the
# workspace so `buck2 test //:smoke` is a single entry point (and `buck2 test
# //...` still covers unit tests too). rust_tests() names each target
# `test-<stem>` per `tests/*.rs` file, so this list is enumerated explicitly —
# Buck2's test_suite takes concrete labels, not cross-package globs.
#
# MAINTENANCE: regenerate after adding/removing a tests/*.rs file, e.g.
#   for p in workspace/compiler/sandbox workspace/heart workspace/registry \
#            workspace/compiler; do
#     for f in "$p"/tests/*.rs; do b=$(basename "$f" .rs); \
#       case "$b" in common|support) ;; *) echo "        \"//$p:test-$b\","; ;; esac; done
#   done
test_suite(
    name = "smoke",
    tests = [
        "//workspace/compiler/sandbox:test-cage_pool",
        "//workspace/compiler/sandbox:test-escape",
        "//workspace/compiler/sandbox:test-typestate",
        "//workspace/heart:test-access_control",
        "//workspace/heart:test-cache_roundtrip",
        "//workspace/heart:test-cache_stampede",
        "//workspace/heart:test-job_key",
        "//workspace/heart:test-refinements",
        "//workspace/heart:test-shared_types",
        "//workspace/registry:test-blob_assembly",
        "//workspace/registry:test-blob_hash_pins",
        "//workspace/registry:test-blob_store_roundtrip",
        "//workspace/registry:test-coordination",
        "//workspace/registry:test-global_id_determinism",
        "//workspace/registry:test-health_and_parse_status",
        "//workspace/registry:test-index_lifecycle",
        "//workspace/registry:test-metadata_guid_hash",
        "//workspace/registry:test-parse_coordination",
        "//workspace/registry:test-persistence_across_restart",
        "//workspace/registry:test-queue_priority_order",
        "//workspace/registry:test-registry_publish",
        "//workspace/registry:test-registry_search_tantivy",
        "//workspace/registry:test-reproducibility_persistence",
        "//workspace/compiler:test-generate_blob",
        "//workspace/compiler:test-ir_pipeline_index",
        "//workspace/compiler:test-linked_data_emit",
        "//workspace/compiler:test-nix_env_leak",
        "//workspace/compiler:test-nix_mini_flake",
        "//workspace/compiler:test-nix_render",
        "//workspace/compiler:test-nix_static_lower",
        "//workspace/compiler:test-parse_rust_to_ir",
        "//workspace/compiler:test-parse_typescript_to_ir",
        "//workspace/compiler:test-phase6_seal_tiers",
        "//workspace/compiler:test-python_classes",
        "//workspace/compiler:test-python_docstrings",
        "//workspace/compiler:test-python_imports",
        "//workspace/compiler:test-python_multimodule",
        "//workspace/compiler:test-python_oracle",
        "//workspace/compiler:test-python_overloads",
        "//workspace/compiler:test-python_version_select",
        "//workspace/compiler:test-references_e2e",
        "//workspace/compiler:test-references_live",
        "//workspace/compiler:test-rust_compiler_e2e",
        "//workspace/compiler:test-snap_csharp",
        "//workspace/compiler:test-snap_go",
        "//workspace/compiler:test-snap_java",
        "//workspace/compiler:test-snap_nix",
        "//workspace/compiler:test-snap_python",
        "//workspace/compiler:test-snap_rust",
        "//workspace/compiler:test-snap_typescript",
        "//workspace/compiler:test-treesitter_extraction",
        "//workspace/compiler:test-tsz_oracle",
        "//workspace/compiler:test-vcs_version_resolution",
    ],
    visibility = ["PUBLIC"],
)
