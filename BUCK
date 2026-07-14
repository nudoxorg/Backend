alias(name = "server",   actual = "//workspace/server:server",   visibility = ["PUBLIC"])
alias(name = "heart",    actual = "//workspace/heart:heart",     visibility = ["PUBLIC"])
alias(name = "ir",       actual = "//workspace/compiler/intermediate-representation:ir", visibility = ["PUBLIC"])
alias(name = "registry", actual = "//workspace/registry:registry", visibility = ["PUBLIC"])
alias(name = "runtime",  actual = "//workspace/runtime:runtime", visibility = ["PUBLIC"])
alias(name = "compiler", actual = "//workspace/compiler:compiler", visibility = ["PUBLIC"])
alias(name = "sandbox",  actual = "//workspace/util/sandbox:sandbox", visibility = ["PUBLIC"])
alias(name = "version",  actual = "//workspace/version:version",      visibility = ["PUBLIC"])
alias(name = "telemetry", actual = "//workspace/telemetry:telemetry", visibility = ["PUBLIC"])

command_alias(name = "add",    exe = "//build/third-party/tools:crates", args = ["add"],    visibility = ["PUBLIC"])
command_alias(name = "update", exe = "//build/third-party/tools:crates", args = ["update"], visibility = ["PUBLIC"])
command_alias(name = "check",  exe = "//build/third-party/tools:crates", args = ["check"],  visibility = ["PUBLIC"])
command_alias(name = "new",    exe = "//build/third-party/tools:crates", args = ["new"],    visibility = ["PUBLIC"])

command_alias(name = "tp-build", exe = "//build/third-party/tools:crates", args = ["build"], visibility = ["PUBLIC"])
command_alias(name = "tp-test",  exe = "//build/third-party/tools:crates", args = ["test"],  visibility = ["PUBLIC"])

# ── Smoke test suite ─────────────────────────────────────────────────────────
# Aggregates every rust_tests()-generated integration test target across the
# workspace so `buck2 test //:smoke` is a single entry point (and `buck2 test
# //...` still covers unit tests too). rust_tests() names each target
# `test-<stem>` per `tests/*.rs` file, so this list is enumerated explicitly —
# Buck2's test_suite takes concrete labels, not cross-package globs.
#
# MAINTENANCE: regenerate after adding/removing a tests/*.rs file, e.g.
#   for p in workspace/util/caching workspace/util/sandbox workspace/heart \
#            workspace/cas workspace/server workspace/registry workspace/runtime \
#            workspace/compiler; do
#     for f in "$p"/tests/*.rs; do b=$(basename "$f" .rs); \
#       case "$b" in common|support) ;; *) echo "        \"//$p:test-$b\","; ;; esac; done
#   done
test_suite(
    name = "smoke",
    tests = [
        "//workspace/util/sandbox:test-cage_pool",
        "//workspace/util/sandbox:test-escape",
        "//workspace/util/sandbox:test-typestate",
        "//workspace/heart:test-access_control",
        "//workspace/heart:test-cache_roundtrip",
        "//workspace/heart:test-cache_stampede",
        "//workspace/heart:test-job_key",
        "//workspace/heart:test-refinements",
        "//workspace/heart:test-shared_types",
        "//workspace/server:test-api_add_package",
        "//workspace/server:test-authz_cap_routing",
        "//workspace/server:test-client_surfaces",
        "//workspace/server:test-forge_coexistence",
        "//workspace/server:test-indexing_flow",
        "//workspace/server:test-initialization_flow",
        "//workspace/server:test-metadata_heuristics",
        "//workspace/server:test-pipeline_end_to_end",
        "//workspace/server:test-search_target",
        "//workspace/server:test-semantic_search_gating",
        "//workspace/server:test-server_config",
        "//workspace/server:test-symbol_store",
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
        "//workspace/registry:test-store_cas_roundtrip",
        "//workspace/runtime:test-embedding",
        "//workspace/runtime:test-graph_expansion",
        "//workspace/runtime:test-graph_structure",
        "//workspace/runtime:test-semantic_gate",
        "//workspace/runtime:test-session_merge_laws",
        "//workspace/runtime:test-session_store",
        "//workspace/runtime:test-text_search",
        "//workspace/runtime:test-tokenizer",
        "//workspace/runtime:test-typed_embedding",
        "//workspace/runtime:test-vector_similarity",
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
        "//workspace/compiler:test-rust_compiler_e2e",
        "//workspace/compiler:test-treesitter_extraction",
        "//workspace/compiler:test-vcs_version_resolution",
    ],
    visibility = ["PUBLIC"],
)
