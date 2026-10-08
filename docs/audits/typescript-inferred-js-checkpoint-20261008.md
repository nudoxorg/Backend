# Inferred JavaScript roots and four-language acceptance checkpoint

Configless JavaScript and JSX files were captured as project inputs but silently omitted by TypeScript's default `allowJs: false`. The host now enables `allowJs` only when no tsconfig was selected and the inferred roots contain `.js`, `.jsx`, `.mjs` or `.cjs`. It clones compiler options, leaves `checkJs` unchanged, and preserves explicit configuration policy. Missing admitted roots produce a typed local error; the existing privacy-safe wire failure remains unchanged.

## Source review and canonical preservation

Root read all production and test changes in native source `4ba330c3ec78d7ed538a276e012be3363bc49e2d`, tree `52f338b0fe64afa61f87ccf0f8705c4f2f86adb9`. Its three changed files replay exactly on canonical `0dd104dbe83a3a0623f88db40f883257c05ebba5`. All17,944 other canonical entries remain byte-identical. Cargo.lock remains `de731929bbf72c5220e59c0543aaddd06fcc2bc16899741a9d6cb5546e60780e`. The exact combined canonical tree has not been built; the native witness binds the identical three-file production change on its preceding canonical base.

## Actual native witness

The genuine engine test `application::typescript_program::tests::configless_javascript_roots_are_admitted_without_checking_js` executed once: **1 passed,0 failed,0 ignored**,915 filtered. Its original Cargo invocation produced a new `fresh:false` executable, then failed inside the test because the isolated runtime contained only `typescript.js` and omitted default declaration libraries. The original failure and its missing-global diagnostics remain intact. The retained same-device executable was rerun directly with the complete133-file TypeScript5.9.3 package and Node24.18.0. No source/target/build-role access or binary rebuilding occurred during that rerun.

The image SHA256 is `bf0c380c44c12fa54045200a81e3f608cd3b34234bef9e0b9af0c045308b435d`; TypeScript API SHA256 is `3ae902c92cc44dace175c0e69e13a4b0899f6983c6121d76b9ab8dd5795e7675`. Raw source inventory matches17,944 Git blobs. Before/after SDK inventories and image hashes agree. The child was directly kernel-waited, its process group disappeared and its runtime permit was released. The remote run used fresh destination capacity admission. Root independently checked the raw output, producing Cargo event, source map, input correspondence and retirement receipts. The1.337GB executable remains on ILO; Root did not claim to rehash local executable bytes.

Native archive: `/private/tmp/luna-caddy-inferred-js-native-run-4ba-20261008/sealed-evidence.tar.gz`, SHA256 `80e2bc723cd81495e979a50d5de7627b387c9b520cad03775b00b29cc00e6a83`. The checked result is retained in [the evidence record](typescript-inferred-js-native-evidence-20261008.json). The standalone Root record SHA256 before copying is `9d5e723f4082f3adab4a63b3afcdd7b43188066d84439631b1cdf491b34dd24d`.

## Separate installed HTTPie diagnostic

Pinned HTTPie `5b604c37c6c67e18e7c3e9aee6c88a8c22b98345` was tested with the genuine installed GNU c001 CLI/MCP/locald release, complete SDK, ordinary PATH and fresh platform-default HOME state paths. Its state was placed on an explicitly authorized private RAM-backed mount after the normal-disk run timed out during a durable genesis write. It compiled133 Python artifacts and resolved the native `HTTPieArgumentParser` class at `httpie/cli/argparser.py:138` and its compiler caller at `tests/test_cli.py:242`.

All17 listed MCP tools returned accepted responses, including real remove/index/start/progress and terminal ticket controls. The additional catalog Search More route also executed. CLI reads, both MCP transports, plain native graph and limit1 page traversal worked. The ORIGINAL graph cursor returned the same page2 records after actual Search More interleaving and complete owner restart; no replacement token was substituted. Root independently verified490 raw file hashes,99 correlated responses,81 complete tool-response byte counts and the original cursor/record equality. Three prior observer mistakes are preserved and classified separately.

This is one qualified application witness from older source `c0016d4f4fc9b0f0a452c7c9556954e2287948b3`, not current-canonical acceptance, normal-disk performance, a controlled storage comparison or physical durability. Semantic embeddings were unconfigured. The200-result full query fits; no byte-credit exhaustion is claimed. Active cancellation, optional catalog lanes and current installed four-language replay remain open.

Ledger: `/Users/mileswirht/Downloads/sol61-real-app-tail-matrix-20261008/currentc001-installed-public-preparation/RAM-closed-final-20261008/result-ledger.json`, SHA256 `cda160128068c39670d24199be5ccffa04604ce66e853ebaf09611c0905184b5`. Raw archive SHA256 `e3197209c94dc84e4f8a703fbef22d6c472c184f8a7d4b0bef02ced4b2aa961f`.

## Open four-language gates

Go's separate753a helper passes eight native controls/four platform subcases; Rust-side lowering and installed Go replay are still pending. Rust6a381 has reviewed source and11 queued native controls, no native pass yet. TypeScript member resolution, enum diagnostics and duplicate body publication remain under repair. Python's current PR80 package-root source has predecessor native evidence, not a new installed release. Prepared publication, Tantivy join, current GUI release and full warnings/Clippy gates remain open. This checkpoint changes no installed binary or installer.
