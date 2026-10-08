# Managed native-host Mac packaging checkpoint

Root reviewed the full source chain through `ab5f3cef2c85276ba4412a30295fd4c1889e2aaf`, including every incremental source and test correction. The six atomic changes replay onto canonical `f4790c5560e7617857b59eab3840087f8f036aef` without conflict. All five resulting packaging files are byte-identical to the reviewed source; 17,953 unrelated canonical tree entries and the current `f10d160e…` lock are preserved. The user's primary checkout and Nix edit were not changed.

The managed release path accepts a source-bound native-host build with four freshly emitted application executables. It binds the effective source, package graph, compiler tools, selected build plan and owner-produced receipt; it refuses untracked build inputs, symlinked graph/provenance locations, missing fresh application artifacts and mismatched receipt/image identities. Before signing, it checks all four current executable hashes and exact `0755` modes against the selected receipt and package manifest.

Root independently rehashed all 12 files in the sealed source packet and inspected its raw final test log: **49 ordinary Python fixtures passed**, zero failed or ignored, in 163.958 seconds. These fixtures include synthetic Cargo/tool receipts. They are packaging-contract tests, not a genuine Rust release build or installed-product proof. No release build, SDK native probes, signing, installation, GUI walkthrough or mtime refresh is credited here. [The evidence record](mac-native-host-checkpoint-evidence-20261008.json) records these boundaries explicitly.

The workflow trusts metadata produced by its authorized build/package owner. It checks an externally selected plan/source and current executable bytes; coordinated malicious rewriting of both owner-produced metadata files is outside this workflow's threat model.

The next gate is a fresh matching CLI/MCP/local-service/GUI release on the final integrated source, followed by ordinary default-path installation and live TypeScript, Python, Go and Rust projects. Existing historical binaries and renderer fixtures cannot satisfy that gate.
