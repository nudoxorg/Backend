# Recovery checkpoint

Frozen borrowed source witness and common checked graph constructor admission. Uses existing allocation-counter integration test instead of unsafe allocation instrumentation. Root requested canonical coordinate/reason validation for both full and borrowed constructors; source includes those independent rejection controls. Final incremental source review and Rust/allocation/storage execution remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
