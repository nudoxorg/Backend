# Recovery checkpoint

Frozen flat graph and independent full-scan comparison harness separating admission, build, first query and warm queries. Typed-identity rebase and Root review remain pending. No Rust tests or benchmarks ran; no performance measurements.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
