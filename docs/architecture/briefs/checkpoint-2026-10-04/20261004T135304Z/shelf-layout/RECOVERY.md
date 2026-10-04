# Recovery checkpoint

Latest partial Shelf source: ordinary GPUI List/Div prepaint compositor, initial reveal and Loading wait intent, item focus and wheel APIs, and drafted Shell tests. Source syntax/diff checks passed; every Cargo, mounted and native acceptance gate is unrun. Hidden Shelf focus ownership still needs review. This is preservation of unfinished GUI work.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
