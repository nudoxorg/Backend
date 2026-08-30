# Final bound-target calibration deck

Frozen card custody: commit `8d990dfe1d8c74ba468d2be70e5b9cf656af63ff`; SHA-256
`ffce71ee4fd8d00f1ff5d1bd4d12397bc8938136a48c1ab0e6434bc48f3c86e8`. Frozen skeleton SHA-256:
`e1138c221def6262d64f8ce168562394202ac9043bf70227743361b97f9d7b7d`; 94 formatted lines.

All roles were explicit, read-only, non-inheriting (`fork_turns: none`). Both cold readers and the
plausible-misreader received only the card and governing skills. The Terra reviewer calibration also
received only the frozen skeleton. The returns below are raw calibration evidence, not edit authority.

| role | task identity | model | calibration result |
| --- | --- | --- | --- |
| cold reader one | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_cold_reader_one` | `gpt-5.6-luna` | pass |
| cold reader two | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_cold_reader_two` | `gpt-5.6-luna` | pass with runtime evidence unverified |
| plausible misreader | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_plausible_misreader` | `gpt-5.6-luna` | pass; shortcut rejected |
| reviewer calibration | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_reviewer_calibration` | `gpt-5.6-terra` | pass; source/runtime unverified |
