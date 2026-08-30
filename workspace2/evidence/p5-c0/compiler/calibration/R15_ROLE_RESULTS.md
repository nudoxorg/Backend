# R15 calibration custody

Card SHA-256: `933b72bcd4bce8ccc2bbfb48817816225e9fbdfecff7464efad94652cb8b6dd3`.
Every task used its explicit requested model and `fork_turns="none"`.

| role | task | model | verdict |
| --- | --- | --- | --- |
| cold reader one | `c0_compiler_r15_cold_one` | `gpt-5.6-luna` | CLEAR |
| cold reader two | `c0_compiler_r15_cold_two` | `gpt-5.6-luna` | CLEAR |
| plausible misreader | `c0_compiler_r15_misreader` | `gpt-5.6-luna` | CARD CLEAR; every attempted shortcut hits a literal hard stop |
| reviewer calibration | `c0_compiler_r15_terra_calibration` | `gpt-5.6-terra` | CLEAR |

The Terra reviewer checked the frozen skeleton only, as the card requires before builder authority.
It replayed `--nocapture` with clean text, fresh-rllib E0599 stderr with zero stdout, exact diagnostic
primary/trailer counts, legal parse, five tests, fmt, and warnings-denied Clippy. R9 control material was
treated as superseded history. Its literal required table follows.

| tripwire | count | exact locations | disposition | evidence or finding ID |
| --- | ---: | --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 | complete frozen set | clear | R15-TW |
| source-dropping conversion or map_err | 0 | complete frozen set | clear | R15-TW |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 | complete frozen set | clear | R15-TW |
| checked-arithmetic sentinel/saturation or operand loss | 0 | complete frozen set | clear | R15-TW |
| dyn/Box/Vec/Arc/Rc | 0 | complete frozen set | clear | R15-TW |
| public tuple fields or positional semantic tuples | 0 | complete frozen set | clear | R15-TW |
| unit/stateless namespace structs | 2 | `skeleton/lib.rs:58,74` | allowed consumed public capability values | R15-GATE |
| public local traits or one-implementation delegation | 0 | complete frozen set | clear; private traits have two implementations | R15-TW |
| one-letter generic parameters | 0 | complete frozen set | clear | R15-TW |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 8 | `subset.rs:10,11,56,83,85`; `dispatch.rs:29,38`; `release_consumer.rs:64` | fixtures/edition/exact diagnostic contract only | R15-E0599 |
| test-only Option/discarded results/success-only assertions | 2 | `subset.rs:10,11` | paired forbidden/legal structural compile probes | R15-E0599 |
| unsafe/SIMD/allocator/dependency additions | 0 | complete frozen set/manifests | clear | R15-TW |
| public item without current consumer and falsifier | 0 | `skeleton/lib.rs:58-79` | dispatch tests, consumer, external subset probes | R15-GATE |

No R15 role found a blocker or major. This four-role deck authorizes only a separate hostile pre-edit
Terra review of the same frozen digest; it does not itself authorize a production path edit.
