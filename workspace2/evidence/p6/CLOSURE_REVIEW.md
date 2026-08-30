# P6 calibrated closure review

## Task and model proof

| role | task identity | explicit model | fork mode | result |
| --- | --- | --- | --- | --- |
| initial independent reader | `/root/p6_manager/p6_cold_reader_one` | `gpt-5.6-luna` | `none` | restated that only calibration/rejection is authorized |
| initial plausible misreader | `/root/p6_manager/p6_plausible_misreader` | `gpt-5.6-luna` | `none` | found definition-as-caller attempt and card block |
| initial pre-edit reviewer | `/root/p6_manager/p6_preedit_reviewer` | `gpt-5.6-terra` | `none` | found conditional verdict and reproducibility gaps; repaired in commit `7533ca4e` |
| independent reader A | `/root/p6_manager/p6_reader_a_final` | `gpt-5.6-luna` | `none` | no authorization-changing ambiguity |
| independent reader B | `/root/p6_manager/p6_reader_b_final` | `gpt-5.6-luna` | `none` | no authorization-changing ambiguity |
| plausible misreader | `/root/p6_manager/p6_misreader_final` | `gpt-5.6-luna` | `none` | all declaration/test/facade shortcuts blocked |
| final reviewer | `/root/p6_manager/p6_reviewer_final` | `gpt-5.6-terra` | `none` | approved the rejection; no findings |

The spawn records were explicit model calls with non-inheriting forks. No builder was dispatched,
because the admission card authorizes no production implementation after the real-consumer count
failed.

## Contract matrix

| contract law | evidence | result |
| --- | --- | --- |
| two real non-test callers before macro | frozen source inventories in `REPRODUCTION.md` | fail: one foundation path; zero index/compiler paths |
| inputs materially consumed | three isolated input-removal mutants in `MUTANT_RESULTS.md` | foundation catches removal; index/compiler only fixture-sensitive |
| manual control retained | diff from `44c22154` contains only card/evidence | pass |
| no runtime reflection/dyn/allocation/panic/dependency | no production diff and candidate tripwire scan | pass for the retained control |
| compile-time uniqueness/conversions/subsets/static dispatch | no candidate admitted | not claimed |
| docs/generics/spans/expansion/compile-fail/codegen | no macro candidate admitted | not applicable; no claim |
| cross-crate release IR/assembly/text/compile time | no candidate admitted | UNVERIFIED by design; not an adoption claim |
| two clean final gates | receipt follows this review | pending receipt |

## Candidate table

| candidate | disposition | reason |
| --- | --- | --- |
| normally formatted manual conversions and match | retained | the only admissible control |
| narrow private `macro_rules!` | rejected without implementation | fewer than two qualifying consumers |
| maintained delegation/static-dispatch crate | rejected without installation | no admitted duplicated proof and no dependency/text comparison earned |
| proc macro | rejected without implementation | no arbitrary parsing or source-spanned semantic diagnostic need |
| tagless/GADT | rejected without implementation | no two real interpreters, uninhabited case proof, or erasure proof |

## Final tripwire inventory

| tripwire | count | exact locations | disposition | evidence or finding ID |
| --- | --- | --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| source-dropping conversion or map_err | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 added | no P6 production paths changed | manual foundation conversion retained | `MUTANT_RESULTS.md` |
| checked-arithmetic sentinel/saturation or operand loss | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| dyn/Box/Vec/Arc/Rc | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| public tuple fields or positional semantic tuples | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| unit/stateless namespace structs | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| public local traits or one-implementation delegation | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| one-letter generic parameters | 0 added | no P6 production paths changed | no candidate | diff from baseline |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 0 added | no P6 production paths changed | baseline not altered | diff from baseline |
| test-only Option/discarded results/success-only assertions | 0 added | no P6 tests changed | no candidate | diff from baseline |
| unsafe/SIMD/allocator/dependency additions | 0 added | no manifest or production paths changed | no candidate | dependency trees |
| public item without current consumer and falsifier | 0 added | no P6 production paths changed | no candidate | source inventory |

## Churn and future boundary

The first card checkpoint `96da7a27` was rejected for a conditional final decision, imprecise
search scope, and insufficiently recorded reproduction material. Its replacement checkpoint is
`7533ca4e`; production churn is zero lines and evidence churn is 94 added/5 deleted lines.

Future integration is intentionally minimal: after an approved typed identity grammar exists, land two
independently shipped, non-test consumers that visibly forward runtime input through closed
protocol-code conversion or one static dispatch boundary. Freeze their call graph and run
input-removal mutants before opening a fresh card. This prototype does not merge into
`orchestra-shared` and does not authorize shared adoption.
