# Recovery checkpoint

Frozen schema9 typed-key cutover with unified strict edge decoder, per-kind authority caps, moved-string decoding and one digest computation. Final optional/evidence corruption oracles are included. Final Root source review and Rust validation remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
