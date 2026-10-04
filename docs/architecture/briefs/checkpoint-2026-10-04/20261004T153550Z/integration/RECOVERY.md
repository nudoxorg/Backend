# Recovery checkpoint

Quick user-requested recovery checkpoint preserving the unfinished integration and real index stages, latest Java fixture repair, borrowed typed graph/Java candidate, committed GUI composition with Root list prefix repair, and frozen in-progress Reader/Ask and owner-held Turso snapshot repairs. Canonical remains at 76587b45 pending validation. Last F library gate: 260 passed, 1 failed, 1 ignored; the Java fixture repair is committed but not rerun. Graph/allocation, GUI/native and snapshot/process acceptance remain pending. No real HEAD/index/MERGE_HEAD is changed.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
