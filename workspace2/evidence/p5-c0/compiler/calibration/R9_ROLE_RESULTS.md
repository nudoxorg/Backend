# R9 calibration custody

Card SHA-256 `c4fa4dd7df6644eff74e44821f2fbcd65dcc05a8f8539977d0b520396a86b74d`.

| role | task | model | verdict |
| --- | --- | --- | --- |
| cold one | `c0_compiler_r9_cold_one` | `gpt-5.6-luna` | CLEAR |
| cold two | `c0_compiler_r9_cold_two` | `gpt-5.6-luna` | CLEAR |
| misreader | `c0_compiler_r9_misreader` | `gpt-5.6-luna` | CLEAR |
| reviewer | `c0_compiler_r9_terra_calibration` | `gpt-5.6-terra` | CLEAR |

All used `fork_turns="none"`. The Terra reviewer freshly replayed paired consumer and registry owner-body
artifacts, found equal realized dispatch branch/call behavior and inlined-drive absence honestly, replayed
the four red seeds, and passed frozen fmt/tests/Clippy warnings-denied. No production edits occurred.
