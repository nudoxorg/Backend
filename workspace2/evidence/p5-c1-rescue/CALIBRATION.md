# Fresh C1 rescue calibration

The card was frozen at `3d0c4dbc2f65793611ff3da259b771b481ef34ef` after the passing detached
control `f40e269e196a8a19b59683888b7ebe9ba8d4fe79`. The prior `workspace2/evidence/p5-c1/format`
v1–v8 material remains rejected history; it was not patched or relabeled.

## Explicit non-inheriting role proof

| role | task identity | model | result |
|---|---|---|---|
| Sol/root | `/root` | inherited root | supplied isolated P5 C1 rescue authority |
| Terra manager | `/root/p5_c1_format_rescue_manager` | Terra | created and replayed detached control; froze card |
| Luna mechanical calibration | `/root/p5_c1_format_rescue_manager/c1_luna_calibration` | `gpt-5.6-luna`, `fork_turns=none` | control passes; reported missing rescue-only exact/fused/allocator tests and fixture Clippy risk |
| Terra hostile reviewer | `/root/p5_c1_format_rescue_manager/c1_terra_reviewer` | `gpt-5.6-terra`, `fork_turns=none` | clear; two non-blocking implementation tripwires |

The Luna report prevents inheriting the smaller control as the production suite: production must add
`ExactSizeIterator`/`FusedIterator`, all-golden prefixes, allocation counting, and avoid its complex
fixture tuple. The Terra review replayed the detached control with formatting, locked tests, and
warnings-denied Clippy; it confirmed exact-one actual rlib selection and seven separated fixtures.

No calibration finding changes the observable capability or requires a card restart. The two reviewer
tripwires are bound into the builder order: the code must count coded/uncoded primary diagnostics rather
than compare prose, and the exact-one rlib resolver must remain adjacent to `current_exe` rather than
claim timestamp provenance.
