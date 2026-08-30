# Rejected C1-FORMAT card v1

The first frozen card digest `0d0400eab935b881d1d9adbfb191a3da913c4efe9910eead3b728e5506947d00`
was read at pre-edit checkpoint `ec2f3cd8cfdfcccbc8921c7d4fafa7926848caf9`.

| Role | Task identity | Explicit model | Result |
| --- | --- | --- | --- |
| Cold reader | `/root/p5_c1_fragment_manager/c1_format_cold_reader_one` | `gpt-5.6-luna`, non-inheriting | Found missing exact signatures, variants, golden/mutation enumeration, LOC method, and a baseline/checkpoint ambiguity. |
| Reviewer calibration | `/root/p5_c1_fragment_manager/c1_format_terra_calibration` | `gpt-5.6-terra`, non-inheriting | BLOCKED for forgeable-view proof, conversion negative space, cursor work/cardinality falsifier, and no source-bearing skeleton/tripwire inventory. |

The card is rejected, not amended by interpretation. Its successor must state the immutable source
baseline and evidence checkpoint separately; correlate private view state through one input borrow and
proved ranges; use actual-rlib downstream forgery/conversion fixtures; freeze complete skeletons;
name exact errors/goldens/mutants; and prove exact-size fused cursors without test-only counters.
