# Integration recovery checkpoint

This is an exact frozen working-source and merge-index recovery snapshot, **not an accepted integration, release or deployment candidate**. It retains both parent `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d` and concurrent canonical parent `76587b45c7abc378dfd5decfed51063cd2559aa6`. The real worktree and real merge index are preserved unchanged.

There are 0 unresolved merge-index paths and 0 paths containing conflict-marker text. This captures all current GUI/backend source, including running-job revocation, Seeded failure admission, client reset hydration, owner-borrowed SQL reconciliation, coherent registry graph snapshots, platform/Tantivy ownership work, borrowed canonical IR rows and the Turso/Qdrant storage slices. All active source writers acknowledged freeze before capture.

The previous exact checkpoint `d691327900c8a2050d40090d14b08c5f9217afe7` ran `backend-platform --lib` on h16001mac: **69 passed, 1 failed, exit 101**. An exact existing-binary replay reproduced the Darwin fixture failure; its new deterministic source fixture remains untested. A separate `backend-client --lib` lane **exited 101 while compiling backend-semantic**, with ten missing `CapacitySpace` import diagnostics; **no client tests ran**. The current source tree has not been compiled or tested. Current native GUI acceptance and live deployment remain pending. `manifest.json` records the gaps and precise older-test provenance.

`merge-index-stages.nul` preserves original mode/OID/stage/path records. `index-objects/` preserves otherwise-unreferenced index blobs so staged versions remain recoverable after pruning. To reconstruct the original index in a **separate checkout**, create a temporary `GIT_INDEX_FILE` with `git read-tree --empty`, then feed the NUL file to `git update-index -z --index-info` using that same variable. Never overwrite another checkout's index.

The original checkout and its human configuration, unrelated processes, caches and runtime data remain untouched. The default `canonical` branch is unchanged. Resume work in the existing integration worktree; do not deploy this recovery snapshot.
