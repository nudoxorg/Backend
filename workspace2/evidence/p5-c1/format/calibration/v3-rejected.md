# Rejected C1-FORMAT card v3

Card digest `2656c43d2b40b2064c5c6a1ea6caa146c2d3955d373e9cf69e85012969b978e6` at
`b077da21854d4da2dde7b8cc87cdbafc674eb695` is rejected calibration history only.

| Role | Task identity | Explicit model | Result |
| --- | --- | --- | --- |
| Reader A | `/root/p5_c1_fragment_manager/c1_format_v3_cold_reader_one` | `gpt-5.6-luna`, non-inheriting | Clear. |
| Reader B | `/root/p5_c1_fragment_manager/c1_format_v3_cold_reader_two` | `gpt-5.6-luna`, non-inheriting | Clear. |
| Plausible misreader | `/root/p5_c1_fragment_manager/c1_format_v3_misreader` | `gpt-5.6-luna`, non-inheriting | Clear. |
| Reviewer calibration | `/root/p5_c1_fragment_manager/c1_format_v3_terra_calibration` | `gpt-5.6-terra`, non-inheriting | Rejected: mutation priority contradicted validation order; test imported unexported IDs; unnamed fixed-width type literal violated the tripwire. |

The entity-count-three counterexample proves why a mutation table cannot hand-assign first errors. A
fresh root-authorized post-ceiling card replaces this specimen with one ordered decision table from
which every corpus expectation is derived. No production source was edited.
