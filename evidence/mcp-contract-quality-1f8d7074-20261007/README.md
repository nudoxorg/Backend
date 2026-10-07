# MCP contract regression gate

This receipt matches source commit `1f8d7074e7c059f1b154c7a8a1743e675e718f99`, tree `8cc3b5ac98e577abc96c69e19cff92050794db44`, and `Cargo.lock` SHA-256 `b508bc8978b37e09e54887a76cdb7055a4188f1243770057784a08ad61560268`.

The ILO native command used the pinned `cargo-wrapped-nix6 test --locked --offline --release -j2 -p backend-mcp --lib jsonrpc::tests -- --test-threads=1` runner, after fresh v5 admission 06. Result: **71 passed, 0 failed, 0 ignored**. Receipt exit code is `0`; `test-binary.sha256` identifies the exact binary reported by Cargo's `Running unittests` line. The pinned compiler, C compiler, wrapper hashes, and admission report are retained alongside the full test log.

The log includes inherited workspace warnings; this gate does not claim warning-free compilation. The preceding commit `c872237bc9a9709c658c0f35e6d757165c413654` had one existing compatibility assertion fail because schema admission intercepted an out-of-range integer before shared lowering. Commit `1f8d7074` keeps JSON type checks strict while routing range faults through shared lowering; this receipt includes the passing regression.
