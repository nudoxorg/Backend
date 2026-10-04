# Recovery checkpoint

Frozen in-progress claim-only stable-family c007 content producer, staged borrowed-reader wrapper with exact compilation attempt binding, cold history verification hook and focused claim-preservation source oracle. Production local/worker publication and metadata cutover are unfinished. No Cargo, build or test run; recovery only.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
