# Recovery checkpoint

Lease-before-stage publication, held-lock cleanup and warm admission before sweeping. Latest source adds a tracked exclusive-file creation receipt carrying post-create failures through admission and identity-bound rollback; Windows retains the original delete-capable handle. Worker stopped explicitly for checkpoint. Platform fault oracles and final static review are unfinished, all builds/tests unrun.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
