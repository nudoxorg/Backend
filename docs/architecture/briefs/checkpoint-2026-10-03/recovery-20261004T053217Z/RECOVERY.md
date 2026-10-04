# Unfinished integration recovery checkpoint

This is an exact frozen working-source and merge-index recovery snapshot, **not an accepted integration, release or deployment candidate**. It retains both reviewed parent `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d` and concurrent canonical parent `76587b45c7abc378dfd5decfed51063cd2559aa6`. The real worktree and real merge index are preserved unchanged.

There are 4 unresolved merge-index paths and 0 paths containing conflict-marker text. Since the 05:05 checkpoint, seven more conflicts have been reviewed and staged. The snapshot includes the GUI cancellation/store/observer work, checked nonzero read IDs, the Turso retained-version disposition fix and its cold-reopen/corruption fixtures, and the corrected prepared-dispatch lease-owner expiry test.

**The typed platform creation-receipt refactor is incomplete and likely does not compile.** Its Windows helper methods, exports, tests and Tantivy integration remain missing. These changes are preserved to avoid losing in-progress work and must be completed before validation. No claim of full code review, successful Rust compilation, runtime tests, native GUI acceptance or deployment is made for this checkpoint.

`manifest.json` records the complete captured source paths and acceptance gaps. `merge-index-stages.nul` preserves the original mode/OID/stage/path records. `index-objects/` preserves otherwise-unreferenced index blobs, so staged and conflicted versions remain recoverable after pruning. To reconstruct the original index in a **separate checkout**, create a temporary `GIT_INDEX_FILE` with `git read-tree --empty`, then feed the NUL file to `git update-index -z --index-info` using that same variable. Never overwrite another checkout's index.

All workers were frozen before capture. The build gate remains held: the latest census found five confirmed workloads against the four-build limit, with ownership unresolved. Human configuration in the original checkout, unrelated processes, caches and runtime data remain untouched. The default `canonical` branch is unchanged. Resume source work in the existing integration worktree; do not deploy this recovery snapshot.
