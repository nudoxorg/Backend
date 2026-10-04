# Recovery checkpoint

Typed Local/Purl package identities, canonical ordering, exact lookups and v2 page cursor recipes. Resolved dependency target admission is tightened. Final patch exists; Root review remains incomplete and Rust tests are unrun.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
