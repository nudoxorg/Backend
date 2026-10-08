# Exclusive writers and coherent read heads

This checkpoint separates the workspace's unique writer from the immutable
head used by readers. A detached writer can prepare durable publication while
queries retain the previous coherent workspace/library pair. A single-use grant
binds selection to its original owner and predecessor; foreign, stale, failed
and unwound settlements retain the writer and its work for recovery. A selected
durable publication is not reported as an unselected failure merely because its
HEAD acknowledgement failed.

Semantic image proofs are captured independently of the mutable publication
fence. Finish preparation borrows the captured snapshot. Read-head preparation
keeps the workspace head and projected library together, including their sink
failure and retry state. These APIs are a foundation for the compiler worker
and publication worker; this checkpoint does not move the complete index route
off the serving thread.

Root replayed nine atomic commits onto canonical
`66914f2997eb4acc1839e6d16bd4f5f9c00968a5`. All 25 subject paths match the
reviewed `b9e72e254d404c3636dd3c8d64fd004d2b12b8ac` checkpoint exactly.
The other 17,941 Git entries, canonical Cargo.lock, and the user's Nix
configuration remain unchanged. The accompanying JSON records exact hashes.

## Validation and its limits

Actual Linux native predecessor runs passed 13 writer controls on
`2043b6f9a762cb387c0619c4f244c1a59fa15bf9`, followed by five coherent-read-head
controls and one genuine multifile Rust image/source fence on
`31f32c4629e2d4a81e55408d4eb8b3c3977136c0`. The preserved producing images are
identified in the evidence. Root independently checked the latter run's
17,959 source blobs against both captured input manifests, Cargo artifact
identity, actual test output, exit status and process retirement. The image
digest is from the producing supervisor; Root did not rehash its 1.7 GB file.

Those predecessor source trees are not this canonical replay. The current
replay has not been compiled or run. A separate frozen worker successor is
queued to rerun all 13 writer and five read-head controls alongside 12 worker,
listener and stale-view controls against canonical's lock and APIs. Its result
must be recorded separately, including failures. This PR does not claim an
installed CLI/MCP/GUI result, final publication responsiveness, release
readiness, or a full cross-language application acceptance run.
