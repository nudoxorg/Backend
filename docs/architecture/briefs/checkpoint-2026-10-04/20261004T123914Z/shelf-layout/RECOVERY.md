# Recovery checkpoint

Frozen measured Shelf and GPUI List source. Latest proportional restoration occurs inside List prepaint before layout freezes; sticky chain uses current-height bottom-anchored clipping. Adds real Shell drawer reopen/wheel and first-resize oracles; root.rs changes are tests only. Parsing and diff checks passed. Latest full Root review, Rust tests and native captures pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
