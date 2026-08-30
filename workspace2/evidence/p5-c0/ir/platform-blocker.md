# P5 C0-IR calibration platform blocker

## Frozen inputs

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Clean starting HEAD: `694f4f6cde5d14e59850f47c21b3a751b1c8f88d`
- Source candidate: `44c22154fd5238e4769562590420371979306050`
- Active card SHA-256: `916dd59a57da11f7264efba9d45ace85453e3bcfc6082082daee0a5061e7db66`
- Compiler-path check: `git diff --quiet 44c22154fd5238e4769562590420371979306050 -- workspace2/planes/compiler` exited zero.

## Required first calibration dispatch

The fresh, source-blind first reader was requested as:

```text
task_name: c0_ir_reader_one_fresh
model: gpt-5.6-luna
fork_turns: none
role: read-only independent cold reader
inputs: only the C0-IR card and governing Rust skills
```

The first dispatch returned:

```text
collab spawn failed: agent thread limit reached
```

Root interrupted the two stale completed tasks that still consumed the runtime task limit and
instructed exactly one retry. The retry used the identical explicit model and non-inheriting fork
parameters and returned the same transport failure:

```text
collab spawn failed: agent thread limit reached
```

## Consequence

No fresh Luna reader exists. Therefore the independently required second Luna reader, Luna plausible
misreader, and blind Terra reviewer cannot begin, and the complete calibration deck is invalid. No
builder or repair authority exists. No production or test path was edited.

## Exact next decision

Restore enough runtime child-task capacity to create four fresh non-inheriting calibration roles with
the card-required models, then restart the complete deck at this exact card digest. Until then, retain
the baseline and leave C0-COMPILER and C1 unopened.
