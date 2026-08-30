# Typed identity salvage ledger

| Existing mechanism or proof | Disposition | Evidence |
| --- | --- | --- |
| Raw `ContentId` and `ArtifactId` checked decodes retain width, authority, observed byte, and raw bytes | Retain and strengthen at trusted locality boundary | Must remain source-bearing in integration falsifiers |
| Typed domain/encoding marker registry | Replace duplicate enum/macro declarations with a single declaration that compiler-checks code collisions | Compile-fail E0081 fixture required |
| `ValidatedLocality` validation and cursor/view APIs | Replace repeated trusted raw reconstruction and impossible read error path with typed borrowed witnessed lanes | Projection becomes infallible after validation |
| Existing locality error tests | Replace only assertions made impossible by immutable witness ownership | New tests name exact validation ingress error rather than read error |
| Workflow record validation | Retain wire semantics; strengthen `UnexpectedOutput` with observed output bytes | Exact error equality test |
| Object-pack identity documentation | Correct false raw-key wording rather than deleting identity documentation | Public doc test/review inspection |

No substantial proof is authorized for deletion merely to reduce the diff. Each deletion must be
listed above with its replacement mechanism or invalidity reason.
