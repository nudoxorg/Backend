# N4 private workspace directory gates: final evidence

Scope: the N4 private-directory implementation and its focused native Linux gates. This is a source/evidence package, not an integration claim for Root’s later startup-platform composition.

## Tested source lineage

Base: 96b04b141affd653e3c6932dc8c325f351775f8f.

| Commit | Tree | Change |
|---|---|---|
| 2affbee78e786f260aac4ece899ccdf7d40848b6 | a7394ed98d47a7dd33cf11dbf175981a2c27dc3e | Create workspace state directories privately under any umask |
| 0645b61c6e0486aa6bea2b323035409038bb2f93 | 5bdf51660d07bf0be41b2018e1e407bc3ec29ae7 | Create registry discovery roots with private permissions |
| 20642fd7902c9feb21164ee6a42ef9fd9d948a04 | a05937c10c0e8c1f7b8fba15adcddef770619bb1 | Fix private Tantivy cache test fixture |
| 7e4f511211e0aaf756a6e471d7c91138f5af1fbc | 1ea2888aaa4ffe3b0b23015406e256e409ce43dd | Use rustix to create FIFO test fixture |
| 95ee463a27eb4deb330a14ad403ff48b96181d68 | 5b531e4a1d8459835b749f74b2e18a2c82232d02 | Create private roots for lease child tests |
| 001d8249ad95bbd1a441a1eff732add04d91b3ac | 201d4cb64eeb7ab50e7e8788eb47fa7e77e508ad | Create private roots in registry process tests |
| b90fbedad21e1faffb577bc5fb49a76b6f3c2fc2 | a8a1fe972fa3dd9615a8823f0e55bfb25a3e4f52 | Create private root for forge worker tests |
| dab7d3494f2b0310c005dfddc6364f2123cd1361 | 548cab7cbbf7434adf311d69cc903b4db4f5f4e8 | Keep OSV spool files private |

Final N4 source is dab7d3494f2b0310c005dfddc6364f2123cd1361, tree 548cab7cbbf7434adf311d69cc903b4db4f5e8. Relative to b90fbedad21e1faffb577bc5fb49a76b6f3c2fc2, the only source difference is six insertions in crates/local-service/src/builtin/registry.rs, setting anonymous OSV spool files to mode 0600.

Across base to final source, the change is 19 files, 634 insertions and 80 deletions. The production change uses an owned private-directory capability for constructors, checks trusted parent/final ownership and modes, creates new roots as 0700 without chmodding existing directories, and sets the anonymous spool file to 0600. The root reviewed the complete production and fixture diff before this package was prepared.

## Native gate results

Each row is an independent filtered test invocation; counts are not summed. Raw logs, launch stamps where available, admissions, and test-binary hashes are included in this package.

| Filter | Result | Source evidence | Raw log |
|---|---|---|---|
| backend-platform | 62 passed, 0 failed | b90 tree; unchanged through dab | raw/platform-store-tantivy-b90/cargo.log |
| backend-store | 275 passed, 0 failed | b90 tree; unchanged through dab | raw/platform-store-tantivy-b90/cargo.log |
| Tantivy | 96 passed, 0 failed | b90 tree; unchanged through dab | raw/platform-store-tantivy-b90/cargo.log |
| backend-engine acquisition | 91 passed, 0 failed | exact dab commit/tree, clean before and after | raw/engine-acquisition-dab/ |
| backend-engine registry | 127 passed, 0 failed | exact dab commit/tree, clean before and after | raw/engine-registry-dab/ |
| backend-engine forge | 28 passed, 0 failed, 1 ignored | b90 tree; engine/forge source unchanged through dab; ignored case is the official network smoke | raw/engine-forge-b90/cargo.log |
| backend-local-service registry | 43 passed, 0 failed | exact dab commit/tree, clean before and after | raw/local-service-registry-dab-final-v7/ |
| backend-local-service discovery | 69 passed, 0 failed, 8 ignored | exact dab commit/tree, clean before and after | raw/local-service-discovery-dab/ |

The b90-to-dab diff touches only local-service builtin registry code for spool permissions. Therefore the platform/store/Tantivy and engine/forge results cover source unchanged through dab; acquisition and engine registry were also rerun on exact dab. Local-service registry and discovery have exact-dab launch stamps. The b90 raw logs do not include per-run launch.txt stamps; their raw test output, binary hashes, admitted census records, and source-diff equivalence are retained separately rather than inventing a launch record.

A prior local-service registry run on b90 produced 42/43 because the anonymous spool file was 0644 instead of the required 0600. Its raw v6 log and launch stamp are under raw/local-service-registry-b90-red-v6/; it is superseded by the exact-dab 43/43 pass under raw/local-service-registry-dab-final-v7/ and is not part of the final pass counts. The exact-dab v7 log was independently checked to contain 43 passed, 0 failed.

The final broad 3-library run was launched with no-fail-fast and reported all three full filtered counts. The earlier clean-0645 REDs are preserved: Tantivy 95/96 in the initial broad-audit log and platform 61/62 in the following three-library log. The subsequent fixture commits repaired the Tantivy temp-root fixture and replaced external mkfifo dependence with a rustix FIFO fixture. The original clean-0645 evidence archive remains at /private/tmp/sol61-n4-native-audit-receipts.tar.gz; its supplied SHA-256 is 67737926e9a41386884a95012936f042d33af695e67f95c00abe8f50ae00016. It was copied unchanged and that original archive was not rehashed.

## Fresh fleet admission and retirement

Every final job had a fresh allowed admission, with the admission JSON included under census/. For the exact-dab runs: local-service registry used the 17:54:03Z allowed sample (12/16 fleet groups); local-service discovery used the 18:17:13Z allowed retry (13/16); engine acquisition used the 18:18:38Z allowed sample (12/16); engine registry used the 18:37:39Z allowed retry (15/16). Earlier same-name discovery/registry samples were denied and are included to show they did not admit jobs. For the b90 runs, final platform/store/Tantivy, engine acquisition, engine registry, and forge admissions were allowed at 5/16, 7/16, 9/16, and 10/16 fleet groups respectively. No new forge job was admitted after the parent asked to stop.

Kernel retirement receipt: raw/kernel-retirement/luna-n4-kernel-retirement-20261007T1855Z.txt. At 18:45:44Z it found no cargo/rustc/test process in the borrowed joined-native worktree and both its slot-0 lock and the shared slot-2 release lock were free. The worktree was then restored cleanly to branch codex/luna-go-236d-native-20261007 at commit 236d4c1689ee622d99dd0de606a05e1010fdb51f, tree 7e09540f7963b89cf825f6350a9f152822013496. The N4 source remains available in the local private-workspace checkout and the N4 ref on ILO.

## Limits

This does not prove repair of pre-existing shared/group-writable application caches; strict refusal is intentional and the tested path is fresh-root creation. Windows runtime was not tested. The capability cannot keep a durable path pin after the caller drops it. Root’s later joined startup-platform composition still needs its own native guard before claiming joined-source native coverage.

## Related preserved evidence

The exact verified CLI/MCP/locald inventory is /root/sol61-linux-joined-evidence-20261006/build-a659d5d181/runtime-build-manifest.json (source a659d5d1819078b3c0046937191a9b54a76b1dfb, tree d76c5ea835ff0b326c24e7e9d3a2cc9ece277dbb). This inventory is not external application acceptance.

Older Go-lane receipts remain under /root/sol61-linux-joined-evidence-20261006: bounded platform readlink passed at luna-go-platform-readlink-20261007T013544Z/receipt.json; Debian captured-root admission passed at luna-go-2c2d-debian-real-20261007T020957Z/receipt.json; unrelated-language startup at luna-go-236d-owner-startup-20261007T021917Z/native-test-image.json failed at the TypeScript authority refusal before Python generation. These are older Go-lane results, not N4 proof.



Packaging audit note: the earlier archive /private/tmp/luna-n4-final-evidence-20261007.tar.gz (SHA-256 1ba9bcea7fc0c10072b22ea10685591f925e1d00f74a63d082f08d4d4170b121) is preserved unchanged as a failed packaging artifact. A multi-source copy reused one destination and placed the v6 RED registry log at the v7 path. Do not use that archive as proof of the registry pass. This v2 package separates the v6 RED and exact-v7 PASS logs and launch stamps.
