# Card C1-R — producer v3 repair (registered role: nudox_luna_implementer)

## Context
Checkpoint 3babc5a35 built the v3 frame (header, directory, digest,
sections, closed vocabularies) and it validates through the reader. The
hostile diff review rejected several facts and the checkpoint is incomplete
against the card's own must-prove list. You own the same paths as card C1
(helper/**, tests/producer_parity.rs, tests/fixtures/producer/**) and
nothing else.

## Findings to repair (each is a red row; fix ALL)
F1 Doc pointer always ABSENT: `DocRow() => Absent` starves the consumer's
   extension plane — `compiler/driver/lower/csharp.rs` `csharp_facts()`
   reads the declaration's doc coordinate to build `CSharpFacts.xml_provenance`.
   Emit the real doc row coordinate in the declaration row (byte 44).
F2 Doc span semantics: docs rows (20B) carry the COMMENT's source-byte span
   (`DocumentationCommentTrivia` full span), not the declaration's span.
   The reader doc says: inclusive start / exclusive end of the comment.
F3 Interface-implementation reference rows use start=end=0 — the consumer's
   owner-relative law (reference.start >= owner.declStart) faults every such
   row. Use the implementing member's real source span for the reference
   (owner.start..owner.nameEnd of the implementing member's span is
   acceptable; keep start<=end and start>=owner declStart).
F4 Partial roles are computed against merged Roslyn symbols and mis-assign
   roles. Contract: role 1 = the defining/bodyless part; role 2 = the
   body-bearing part. Decide by syntax-site identity:
   a part IS the definition iff the symbol's PartialDefinitionPart is null
   or equals this part's syntax reference; the part carrying the body is
   role 2. Partial types: every part is role 1. A partial method with a
   single declaration bearing a body is role 2 (Roslyn's implementing
   declaration). Key rows by (symbol, syntax node) so both parts emit.
F5 Offsets(): your per-char UTF-8 byte loop mishandles surrogate pairs
   (counts 6 bytes for one 4-byte codepoint and splits the offset). The
   tested machinery lives in Extractor.BuildByteOffsets — reuse exactly
   that implementation (extract one shared internal helper; do not fork a
   second offset table). Verified by the BOM/non-ASCII fixture law below.
F6 Delegate rows lose their parameters: GetDeclaredSymbol on a
   DelegateDeclarationSyntax yields the delegate TYPE symbol, so the
   `symbol is IMethodSymbol` parameter path never fires. Write the
   delegate's invocation parameters (DelegateInvokeMethod.Parameters) into
   the Parameters section and the row's param start/count.
F7 Name atoms must equal the exact source bytes at [nameStart,nameEnd):
   the consumer's checked_name slices the source and compares. Today
   operators emit "op_Addition" over token "+" and constructors ".ctor"
   over "Widget" — collect fails. Rule: name atom = UTF-8 bytes of the
   source span you wrote into nameStart/nameEnd, always. Keep kind cells
   carrying the semantics.
F8 Record positional members: the JSON producer emits the record's
   positional properties; the syntax walk never sees them. For record
   class/struct declarations, also emit one Property row per positional
   parameter (symbol-level, owned by the record row, declared type =
   parameter type, span = the record's parameter list slice; name span =
   the parameter identifier). This is the parity law, not a nicety.
F9 Explicit-interface flag (0x10) must also fire for explicitly implemented
   properties/indexers/events (IPropertySymbol/IEventSymbol
   ExplicitInterfaceImplementations), not only methods.
F10 Atom plane: Atom() must reject empty byte arrays with OracleFailure (the
   reader rejects length==0 atoms). Today `node.Name?.ToString() ?? ""` can
   manufacture an empty atom.
F11 Owner law: references' owner = nearest ancestor declared symbol of ANY
   kind (field initializers and property initializers currently drop their
   invocations because only IMethodSymbol owners are accepted); keep the
   file-level using omission documented. owner-relative spans must hold.

## Must-prove (unchanged from card C1, now enforced)
- The fixture zoo MUST cover every fact class from card C1's list: partial
  type (two parts), partial method (both halves), generics out+in with
  class/struct/notnull/unmanaged/new()/allows-ref-struct constraints,
  parameters value/in/ref/out/ref readonly/params/defaults, nullable T?,
  labelled tuple, int[]/int[][]/int[,]  jagged+multidim, pointer,
  managed + unmanaged[Cdecl] function pointers, dynamic, unresolved error
  type, BOM + non-ASCII identifiers in a SECOND fixture, nested types,
  event field, record positional members, explicit interface
  implementations of method AND property, doc comments with see/paramref/
  exception, attributes with named arguments, usings, invocations, object
  creations, member accesses, delegates with parameters.
- producer_parity.rs: deep per-fact assertions (closed cells, not just
  kinds): nullability cells, ref kinds, variance cells, constraint flags,
  tuple labels, function-pointer has_return, partial roles per part,
  params/has-default bits, default spelling, doc provenance span, record
  positional properties, name-atoms == source slices for operator and
  constructor rows.
- Dotnet regeneration: when COMPILER_CSHARP_COMPILER or PATH dotnet
  exists, rebuild the oracle and byte-compare every committed fixture
  image; assert byte-identical double-run determinism. No #[ignore], no
  skip. When dotnet is absent the committed-fixture assertions still run.
- Mutation falsifiers IN producer_parity.rs (do not lean on tests/image.rs):
  mutate each structural class (declaration kind, flags, partial, refKind,
  variance, type kind, nullability, reserved non-zero, atom out of range,
  inverted span, coordinate out of section, doc inverted span), recompute
  the domain digest, and require the exact ImageError variant.
- Test style: the crate's tests return Result and do not unwrap/expect on
  decoding paths — carry typed test errors instead (workspace lints deny
  clippy::unwrap_used/expect_used in shipping crates; keep that bar).
- NEW: run the consumer's `collect` against every committed fixture image
  is card C2's conformance section; your job ends at reader-level truth
  plus byte-exact fixtures.

## Environment receipts (unchanged from card C1)
dotnet: /var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk
(PATH + DOTNET_ROOT). `dotnet build -c Release` and `dotnet restore
--locked-mode` inside helper/ must pass with zero warnings
(TreatWarningsAsErrors). Rust gate:
CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
cargo test -p compiler-languages-csharp --offline — all green, including
tests/image.rs (5), tests/protocol.rs (8), tests/producer_parity.rs.

## Bounds & forbidden (unchanged)
No new dependencies, no unsafe, no driver/** or reader edits, deterministic
byte-exact output, fixture images < 64 KiB each, <= 5 fixtures.

## Checkpoint & return
Commits prefixed feat(csharp-producer):. Return: commit hash(es); exact
commands + tails; the fact-class -> fixture -> assertion table; the
mutation table; the finding-by-finding repair confirmation (F1..F11);
smallest remaining red row.
