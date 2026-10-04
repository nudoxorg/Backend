# Graph + Java validation packet

This is a local-only source packet for candidate `40abfeac823123ae752261e3eccffbaed978c994` (`cab3284ce01237a15268c90936f01930ed6d4c35`) over base `f85fb905ddc499bcfe701812ab68e971f7aad097`. The exact candidate is the `graph_java` receipt section: its parent is `d5a19a02268e45b9b0543240e50946708ba628cd`. The receipt's reviewed Java fixture is `0e18c888f70de46c410ddb7333391d661a94d650`.

`packet-preparation-v2` reuses the private source-packet verifier/stager structure from `/private/tmp/nudox-borrowed-typed-candidate-20261004T144100Z/packet-preparation-v2`, while the copied helpers here are independently pinned and scoped to this candidate. The exact changes from that prior helper set are:

- The allowlist grows from nine to ten paths by adding `crates/library/semantic_shape.rs`; the allocation test remains the only new path.
- The builder checks the selected receipt's `graph_java` identity and parent, checks that the candidate changes only `semantic_shape.rs` from its `d5a19` parent, and verifies that file's Git blob is exactly the reviewed Java fixture blob.
- The builder reads each selected file directly from the pinned candidate Git tree, derives a ten-path source patch, and emits a selected receipt carrying the original input-receipt hash, exact ten source SHA-256 values, and exact Git blob IDs. The GUI source changes are not part of this packet.
- The verifier pins the ten changed-file SHA/blob/size tuples and requires the final path set to equal the base plus the one allocation-test file. The stager pins the selected receipt, source patch, archive, manifests, verifier, and package receipt.
- Python protocol checks now include a positive ten-file overlay fixture, refusal to reclassify the new path as base content, third-change rejection, lock/gitlink mismatch rejection, archive traversal/unknown/duplicate/type rejection, and exact new-file type/mode checks.

The packet and helper hash inventory is in `packet-preparation-v2/helper-manifest.json`. The Python checks cover packet validation and a disposable fixture containing only the ten changed targets. They do not claim full-tree materialization or Rust acceptance. No APFS clone, release staging, transfer, Cargo, build, Nix, or runtime action was performed.
