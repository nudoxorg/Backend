# Frozen DTO20 functional follow-up

These are actual ordinary CLI/MCP observations on `ilo` (`95.217.56.147`, Linux x86_64), using source `039c360d286962a6cf19488fb86caabd1d8c93de`, tree `a3911d62ef9c4ef1f8206c4fc05b8a8c6fde5156`, and matched runtime manifest SHA256 `a693977a9bcbf2e25c7b7ebb3c820d8a971ac5a190cf25708d608ee69c4c07f8`. Each owner had private source, HOME, state, endpoint and cwd. The acquired official sources and earlier receipts remain unchanged. The compiler environment is explicitly configured from the retained closed 805 host snapshot; this is not installer or stock-setup acceptance.

Whole-package passes: **0**. Source bodies, native publication, history, references, paging and cold lifecycle are separate obligations.

`inventory.json` binds 404 copied raw files / 3,404,062 bytes. Its SHA256 is `810eb7e662d1a8e47345c65232e2f3e91a253a95dfc80a540fa84363da23c82a`. Transport archive SHA256 is `a67214dc877f9b0e7d20244dc8b7a548a73c76c5b79e290104708ee1747abe84`. Every member was independently checked after copying to this worktree. This README is explanatory and outside the frozen inventory. Full owner stores, authority secrets, dependency caches and the later 13:08 runtime cohort are excluded.

| Observation | Actual outcome and limit |
| --- | --- |
| tslib 2.8.1 | Native compilation refused `tslib.es6.js` at authority/type_check, with a retained truncated 256-byte diagnostic. Source facts and ordinary reads survived the cold boundary; no native generation was published. |
| Immer 11.1.18 | Selected Complete generation reports 16 artifacts. `WritableDraft` cannot resolve or search. Both membership pages expose `src/types/types-external.ts` as the only all-zero content/null identity row among 17 selected files. The public DTO does not disclose the internal unavailable reason. |
| Idna 3.20 | Both membership pages contain the requested `idna/core.py` with a nonzero identity/version; the missing requested source body is not explained by absent file capture. The selected 22-image history published after 20.101 seconds and passes the same frozen shared history gate. Requested source fidelity remains unfulfilled. |
| Certifi 2026.7.22 | All six selected source members, including `certifi/core.py`, have nonzero identity/version. The requested `contents` body was not obtained; successful command/parity results cannot grant its source fidelity. |
| Itsdangerous 2.2.0 | Selected 15-image history published after 8.045 seconds. The frozen shared gate validates exact package identity, sequential image lineage and current source frontier. Honest `input_replay_status=unproven` does not claim source recompilation. |
| Packaging 26.3 | Selected history refused at VerifyPayloadClosure/ResourceLimit after 16.146 seconds: one canonical row needs 4,314 bytes against a 4,096-byte segment ceiling. Exact read-only structural forensics follows below. |

The v3 membership observer captured only the first page because it read the public envelope rather than `package_source_membership_page`. The preserved v4 observer walks both actual pages with their unchanged source roots/version and opaque cursor. Both receipts are retained; the partial v3 capture is an observer issue, not a product refusal.

## Packaging row accounting

`observations/packaging-oversized-core-row-forensics-v1.json` (SHA256 `caff8dba0d50c3714b4fd962b213b374d52989e62b50d03f50b107bdb72e787c`) examines 118 retained NXFI frames without starting an owner or modifying selected HEAD. It reconstructs Core row payloads using the frozen full-wire and canonical-row grammar. This is structural forensic evidence; it does not claim outer store cryptographic re-admission.

The 4,314-byte match is `SemanticPlaneKind::Ir(Core)`, tag 1, `tests/test_version.py::test_normalized_versions`, canonical entity 215, key `d96e123028d2925bfce925f213a5e6d008bdf37222ab43cb56a170dad618379a`. It has **zero members** and **one 4,117-byte decorator attribute**. That attribute exactly equals the official `pytest.mark.parametrize(...)` AST source slice, SHA256 `14f8c36599adaa6a4b444ca56da74abb57ff7e5ce9aefb6b561a970a360fcb79`; the containing unchanged source file SHA256 is `41231199da5559098698684691f75f3ed0269e45f79ee0e7a9e5313fa85ca9d5`.

| Field | Encoded bytes |
| --- | ---: |
| Declaration identity | 32 |
| Name length and bytes | 28 |
| Kind / visibility | 2 / 1 |
| Parent / parentage | 33 / 33 |
| Eight authority flags | 8 |
| Member count / identities | 4 / 0 |
| Attribute count | 4 |
| Attribute length and bytes | 4,121 |
| Payload total | 4,266 |
| Segment header / row header | 11 / 37 |
| Total observed | **4,314** |

Two retained CAS object versions contain the same NXFI payload (SHA256 `a9287cb8867ee00129c6b47caf572502f4d45b9aa53b9424e4238c32fbf950a6`). Their envelopes and the matching public source files are included under `packaging-forensics/`. The same retained images contain five other unique oversized decorator Core rows in `tests/test_specifiers.py` with observed lengths 4,956, 6,964, 7,299, 8,703 and 9,141. A member-list-only repair or a cap adjusted just to 4,314 would miss those real attributes. The receipt retains every exact row key, field length, payload digest and source match.
