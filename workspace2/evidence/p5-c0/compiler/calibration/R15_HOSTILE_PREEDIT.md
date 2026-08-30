# R15 hostile pre-edit Terra review

| task | model | object | verdict |
| --- | --- | --- | --- |
| `c0_compiler_r15_hostile_preedit` | `gpt-5.6-terra` | frozen R15 skeleton | CLEAR |

No repair authority was issued. The intentional pre-builder destination/skeleton mismatch was treated as
the lifecycle requires. R9 artifact material was superseded history, not current authority.

| tripwire | count | exact locations | disposition | evidence or finding ID |
| --- | ---: | --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 | complete frozen set | CLEAR | R15-HOSTILE-READ |
| source-dropping conversion or map_err | 0 | complete frozen set | CLEAR | R15-HOSTILE-READ |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 | complete frozen set | CLEAR | R15-HOSTILE-READ |
| checked-arithmetic sentinel/saturation or operand loss | 0 | complete frozen set | CLEAR | R15-HOSTILE-READ |
| dyn/Box/Vec/Arc/Rc | 0 | complete frozen set | CLEAR | R15-HOSTILE-READ |
| public tuple fields or positional semantic tuples | 0 | complete frozen set | CLEAR | R15-HOSTILE-READ |
| unit/stateless namespace structs | 2 | `skeleton/lib.rs:58,74` | allowed consumed public capability values | R15-HOSTILE-CAP |
| public local traits or one-implementation delegation | 0 | `skeleton/lib.rs:10-18,20-48` | private traits have two concrete implementations | R15-HOSTILE-CAP |
| one-letter generic parameters | 0 | complete frozen set | sole generic is `ConcreteFrontend` | R15-HOSTILE-READ |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 8 | `subset.rs:10,11,56,83,85`; `dispatch.rs:29,38`; `release_consumer.rs:64` | fixtures, edition, exact diagnostic contract only | R15-HOSTILE-E0599 |
| test-only Option/discarded results/success-only assertions | 2 | `subset.rs:10,11` | paired forbidden/legal structural probes | R15-HOSTILE-E0599 |
| unsafe/SIMD/allocator/dependency additions | 0 | complete frozen set/manifests | CLEAR | R15-HOSTILE-READ |
| public item without current consumer and falsifier | 0 | `skeleton/lib.rs:58-79` | dispatch tests, consumer, external subset probes | R15-HOSTILE-CAP |
| stdout containment and exact diagnostic parser | 0 | `skeleton/subset.rs:55-92` | null child stdout, piped stderr, exact header/trailer/name check | R15-HOSTILE-E0599 |
| actual-rlib absence proof / stale-artifact resistance | 0 | `skeleton/subset.rs:13-43,45-73,94-110` | exactly one discovered rlib and explicit extern | R15-HOSTILE-RLIB |
| closed typed rows and sole decisions | 0 | `skeleton/lib.rs:10-18,20-55,58-79`; `dispatch.rs:10-47` | two private rows; one stage and one language match | R15-HOSTILE-CAP |
| path/cap restrictions and frozen-object custody | 0 | card lines 33-43,189-192,228-248; frozen skeleton | card and skeleton custody match; R9 superseded | R15-HOSTILE-CUSTODY |
