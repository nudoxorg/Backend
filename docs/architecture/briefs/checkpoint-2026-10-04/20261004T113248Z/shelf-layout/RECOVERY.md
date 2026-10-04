# Recovery checkpoint

Frozen GPUI measured-list Shelf, wrapped Notes, semantic row/fraction history, sticky-aware keyboard reveal, disjoint splices, estimated offscreen extent probe and vendor uniform-height hint invariant repair. Mounted regression oracles included. Rustfmt parsing and scoped diff checks passed; final Root review, Cargo tests and native captures remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
