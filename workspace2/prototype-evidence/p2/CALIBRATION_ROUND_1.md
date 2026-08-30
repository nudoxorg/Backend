# P2 control card: calibration round one raw record

## Specimen

`CONTROL_CARD.md` at manager commit `c4dad629416178de531a552af5f10e6a14dddf7d`, SHA-256 `e61c4a58c944323d7f9a337e3d65fb8148fccf973d64a6dc0330186e3ee874b0`.

## Explicit child-model and transport proof

| Role | Task ID | Explicit model | Fork | Result |
|---|---|---|---|---|
| independent reader | `/root/p2_manager/p2_control_cold_reader` | `gpt-5.6-luna` | `none` | completed read-only restatement; rejected calibration |
| blind reviewer | `/root/p2_manager/p2_control_reviewer` | `gpt-5.6-terra` | `none` | completed read-only review; B1/B2/B3 blockers and M1/M2/M3 majors |

## Reader output retained verbatim in substance

The Luna reader correctly restated the first terminal, exact paths, baseline, nine evidence rows, and 272/246/26 production plus 225/200/25 test budget. It found the following literal omissions: public journal/receipt/result names and visibility; header/frame offsets, version and checksum preimages; first sequence and durable-end arithmetic; exact successful fixture; malformed header/sequence behavior; error variants and cleanup composition; permitted deterministic fault seam; directory-sync platform policy; one-file versus transient directory descriptor; exact allocation proof and clean-tree procedure. Its direct conclusion was that these omissions could let a builder broaden or substitute the terminal.

## Reviewer output retained verbatim in substance

The Terra reviewer scanned the frozen card and absent candidate path set and independently restated the same first terminal, caps, negative space, and next decision. It reported:

1. **BLOCKER B1:** a 92-byte frame contains 8-byte sequence + 68-byte record + 16-byte checksum, leaving no stored frame-version cell although checksum prose required it; header bytes/checksum semantics were unspecified. Required correction: exact byte diagram, preimage, mutation priority, and golden bytes.
2. **BLOCKER B2:** real successful `sync_all` cannot prove the effect was not formed before sync; deterministic production-transition write/file-sync faulting was prohibited/UNVERIFIED. Required correction: a narrow no-queue, no-async private fault seam and exact sync-failure assertion.
3. **BLOCKER B3:** private receipt claim lacked literal public surface, private fields, construction/conversion prohibition, and compile-fail evidence. Required correction: name exact types, private receipt fields, forbidden conversions/shared typestate, and downstream compile-fail fixtures.
4. **MAJOR M1:** directory-sync failure was required but not falsifiable. Fold it into B2's seam with exact typed error/no journal.
5. **MAJOR M2:** post-sync in-memory state replacement was only implied. State it and test every success plus a forced sync failure.
6. **MAJOR M3:** manifest/dependency/nested-workspace/current status law was not executable. Freeze direct dependencies, expected tree, and clean-status steps.

The reviewer’s strongest counterexample was: write a complete `PublicationStarted` frame, construct/retain `Publish` before `sync_all`, return it only after a success sync, then pass all real-file happy-path tests. A forced sync error would expose this, but the original card did not authorize one. It cleared the standard-library exclusive `File` control as the simplest representation; the failure was proof grammar, not a reason to add service machinery. It retained device cache, Windows/NFS, cross-process, and physical power-loss gaps as unverified.

## Calibration result and repair

Round one failed. The active card was replaced as one canonical body in the next manager checkpoint; no production edit authority was issued. The prior digest is retained only by this committed evidence record and Git history.
