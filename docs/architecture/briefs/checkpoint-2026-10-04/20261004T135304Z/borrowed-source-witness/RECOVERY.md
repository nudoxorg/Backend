# Recovery checkpoint

Frozen borrowed typed source-state witness API, shared v1 witness encoding and Turso read allocation avoidance. Golden, differential, shape, allocation and durable decoder tests were added but not executed. Full final Root source review and all Rust/storage gates remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
