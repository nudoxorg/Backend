# Coupling skeleton

| Module/control | Invariant owner | Public terminal/control boundary | Direct dependencies | State/resource boundary |
| --- | --- | --- | --- | --- |
| `nudox-root` canonical root reader | Root grammar and exact structural errors | `ValidatedRoot::try_from(&[u8])` (new) | `nudox-id`, `nudox-object`, `nudox-schema` | one borrow of caller bytes; validated rows point into it |
| `nudox-root` borrowed generation view | Root/locality identity pairing and row selection | `BorrowedGenerationView::new` (new) | borrowed root + `ValidatedLocality` | complete/partial locality stays explicit; no native row owner |
| `nudox-hydration` borrowed need/plan | Need-to-view bind and closure evidence | `Need::bind_borrowed`, `plan_borrowed` (new) | borrowed generation view, plan/closure scratch | complete authority required; no reparse after bind |
| `nudox-object-pack` prerequisite | Descriptor/body pairing and content digest | existing `lookup`, `SelectedObject::verify` | pack backing | selected bytes borrowed from pack backing |
| `nudox-store-memory` prerequisite | bounded first-write immutable ownership transfer | existing `insert_owned`, `get` | selected verified body | no copy/per-object allocation; rejected owner returned |
| `nudox-hydration` publication | full required-store presence fact | existing `StagedGeneration::verify` | required descriptors and store predicate | only complete projection can mint sealed witness |
| `nudox-operation` bind/run | one-time verified root/object binding | `LocalObjectProvider::bind_verified` (new) | borrowed generation view + sealed witness | stale root rejected here; later run has no equality recheck |
| root/hydration/operation unit tests | exact edge errors, pointer/work/layout facts | owning crate tests | public values only | malformed input never admits; warmed paths allocation-free where claimed |
| chief integration | complete cross-crate travel | chief red journey | all above | ordinary top-level test; no test-only graph |

## Resource controls

| Resource row | Baseline/control | Required bound | Measurement | Rollback trigger |
| --- | --- | --- | --- | --- |
| Root borrowed validation | current owning `GenerationRoot` | returned byte and entry-derived pointers are inside caller root buffer; no retained root-row arena in terminal | pointer ranges and allocator counter around warmed validation | any copy, `Box`, or row backing retained by witness |
| Closure/plan | existing owned-root planner | no post-validation root/locality reparse; bounded scratch explicit | plan work/allocator counters and constant-body mutant | a scan/conversion repeats trusted validation |
| Pack/store transfer | existing pack + memory controls | selected stored pointer equals pack body pointer; no per-body copy/allocation | pointer equality, allocation counter, store accounting on success/reject | copied body or admission after failed verify |
| Root representation | 1 and 100,000 rows | owner+backing, peak live bytes, construction scratch, lookup/scan work recorded | isolated process allocator measurement and work counters | borrowed form regresses evidence without compensating law |
| Public operation | current `from_view` path | one binding comparison boundary; no later generation/object recheck | bind fault counter/constant-body and stale-root mutations | duplicate check remains necessary to reject a case binding should reject |
