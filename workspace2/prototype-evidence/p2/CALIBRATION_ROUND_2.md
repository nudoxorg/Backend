# P2 control card: calibration round two raw record

## Specimen and fresh task/model proof

The round-two specimen was `CONTROL_CARD.md` at manager commit `44cd7e4bddfd4373215911a3e60982ea3eea0cf2`, SHA-256 `0b3668768ebf4d2e98d0b58ba593eefc647ca6bf9d77852bf92c40ceda3d49e3`.

| Role | Task ID | Explicit model | Fork | Result |
|---|---|---|---|---|
| fresh independent reader | `/root/p2_manager/p2_control_cold_reader_v2` | `gpt-5.6-luna` | `none` | restated every main law; found evidence-directory wildcard scope gap |
| fresh blind reviewer | `/root/p2_manager/p2_control_reviewer_v2` | `gpt-5.6-terra` | `none` | rejected calibration with three blockers and three majors |

## Reader result

The Luna reader restated the five-event terminal, 32/92 grammar, sequence/end arithmetic, exact public surface, all ten evidence rows, 380/337/43 production and 320/282/38 test caps, normal dependency bounds, and all negative space. Its sole ambiguity was that `append-only raw evidence under workspace2/prototype-evidence/p2/` was directory-wide permission rather than an exact path list. This could broaden writable scope and hide an unplanned artifact.

## Reviewer result

The independent reviewer found the following proof gaps:

1. **BLOCKER:** all advertised adapter tests enabled `fault-injection`, so normal `std::fs::File` code was not independently compiled/tested/released. Correction: add feature-off test, Clippy, and release gates with the same terminal/reopen law; state the seam does not change frame/state transitions.
2. **BLOCKER:** recovery was compared but never resumed by append after ordinary reopen or tail repair. Correction: require normal/repaired reopen to append exactly at prior durable end with next sequence and preserved prefix.
3. **BLOCKER:** physical sequence acceptance predicate was missing. Correction: require first zero, every next exact sequence, `FrameSequence { expected, observed }`, no repair, and bytes unchanged.
4. **MAJOR:** write fault schedule omitted the complete-write-before-error `92` case. Correction: test `0..=92`; the 92-byte case must recover the new frame while the original caller still receives no receipt/effect.
5. **MAJOR:** file path policy was not literal. Correction: reject symlink/directory/nonregular before header I/O with exact `OpenError::Path`.
6. **MAJOR:** receipt compile-fail tests did not defend against all added construction/bridge routes and nested workspace did not inherit unsafe denial. Correction: exact post-build public surface inventory, prohibition of all extra routes, `#![forbid(unsafe_code)]`, and a source tripwire.

The reviewer’s strongest counterexample was a feature-gated alternate append path: all prior feature-on gates pass while feature-off normal code is divergent or uncompiled. It also identified likely resume corruption by returning a correct recovery then writing the next event at offset zero. Its full pre-edit tripwire found no candidate source because all allowed adapter paths were absent; this is evidence of a contract gap, not evidence that future source is clean.

## Calibration result and repair

Round two failed. The manager replaced the active card atomically, narrowed the evidence path list, and added all six reviewer falsifiers. No source edit authority has yet been issued. The next trial is the final permitted calibration retry for this card family; a further semantic-card failure is an unresolved manager/parent decision rather than another prompt loop.
