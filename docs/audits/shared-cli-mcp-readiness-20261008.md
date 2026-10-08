# Shared CLI and MCP outcomes

Base: `352217d351d82634519f265b718ed19f0117e87f`, tree
`9df138c76c8757e62facb6bd9029e6d3ea8c0dee`, protocol 24. GitHub canonical was
fetched before creating the isolated source worktree. Canonical
`6c3eea8fb6ae7c4307fb941bc125b956f99a81c6` and foreign dirty configuration
were not edited. No native target or graph was copied or used.

## Baseline and actual evidence scope

Report 11's actual observations belong to `0e1f9bc91`, not this candidate.
At that source, `dependencies pkg:npm/react@19.1.0` printed a refusal but
exited 0; empty MCP packages suggested `backend.index` with `{}` despite its
required path; JSON ticket strings were rejected by MCP.

Source inspection of 352 confirms the mechanisms remain: CLI `main_entry`
emits every successfully projected Answer with exit 0, both MCP presentation
paths hard-code `isError: false`, Markdown shelf emits `{}`, and MCP ticket
parsing independently calls `from_value` while CLI calls `from_str`.
These are source-backed baseline explanations, not new live application runs.

## Behavior

Admitted dependency failures now retain the exact requested package, original
reason and an inspect-package action. Their cause is missing captured evidence,
not an invented SDK configuration failure. The shared Answer exposes its
existing typed fault; CLI keeps the full rendered answer and exits 2, and MCP
sets isError while preserving the full structured answer.

Refused, Failed and PartiallyPublished index terminal receipts attach the shared
Fault without dropping the exact ticket or native compiler/partial publication
payload. Human and Markdown rendering retain those receipts after the fault.
Published, Unknown lookup observations, and successful cancellation remain
distinct. Accepted starts, Pending progress and Requested cancellation derive
one typed PollIndex action containing the issued ticket and exact next sequence.
NoIndex directs callers to inspect the shelf; it does not promise an active job.
Only actual typed accepted work gets the in-flight polling guidance.

CLI and MCP use one closed ticket decoder for an object or its copied JSON
string. Strings are bounded before decoding; malformed epochs, zero counters,
unknown fields and scalar tickets remain refused before owner access. The MCP
schema advertises both accepted spellings and preserves the closed object
schema. Missing index path produces a structured usage fault with explicit
required-path guidance. Empty shelf guidance names the required absolute path
without fabricating a project path.

Existing bounded native PackageCompilerFailure values provide exact source,
recipe, phase and closed cause in structured output. Terminal projection now
preserves that same fault. This atom does not create compiler stderr, expose
local-only diagnostics, infer a hidden checker cause from text, or change the
existing private bounded owner-startup failure receipt.

## Validation

New shared controls exercise strict ticket object/string parsing, malformed and
oversized inputs, executable exact pending-job actions, requested package
binding and absent-versus-incomplete readiness. CLI checks full JSON evidence
with nonzero status; MCP handler controls check copied ticket spelling, missing
path before owner work, dependency failure status and unchanged ticket payload.
Existing compiler/partial controls now also assert retained FaultDto provenance.
Existing progress assertions check the exact next sequence in the executable
tool-call JSON; terminal refusal checks isError while retaining all receipt
identity assertions.

Source validation: Rust syntax parsing of every edited module, formatting the
new test module, and git diff --check. All Cargo/native controls are unrun here.
The MCP acceptance worker reported fresh capacity admission refusal; no native
result is inferred from parsing or prior source epochs. Root reviews and
composes this atom before any explicitly admitted warm remote gate.
