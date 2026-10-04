# Recovery checkpoint

PARTIAL SOURCE: lease-before-stage publication and tracked exclusive-file creation receipts preserve the original Windows delete-capable handle. Latest receipt-removal outcomes and StageLease retry retention are partly implemented. Windows callers/tests still need adapting to the changed helper signature; this slice may not compile. Latest formatting, Root review and all builds/tests unrun. Worker explicitly froze source for this checkpoint.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
