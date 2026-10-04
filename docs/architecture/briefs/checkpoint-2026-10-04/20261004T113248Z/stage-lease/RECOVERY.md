# Recovery checkpoint

Partial Tantivy lease-before-stage lifecycle, token/identity-bound cleanup, lock held through publication, committed-cleanup refusal, and durable warm admission before sweeps. Unreviewed and unvalidated source. Platform create_file_exclusive can fail after creation without returning an ownership receipt: this cut aborts before stage creation and refuses name-based marker deletion.

This preserves frozen working source, not an accepted integration or release. No real worktree index, HEAD, branch, or merge state was changed. The default canonical branch was not moved.

`merge-index-stages.nul` and `index-objects/` preserve staged-only content. To reconstruct the index, use a separate checkout and temporary GIT_INDEX_FILE; feed the NUL records to git update-index -z --index-info after git read-tree --empty. Do not overwrite the existing integration index.
