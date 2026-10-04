# Recovery checkpoint

Measured Shelf and GPUI prepaint restoration source, with real Shell drawer/resize oracles. Parsing and diff checks passed; Rust and native GUI acceptance remain unrun. Adversarial source review still identifies nonmultiple-height sticky/reveal geometry, first-frame reveal and Loading-to-Ready reveal-intent gaps. This is not accepted GUI integration.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
