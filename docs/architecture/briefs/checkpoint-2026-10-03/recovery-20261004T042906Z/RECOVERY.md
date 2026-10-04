# Unfinished integration recovery checkpoint

This is an exact frozen working-source and merge-index recovery snapshot. It is **not an accepted integration, release or deployment candidate**. The two parents retain reviewed source `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d` and concurrent canonical `76587b45c7abc378dfd5decfed51063cd2559aa6`. No existing worktree or real index is modified.

There are 16 unresolved index paths and 0 paths with working conflict-marker text. Since the previous checkpoint, 20 index conflicts were resolved and staged after source review, the client renewal syntax was corrected, the Turso attempt-history result materialization was bounded with corruption fixtures, the lease sweep stopped allocating a temporary ID vector, and additional GUI read-pool ownership/drain/cancellation fixtures were written. These current composed changes have not passed Rust tests or native acceptance.

The exact pending issues and captured paths are in `manifest.json`. The source-only service audit found prepared-encoding, frame-limit and request-exhaustion cleanup gaps; they remain pending. The Tantivy publisher and stage preparation refactor is unfinished. The static manifest audit does not replace locked Cargo resolution. No Cargo/build, Rust test, native interaction or deployment ran for this snapshot.

`merge-index-stages.nul` preserves exact mode/OID/stage/path records. `index-objects/` retains additional blobs so the original staged and conflicted objects remain reachable after pruning. To reconstruct this index in a **separate checkout**, initialize a separate temporary `GIT_INDEX_FILE` with `git read-tree --empty`, then feed the NUL file to `git update-index -z --index-info` with that same variable. Never replace an unrelated checkout's index. Continue the merge only after full review and the documented acceptance gates.

The default `canonical` branch is unchanged; this branch must not be deployed or merged as finished work. All workers were frozen before capture. Existing build limits and the held native/build gate are preserved. Human configuration and unrelated repositories, processes, caches and runtime data remain untouched.
