# Recovery checkpoint

Schema9 typed storage, unified strict edge decoder, independent per-kind authority caps, and exact-key source-state admission. Latest consolidation uses one SQL query for source, witness, optional state and indexed-edge presence, rejecting unavailable/unknown states with edges. The latest incremental Root review and all Rust validation remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
