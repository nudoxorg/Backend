# Recovery checkpoint

Frozen in-progress Reader viewport intent, visit watermark, bounded keyboard command admission, Ask native hitbox occlusion, and regression oracles atop the committed Shelf/Reader composition. Latest history oracle is newly drafted; full review, parsing, Rust and native GUI validation remain pending. Recovery only.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
