# Final variance-calibration deck

Frozen custody: card commit `2e317c6329d9b73c576a8f9f2107cf79ff541e35`; card SHA-256
`17143118e732379f7afff33569de5fe261accca0845b6d6046b7fcca397fd215`; skeleton SHA-256
`e214f0c821d2e209ae775cd153fa30422170b6654ede13b92958f68c6df04a0c`; 93 LOC.

| role | task identity | model | result |
| --- | --- | --- | --- |
| cold reader one | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_cold_reader_one` | `gpt-5.6-luna` | pass |
| cold reader two | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_cold_reader_two` | `gpt-5.6-luna` | pass |
| plausible misreader | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_plausible_misreader` | `gpt-5.6-luna` | pass |
| reviewer calibration | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_reviewer_calibration` | `gpt-5.6-terra` | pass |

All roles were read-only and explicit `fork_turns: none`. Luna received card and skills only; Terra
also received the frozen skeleton. The files retain their raw calibration findings.
