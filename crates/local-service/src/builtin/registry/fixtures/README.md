# Cargo registry archive fixture

`equivalent-1.0.2.crate` is the unmodified Cargo registry archive for
`equivalent` 1.0.2. Its archive SHA-256 is the value pinned for this package in
the registry checksum authority fixture. The crate declares `Apache-2.0 OR MIT`;
both license texts are included beside the archive and inside it.

The archive provenance and the known `src/lib.rs` member digest are recorded
in `provenance.json`. The test embeds the checked-in archive, so it does not
read Cargo's machine-local registry cache.
