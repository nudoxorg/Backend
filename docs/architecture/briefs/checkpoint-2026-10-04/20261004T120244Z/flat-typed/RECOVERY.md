# Recovery checkpoint

New frozen flat graph experiment over the exact imported typed-library baseline. Typed independent full-scan oracles include same-spelling Local/Purl identities, resolved reverse lookups, authorities, versions and cursor rejection. Separates admission, construction, first query and warm query; no measurements were run. Root full review and Rust validation pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
