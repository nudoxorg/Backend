# Recovery checkpoint

Complete unfinished integration source and original merge index, linked to all previously pushed slices and the newest borrowed witness and partial GUI work. Recovery checkpoint only. Last Rust library attempt failed to compile before tests; the fixture repair is staged remotely but its rerun is pending. The cache shell protocol harness passed and its atomic source commit is pushed. Native exploration captures were produced against an older binary and do not validate this checkpoint. No release, deployment or canonical integration acceptance is claimed. Existing worktree HEAD, index, merge and working source are preserved.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
