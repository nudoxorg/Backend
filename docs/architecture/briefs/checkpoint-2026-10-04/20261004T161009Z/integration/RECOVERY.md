# Recovery checkpoint

User-requested quick checkpoint preserving the unfinished integration, exact real index stages, current GUI viewport/history source, in-progress stable-family compiler/IR publication, committed cancellation-safe owner snapshot, and the standalone Turso VACUUM literal fix as a verified Git bundle plus patch. Canonical stays at 76587b45. Graph/Java source is staged remotely but not yet run. Latest completed F library gate is 260 passed, 1 failed, 1 ignored. New slices remain unaccepted and runtime/native validation is pending. No real HEAD/index/MERGE_HEAD is changed.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
