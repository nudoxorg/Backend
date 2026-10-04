# Recovery checkpoint

Frozen in-progress owner-borrowing snapshot operation and cancellation/failure receipt repair. Includes digest slice compile fix and precreation apostrophe-path refusal for a pinned Turso parser limitation. Source recovery only; tests and deep review are unfinished, Rust/storage/process validation remains pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
