# Recovery checkpoint

Latest frozen standalone Shelf source, including Ask/Spine native-focus restoration additions after the previous checkpoint. Source only, tests unrun. Includes the same known vendored cursor prefix bug recorded in the composed candidate; repair and acceptance remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
