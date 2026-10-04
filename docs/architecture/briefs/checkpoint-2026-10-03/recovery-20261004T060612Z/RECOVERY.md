# Unfinished integration recovery checkpoint

This is an exact frozen working-source and merge-index recovery snapshot, **not an accepted integration, release or deployment candidate**. It retains both reviewed parent `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d` and concurrent canonical parent `76587b45c7abc378dfd5decfed51063cd2559aa6`. The real worktree and real merge index are preserved unchanged.

There are 2 unresolved merge-index paths and 0 paths containing conflict-marker text. This checkpoint includes the checked GUI read identities and restored real-worker failure oracles; shared client request-loop refactoring and corrected socket tests; the Turso original-publication disposition fix and fixtures; prepared-dispatch service fixes and post-encode lease-expiry tests; and the coherent source-only platform creation-receipt/Tantivy slice.

The Turso generation namespace module is still an **incomplete, unintegrated draft**. Final source review and dependency resolution are pending for portions of the integration. No claim of successful Rust compilation, runtime tests, native GUI acceptance, production readiness or deployment is made.

`manifest.json` records captured source paths and acceptance gaps. `merge-index-stages.nul` preserves the original mode/OID/stage/path records. `index-objects/` preserves otherwise-unreferenced index blobs, so staged and conflicted versions remain recoverable after pruning. To reconstruct the original index in a **separate checkout**, create a temporary `GIT_INDEX_FILE` with `git read-tree --empty`, then feed the NUL file to `git update-index -z --index-info` using that same variable. Never overwrite another checkout's index.

Source writers were frozen before capture. The build gate remains held: the latest census confirms three local Zig builds plus two unresolved conservative candidates, against the four-build limit. Human configuration in the original checkout, unrelated processes, caches and runtime data remain untouched. The default `canonical` branch is unchanged. Resume source work in the existing integration worktree; do not deploy this recovery snapshot.
