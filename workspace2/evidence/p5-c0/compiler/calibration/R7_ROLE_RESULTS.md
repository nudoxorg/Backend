# P5 C0-COMPILER R7 calibration role results

All four fresh roles were spawned with `fork_turns="none"` against card SHA-256
`d30b53c129552e37e3baec11cd4b156a3b5833f7ed4cbd787ef37dfacacaa553`.  No role received production
edit authority.

| role | canonical spawned task | explicit model | allowed inputs | result |
| --- | --- | --- | --- | --- |
| cold reader one | `/root/p5_c0_compiler_manager/c0_compiler_r7_cold_reader_one` | `gpt-5.6-luna` | card and ten governing skill instructions only | CLEAR |
| cold reader two | `/root/p5_c0_compiler_manager/c0_compiler_r7_cold_reader_two` | `gpt-5.6-luna` | card and ten governing skill instructions only | CLEAR |
| plausible misreader | `/root/p5_c0_compiler_manager/c0_compiler_r7_plausible_misreader` | `gpt-5.6-luna` | card and ten governing skill instructions only | CLEAR; intended shadow fixture rejected |
| reviewer calibration | `/root/p5_c0_compiler_manager/c0_compiler_r7_reviewer_calibration` | `gpt-5.6-terra` | card, skills, skeletons, six seeds, R7 receipts, rejected commits | CLEAR |

The cold readers independently confirmed the manual Rust Parse/LowerIr and TypeScript Parse-only
terminal, exact rejected operands, fresh actual-rlib sole-E0599 test, four writable paths, forbidden
surface, byte-identical same-source consumer mechanism, and exact cap arithmetic.  They also confirmed
the R7 Debug requirement observes rather than discards the inner typed frontend error.

The plausible-misreader proposed a test-local shadow `TypeScriptSubset` and correctly classified that
proposal itself as a blocker: it would not prove the exported registry API.  The literal card requires a
fresh actual `--extern` registry rlib and explicitly hard-stops shadow/test-only fixtures.  It found no
compliant loophole or card ambiguity.

The non-inheriting Terra reviewer verified every digest, replayed all four seed runners from
`9225fa3bd192ea1f0c6a69f1a57a8afe66cdaf36` (runners 0; targeted red tests 101; three mutant codegen
runs 0), and independently reran unmutated frozen fmt, package tests, and warnings-denied Clippy (all
0).  Its literal tripwire scan cleared all prohibited constructs, classified six unit structs as required
closed markers/consumed capabilities, confirmed ConsumerError preserves and observes the typed error, and
reconciled the 28/25/17/22 per-file reserve math.  It leaves only the explicitly UNVERIFIED
before/mutant callable-IR and zero-cost claims.
