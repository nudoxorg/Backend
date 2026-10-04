# Integration recovery checkpoint

This is an exact frozen working-source and merge-index recovery snapshot, **not an accepted integration, release or deployment**. It retains parent `4d6bf5fee050a3ec0a5fac5e07a2938e4609322d` and concurrent canonical parent `76587b45c7abc378dfd5decfed51063cd2559aa6`. The real worktree and original merge index are preserved unchanged.

All GUI/backend source is captured, including the latest compiler type repairs, paired graph/admission and Turso read/publication tests, Tantivy warm-hit maintenance avoidance and strict UTF-8 motion sidecar parsing. There are 0 unresolved index paths and 0 conflict-marker paths. The separate partial flat graph layout experiment and two committed implementation slices are preserved on additional recovery branches.

Older candidate `13d9919ae6a9fc4d662c5ffb322462c6e3dfb25d` passed `backend-platform --lib` on h16001mac: **70 passed, zero failed**. Its client gate stopped with four library compile errors; no client tests ran. The current source repairs those diagnostics. Immutable candidate `9a896bf197aed9028e9dbf14f0532eb00daad077` contains three atomic commits but is **unrun**. The latest motion protocol test selection passed **33 tests**; 18 other methods and native runtime acceptance remain unrun. Current-source GUI/native acceptance, the typed package-reference alias cutover, full storage validation, remote deployment and a real live ingest backup remain pending. See manifest.json for exact distinctions and evidence hashes.

`merge-index-stages.nul` preserves original mode/OID/stage/path records. `index-objects/` preserves otherwise-unreferenced staged blobs. To reconstruct the original index in a **separate checkout**, create a temporary `GIT_INDEX_FILE` with `git read-tree --empty`, then feed the NUL file to `git update-index -z --index-info` using that same variable. Never overwrite another checkout's index.

The original checkout and its human configuration, unrelated processes, caches and runtime data remain untouched. The default canonical branch is unchanged. Resume in the existing integration worktree; do not deploy this recovery snapshot.
