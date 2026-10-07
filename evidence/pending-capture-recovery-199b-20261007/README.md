# Interrupted Pending capture recovery — native gate

Frozen compiled source: `199bef18b011e41493a3a973f8765015b29ab412`, tree `396b0720dff5f3a74577115670366c89a2743dd9`, Cargo.lock SHA-256 `b508bc8978b37e09e54887a76cdb7055a4188f1243770057784a08ad61560268`. Base is reviewed `96b04b141affd653e3c6932dc8c325f351775f8f`. The evidence commit does not re-label that source or its immutable debug images.

The current exclusive workspace owner recovers a selected Pending capture only after checking its epoch/fence, the exact retained source marker, no active local index job, and no protecting remote reservation, assignment, guarded result or unresolved stored admission. A keyed capture additionally needs its exact Accepted operation, source base and authenticated structural receipt. It uses the existing authenticated Pending-to-terminal transition, retains capture identity and predecessor history, and publishes the resulting view. Explicit retry/removal triggers recovery; no workspace state is deleted or reset. Real live work and cancellation remain protected. Prepared, Failed, missing or conflicting keyed ownership remains a conservative refusal; this candidate does not guess those states away.

Terminal native gates are preserved separately, with complete source and launch identities:

- `capture_recovery_`: 3 passed, 0 failed. Tests cover live-work protection, exact same-state interrupted capture/retry, genuine unavailable compiler refusal, removal/two cold reopens, guarded/offered/unknown remote work.
- Full adapter tests: 21 passed, 0 failed, including cancellation, queued removal, reply abandonment preserving accepted work, and slow metadata responsiveness. The two capture adapter tests overlap the targeted gate; these counts are not a claim of 24 unique tests.
- Matched debug CLI/MCP/locald build: exit 0. Image hashes and immutable paths are in `pending-capture-199b-debug-runtime-build-manifest.json`.

The earlier failures are retained:

- Initial ceefaed test compilation failed with missing test imports; no tests ran. Its 31.577804-second sample age violated the later clarified 30-second admission criterion. This is recorded as an admission deviation, not a compliant launch; no post-hoc sample is substituted.
- The next gate passed legacy recovery but failed a test assertion expecting an unchanged source observation to increment. The corrected test uses a new operation key and requires the exact unchanged source/input/count/observation tuple to remain identical.
- All successful 199b native launches were admitted with complete successful samples aged 4.948427–5.222304 seconds, managed lifetime lock, explicit `-j4`, and host memory/disk floors. Complete fleet samples and raw command/test outputs remain retained.

Each gzip preserves the original raw stdout/stderr bytes; `artifacts.json` records both raw and retained SHA-256. `source-identity.json` binds changed source blobs. No application workspace, home, database, credentials or executable images are included. Authentic public application results are separate follow-up evidence; native fixtures are not whole-app acceptance proof.
