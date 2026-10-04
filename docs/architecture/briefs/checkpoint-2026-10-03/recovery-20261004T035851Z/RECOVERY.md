# Unfinished reconciliation recovery checkpoint

This branch preserves the frozen working source and exact merge-index stages. It is a recovery snapshot, not an accepted integration or deployment candidate. The reviewed source parent is `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d`; the concurrent canonical parent is `76587b45c7abc378dfd5decfed51063cd2559aa6`. Both histories are retained as parents of this recovery commit.

The actual integration worktree and its original index are untouched. Its 36 unmerged index paths remain pending. The snapshot includes the new owned `crates/client/src/reset_hydration.rs` file. Remaining working conflict markers: 0 paths. Absence of marker text does not establish type correctness or completed review. The lease renewal source has a known missing semicolon, and GUI pool test ports and the merged lock validation remain unfinished. No build, Rust test, native action, or service deployment ran for this checkpoint.

`manifest.json` identifies the source tree before adding these recovery files, original index digest, changed paths, pending index conflicts, and acceptance limits. `merge-index-stages.nul` is the exact NUL-delimited output of `git ls-files --stage -z`. The `index-objects` directory retains index blobs that differ from the captured working-source entries, so staged and three-way conflict contents survive Git object pruning.

For inspection, check out this recovery branch in a new worktree. To reconstruct the original merge index there, use a separate temporary index: initialize it empty with `git read-tree --empty`, then feed `merge-index-stages.nul` to `git update-index -z --index-info`, with the same `GIT_INDEX_FILE` on both commands. This must not overwrite an unrelated checkout's index. The source paths in this branch retain the frozen working contents; the manifest records the intended merge parent for continuing the reconciliation.

The default canonical branch is not changed by this checkpoint. Do not deploy or merge this recovery branch as a completed integration.
