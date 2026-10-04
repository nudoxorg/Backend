# Recovery checkpoint

Measured GPUI Shelf, wrapped Notes, semantic row/fraction history, sticky-aware reveal, disjoint splices and uniform height hint repair. Latest partial saved refactor uses owner tickets to revoke stale deferred restore/reveal, child-prepaint settlement and sticky clipping. Worker stopped explicitly for this checkpoint. Latest refactor is source-partial/unvalidated; full Root review, Rust tests and native captures pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
