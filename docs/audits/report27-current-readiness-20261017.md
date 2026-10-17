# Report 27: current verification checkpoint

Updated 2026-10-17, 17:54 UTC. Current native validation targets commit `469b56895b6c17a540987819b55c76b3a31e1abe`, tree `0ab05d6d3d87b9f0d0ab7fc6156a722c0da9993f`. Full report closure and application acceptance remain open. The [historical checkpoints](report22-current-readiness-20261013.md) retain earlier observations.

## Current native result and repair

Root independently replayed the closed current38-R2 run at source `0f23e973f49619131929aeb4731c7bf8e5bf9837`: **13 passed, zero failed, zero ignored and 281 unrun** of 294 selected bodies. Six parser bodies and seven rendering bodies ran. The other bodies were blocked by compilation, rather than failed test assertions.

The actual compiler reported E0502 in TSZ `cross_file.rs`: an immutable symbol borrow remained live across a mutable merged-library-symbol lookup. All-crate Clippy consequently exited 101; parser and rendering Clippy exited zero. The three runs emitted 8,717 warning records, deduplicated by Luna to 6,267 diagnostic sites. No clean-Clippy claim is made.

Commit `469b56895b6c17a540987819b55c76b3a31e1abe` adds a fresh symbol lookup after the mutable lookup, allowing the earlier borrow to end. Root, Sol 6.1 and Luna reviewed the actual diagnostic and the complete surrounding method. The repair preserves the existing alias, namespace, library-origin and shadowing guards. Current39 all-crate/test Clippy has now completed successfully on this exact repair: exit zero, zero compiler errors and 31,088 warning records. Root independently streamed and parsed all 33,803 raw JSON records (79,347,505 bytes), verified the unique final successful Cargo record and matched the result receipt. Raw SHA-256: `8512b25da42ddd5b30818f0d9bdda9712c0ea79fa8cf036d6487eff41a73b591`. This clears the observed E0502 blocker; it does not establish a warning-free build or any test-body pass.

Root's current38-R2 replay verified all 463 courier members and 122,095,406 bytes, the exact 18,536-blob source, 533 metadata files, 137 ELF path identities covering 125 inodes, retained images, ECHILD closure, permit release and complete resource accounting. Replay SHA-256: `a65b63399b380a633f1e3867b64cfdd13c7facc73c6d08cac90779cd63e622bc`. The ordinary application build was not reached in that run. The fixed resource epoch was not reset and no retained data was credited back.

## Current39 execution

The selected current39 scope uses the exact `469b56895b6c17a540987819b55c76b3a31e1abe` source above. It runs the same 294 selected bodies and three separate Clippy phases, followed by an independently reported build of the four default-feature application binaries, executable copies and loader inspection. Earlier passes give it no test credit.

Two preparation attempts stopped before a permit, actor or Cargo command: the first referenced a helper before staging; the second completed staging and a full read-only preflight, then correctly refused to overwrite an existing guard filename. Root and Luna reviewed the narrow controller corrections. All failed evidence and the 31 staged inputs remain preserved.

Root launched the explicit staged-data resume at **17:44:05 UTC**. It authenticates the existing inputs, accounting receipt, untouched source and empty actor namespace; repeats the full preflight and fresh fleet admission; and uses distinct, exclusively created preflight/final guard names. Managed destination custody remains mandatory. The held scope is now running real Cargo on ILO. Root separately verified the genuine source transition, clean target commit/tree and all 18,537 source blobs. All-crate/test Clippy completed as described above; remaining native-body, lint, ordinary-build, loader and application results are still pending. No selected-body credit comes from metadata, Clippy or dispatch.

## Earlier ordinary binaries

Source `1fc3e0a3ab703c34d0af4288fab6dfd1e29df4e7`, tree `b0a7a53279d06511c2c4b81e0ba10929971dfe90`, built all four default-feature applications on ILO in 227.03 seconds, with zero compiler errors and 1,706 warnings. Root independently verified 344 courier files, exact Cargo artifacts, independent executable copies, loader dependencies, source preservation and scope closure.

| Binary | SHA-256 |
| --- | --- |
| CLI | `9317cb0ead8a7745d38bf3ea5b79417662d4012cc3c71b83b02cbff51ecfcfdb` |
| MCP | `927cabbfe9deefea767f4f0b0387296840ca362a2f0cae4bcac1920165579b88` |
| locald | `ddfc54ec64adf7a1162e1cffdadba858bf3e689619be1003b53cadd0e627b3c3` |
| Desktop | `53ea53896b6dedce41c981c14fb56f3dd77213fdfc2ded3f6cdc21d08524b05d` |

Root adoption SHA-256: `16a218108a26cdf1530f6b52339094e0cf2ad850d7f560e6d7483640099cb02b`. These remain older experiment candidates. Their direct ELF GLIBC floor is 2.39; Ubuntu transitive compatibility and release acceptance remain unproved. They are not current39 binaries.

## Application gates

| Gate | Current status |
| --- | --- |
| TypeScript CLI/MCP | A prior Angular scope acquired its permit but refused entry on a stale nested runner hash. Zero application or MCP calls ran. The current39 caller derives and checks that hash before admission; its actual full search, declaration identity, graph, pagination, cancellation, retry and restart run remains unrun. |
| Python and Go | Current39 callers for the full Docs and PocketBase applications are prepared. Docs requires all 18 tool postconditions, genuine cancellation, CLI retry with MCP joining the same operation, and complete outline/document pagination. Preparation is not application evidence. The earlier PocketBase JavaScript-root diagnostic does not establish Go acceptance. |
| GUI | A prior run drew Rust frames but exposed no visible PID-owned X11 window; its black image was a desktop fallback. Missing private CargoHome and an unaccounted tool symlink also blocked the harness. Reviewed current39 driver corrections preserve strict window ownership checks. Actual current39 GUI acceptance remains unrun. |
| Original reproductions | Exact ASP/GymBroApp sources remain unavailable here. Representative projects cannot close those reproductions. |
| Remaining findings | Registry latency, row-count accounting, HTTP MCP auto-start, original N15, configured vector search and broad-corpus acceptance remain open. |
| Delivery | Historical installer/startup-help checks remain separate. No current39 release, Homebrew proof, Ubuntu compatibility proof or current Mac bundle is established. |

## Integration and resources

Root freshly fetched coworker refs from Forgejo. Canonical is `399cc9f63c9572dba1ec98ac9d8a5f56a33f98f6` after reviewed [PR 500](https://dev.nudox.org/git/Nudox/Backend/pulls/500) and [PR 501](https://dev.nudox.org/git/Nudox/Backend/pulls/501). Root merged both and fetched/verified their exact resulting trees. The changes remove five unused engine test imports, confirmed unused projection imports and a dead duplicate viewport calculation. Test bodies, assertions and the later dimension checks remain. These narrow cleanups do not establish a complete warning-free build and were not independently rebuilt.

The TypeScript repair and reviewed integration are tracked in [PR 498](https://dev.nudox.org/git/Nudox/Backend/pulls/498). Current39's frozen validation source remains the exact commit identified above even when the PR receives this documentation or the canonical cleanup merge.

Heavy compilation stays on ILO. Every admission includes fresh local, ILO and Prod accounting; the freshly offline Mac is explicitly excluded under the user's authorization. Prod lacks destination disk for another workload. No local heavy build was started. The unreadable Downloads checkout was not reset, replaced or edited, and the user's preexisting Nix change remains untouched. No production-readiness claim is made.
