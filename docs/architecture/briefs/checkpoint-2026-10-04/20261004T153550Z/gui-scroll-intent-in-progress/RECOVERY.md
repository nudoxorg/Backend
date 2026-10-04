# Recovery checkpoint

Frozen in-progress Reader scroll ownership and Ask native occlusion fixes, atop the committed Shelf/Reader composition and Root vendored list cursor repair. Source recovery only; new interleaving tests, full Root review, Rust/native GUI validation remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
