# Recovery checkpoint

Canonical CargoHome shared-leaf cache admission fix, retaining final-entry symlink refusal and crate-type restrictions. Agent shell dispatch harness and syntax/diff checks passed. Root final owning-harness review and real compiler/cache performance validation remain pending; no speed claim.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
