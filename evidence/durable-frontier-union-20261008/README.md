Durable frontier union: source-bound work regression

The exact durable stage now unions immutable object IDs once in a BTreeSet instead of recomputing both objects’ content commitments for every preceding object. Input order, immutable identities, CAS admission, membership checks and publication boundaries are retained.

The unchanged physical fixture contains 132,096 logical pairs, 446 measured physical nodes and about 4.27 MB of selected payload. The original implementation fails its strict `commitments <= 64 * physical_nodes` control: 198,481 commitments and 1,906,926,126 hashed payload bytes. The fixed implementation uses 2,231 commitments and 21,331,373 hashed payload bytes, and the full store library passes all 275 tests. Single debug stage timings were 4,476,891 versus 2,557,272 microseconds; these are one fixture execution each, not release benchmarks or public-project latency claims.

The first apparent fixed run is invalid: Cargo reused the baseline image. Its raw logs remain in `stale-fixed-invalid-source/`. The accepted fixed run forced only the owned source mtime without changing bytes, observed real rustc in the fixed worktree, received Cargo `fresh=false`, and executed a different image with SHA-256 `94ca33a3032c1eacb1de219fce22a5d6849fba2c09c38fa1d332adb8ab9968a3`. Both native children kernel-waited with exit zero, and the warm graph was formally returned to GUI ownership.

Root independently checked all 22 raw members, their stored gzip round trips and hashes, all tracked store input bytes against the native source manifest, actual named test results and build-to-execution image correspondence. `inventory.json` maps compressed retained files to original bytes. Complete commands, plans, source manifests, receipts, guard samples, predecessor failure and graph return are retained. `root-audit.json` states the scope precisely.

Actual Docs, Mealie and PocketBase remove responsiveness must be retested on matching fixed CLI/MCP/service images. This packet does not clear those runtime failures.
