# Recovery checkpoint

Quick checkpoint of the exact unfinished integration source and real index stages, with separately preserved latest GUI composition, standalone Shelf, borrowed typed graph d5a19a and owner-held Turso snapshot 14c68d2. No accepted merge, deployment or release. Latest f85 library gate: 260 passed, 1 failed, 1 ignored; remaining annotation fixture request fails InvalidSource. Post-run source/wrapper verification passed; client did not run. Full Root GUI review found a vendored cursor bug still pending repair. New graph and snapshot source/runtime acceptance remain pending. Original main HEAD/index/MERGE_HEAD are preserved.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
