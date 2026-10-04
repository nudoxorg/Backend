# Recovery checkpoint

Exact 13-path Shelf/Reader composition on f85. Source only and not accepted. Root found a serious vendored ListState focus-handle cursor bug: seeking to the target before slicing the prefix drops preceding rows. Repair and count/order/geometry regression oracle remain outstanding. GUI Rust gates, native captures and full Root source review remain pending.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
