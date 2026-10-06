# Joined source gates, 2026-10-06

These are source/test gates, not whole-package acceptance or a release receipt.

The library and unified-terminal runs used `3c6998cf671c4687361810cc8d47136072e9c501`, whose tracked tree equals Root `3a3f283316f29c0883d23e9841ea59d96f214dbc` (tree `2da38b45973ce2b0e201e856025d794348704ada`). Both guarded receipts record unchanged source/toolchain and exit 0. `backend-library --lib` passed 291 tests, with one existing ignored test; its stdout was observed in the agent execution transcript and was not separately saved. `unified_terminal` passed all 3 tests; its complete log is attached. These do not include the later service-certificate fix.

The actual service certificate regression ran against `a31d2daea` plus the qualified Python fixture repair, in the isolated query-continuation worktree. Both tests pass: serialized production certificates for names/search, first/successor/terminal pages, absent/explicit-empty/nonempty manifests, and negative family/page-credit/manifest substitution. Full command: `cargo test -p backend-local-service --lib query_page_certificate --locked --offline`. Build 10m21s; tests 0.01s. It tests the service certificate producer, rather than constructing a valid certificate in the client fixture.

The joined complete-package history observer passed 72 tests. A genuine retained Pi three-image publication independently passed the strict bounded history identity/lineage gate; its original runtime cohort remains DTO19. This is not input replay or full package acceptance.

The first-page query failure was observed in the actual matched Mac DTO20 runtime and is preserved in the separate successor-macos report. A matched-image rerun of the new service-certificate fix is still required. The new full library suite is running separately. Warning counts remain substantial; these gates are not warning-free validation.
