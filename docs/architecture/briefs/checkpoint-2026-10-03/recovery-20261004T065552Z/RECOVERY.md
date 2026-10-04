# Integration recovery checkpoint

This is an exact frozen working-source and merge-index recovery snapshot, **not an accepted integration, release or deployment candidate**. It retains both parent `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d` and concurrent canonical parent `76587b45c7abc378dfd5decfed51063cd2559aa6`. The real worktree and real merge index are preserved unchanged.

There are 0 unresolved merge-index paths and 0 paths containing conflict-marker text. This captures the GUI page-generation/exhaustion slice, whole-call reset hydration, original-publication disposition, prepared-dispatch and lease expiry source, platform/Tantivy ownership work, and the latest incomplete Turso and Qdrant storage redesigns. The Qdrant state module is present but unfinished; remote operation guards, active pins and exact stale-residence digests are not integrated. Source writers acknowledged a freeze before capture.

Final source review, dependency resolution and owning-callsite integration are pending. No Rust compilation, test execution, native GUI acceptance, production-readiness or deployment success is claimed. `manifest.json` records the specific gaps, including Unix creation provenance and the new Windows test macro import correction.

`merge-index-stages.nul` preserves original mode/OID/stage/path records. `index-objects/` preserves otherwise-unreferenced index blobs so staged versions remain recoverable after pruning. To reconstruct the original index in a **separate checkout**, create a temporary `GIT_INDEX_FILE` with `git read-tree --empty`, then feed the NUL file to `git update-index -z --index-info` using that same variable. Never overwrite another checkout's index.

All owned Cargo, builds, tests and native runs remain held. The original checkout and its human configuration, unrelated processes, caches and runtime data remain untouched. The default `canonical` branch is unchanged. Resume source work in the existing integration worktree; do not deploy this recovery snapshot.
