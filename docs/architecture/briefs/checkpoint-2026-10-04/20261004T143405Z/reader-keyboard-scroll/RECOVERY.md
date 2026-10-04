# Recovery checkpoint

Frozen Reader PageDown/PageUp/Home/End with current mounted visit, bounded viewport, exact native focus and shared Graph/Settings scroll-body admission. Mounted test source covers partial final pages, zoom/resize, native focus, native editor exclusion and Graph camera preservation. Source parse and diff checks passed. Rust and native captures remain unrun; not a visually accepted GUI integration.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
