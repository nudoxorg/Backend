# MCP usability response contract repair

Source baseline: `24390d5666094dc807a7f10d12e62ee364de2ede`.
Report: GymBroApp Dashboard Redesign, 2026-10-10.

This atom repairs three response contracts shared by CLI and MCP:

- Batch `read` retains the captured source extent through the actual product
  producer, strict wire record, presentation record, JSON, terminal text, and
  Markdown. A truncated excerpt carries `extent: truncated`; a signature
  fallback or missing row has no source extent claim. The preexisting
  `signature` field still contains the selected source excerpt or signature.
- Document summary retains the same signature as standard detail and the same
  identity. It does not hydrate additional source or documentation sections.
- Shelf entries with no declaration count evidence return `declarations: null`
  and say `declaration count unavailable` in text. The owner's readiness stays
  intact. Count evidence remains present for a root carrying declaration rows;
  an explicitly supplied empty language count can represent a known zero.

`DeclarationRecord` is directly embedded in a strict live reply envelope.
The new optional extent cannot be treated as compatible with older strict
readers: live DTO version advances from 26 to 27. Historical journal envelopes
21 through 26 remain admitted under the existing checked grammar because the
change affects only live read replies. Optional extent defaults to absent for
older standalone record JSON; that absence does not claim source completeness.
The old hardcoded DTO25 partial-publication test is updated to the current
version law and continues to refuse older live peers, now including 25 and 26.

Prepared validation (all Rust tests **UNRUN**, pending fresh fleet admission):

```sh
cargo test -p backend-present --lib usability_
cargo test -p backend-library --lib usability_
cargo test -p backend-local-service --lib usability_
```

The five focused cases exercise actual product read over immutable fixture
rows, typed encoding at all detail levels, both human renderers, strict wire
roundtrip, malformed extent admission, historical26 journal snapshot decode,
live26 refusal, missing source/signature fallback, UTF-8 truncated source, and
unknown versus known count. They are regression fixtures, not native compiler
or end-to-end installed-runtime evidence. Source formatting of the added test
functions and `git diff --check` were run successfully.

Remaining report items are deliberately unclaimed:

- Resolve ranking already has source declaration versus external-target evidence
  in `query/local.rs`, but its exact-name/kind placement does not by itself prove
  the reported WeekService declaration/import/field behavior. It needs an actual
  compiler-backed TS fixture and page-one result assertion.
- The MCP cursor wraps an owner-issued token in base64 plus expiry and MAC. An
  opaque short handle would alter portability, restart, expiry, and request
  binding contracts; the reported long cursor is not fixed by this atom.
- References depend on selected semantic link evidence. An empty result cannot
  be repaired by fabricating imports from resolve display rows. Actual TS
  compiler import/use evidence and selected-artifact link coverage must be
  checked separately.
- Outline preserves unresolved identity markers and typed truncation. Omitting
  anonymous nodes would change its selection/count contract and could hide
  compiler declarations. Native source-name evidence and pagination must be
  checked before any default filtering or source/spec ordering change.
- Angular configuration cycles, search preparation, member addressing, and
  HTML/CSS coverage are outside this response projection atom.

Historical installed connector results are not proof of this source change.
No owner, index, MCP stdio, Cargo build/test, or native compiler validation was
started while preparing the atom.
