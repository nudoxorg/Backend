# Card C1 — producer v3 parity (registered role: nudox_luna_implementer)

## Baseline & owned paths
Baseline commit: c4b6ed32e (see ../index.toml digests). You own EXACTLY:
- compiler/languages/csharp/helper/** (Program.cs, Extractor.cs, SourceLoader.cs,
  AuthorityImage.cs, TypeSigWriter.cs, DocComments.cs, oracle.csproj, packages.lock.json)
- compiler/languages/csharp/tests/producer_parity.rs (new)
- compiler/languages/csharp/tests/fixtures/producer/** (new, committed fixture images + sources)
No other path may be written. image.rs, oracle.rs, lib.rs, Cargo.toml, driver/** are forbidden.

## One public terminal
The vendored producer emits binary authority images that the version-3 reader
(compiler/languages/csharp/image.rs — your wire authority, read it completely)
accepts, on sources exercising every fact class, with every decoded cell equal
to source truth.

## Known baseline facts (verified, do not re-litigate)
- AuthorityImage.cs currently writes a v1 image (88-byte header) and does not
  even compile: it references CSharpCompilation without
  `using Microsoft.CodeAnalysis.CSharp;`.
- The reader rejects anything but version 3. The v1 code is dead weight once
  v3 lands; delete it, keep the BOM/preamble logic (it is correct and tested
  by the span law).
- packages.lock.json (untracked) pins Roslyn 5.6.0; commit it. Never commit
  helper/obj/** or bin/**.

## Wire contract (reader truth; the reader is the only authority)
Header: 256 bytes. magic "NCAI"; u16 version=3 @4; u16 header=256 @6;
u32 body bytes @8; 32-byte SHA-256 of the raw bound source file (BOM
included) @12; u16 section count=11 @44; 2 reserved zero bytes @46;
directory of 11 x 16-byte entries @48 (u16 tag, u16 row width, u32 count,
u32 absolute offset, u32 byte count); 32-byte image digest @224.
Digest = SHA-256(b"nudox.csharp.authority.image.sha256.v3\0" ++
image[0..224] ++ body). Sections appear in canonical order, contiguous:
Atoms(tag1,row 8: u32 offset, u32 len), AtomBytes(2, 1), Declarations(3, 48),
Parameters(4, 24), TypeParameters(5, 12), TypeConstraints(6, 4),
Types(7, 16), TypeChildren(8, 8), Attributes(9, 8), Docs(10, 20),
References(11, 28). ABSENT = u32::MAX. All integers little-endian.
Atoms: len>0, valid UTF-8, offsets contiguous from 0 covering the whole
atom-bytes plane exactly.
Declarations (48B): kind u8; flags u8 (0x1 extension, 0x2 async, 0x4
iterator, 0x8 const, 0x10 explicit-interface; |=0x1f max); partial u8
(0 none, 1 definition, 2 implementation); refKind u8 (0 value, 1 in, 2 ref,
3 out, 4 refReadonly); name atom u32; qualified atom u32|ABSENT; owner row
u32|ABSENT; declared type row u32|ABSENT; declStart u32; nameStart u32;
nameEnd u32 (law: declStart<=nameStart<=nameEnd); paramStart u32 +
paramCount u16; genericStart u32 + genericCount u16; doc row u32|ABSENT.
Closed kind vocabulary 1..17: 1 class, 2 struct, 3 interface, 4 enum,
5 delegate, 6 record class, 7 record struct, 8 namespace, 9 field,
10 enum member, 11 property, 12 indexer, 13 event, 14 constructor,
15 method, 16 operator, 17 conversion.
Parameters (24B): type row u32; name atom u32; refKind u8; flags u8 (0x1
params, 0x2 has-default); reserved u16 = 0; default spelling atom u32|ABSENT
(non-empty when present); nameStart u32; nameEnd u32 (start<=end).
TypeParameters (12B): name atom u32; constraintStart u32; constraintCount
u16; variance u8 (0 invariant, 1 out, 2 in); flags u8 (0x1 class, 0x2
struct, 0x4 notnull, 0x8 unmanaged, 0x10 new(), 0x20 allows ref struct).
TypeConstraints (4B): type row each.
Types (16B): kind u8 (1 named, 2 array, 3 pointer, 4 nullableValue,
5 tuple, 6 functionPointer, 7 typeParameter, 8 dynamic, 9 error); nullable
u8 (0 none, 1 annotated, 2 notAnnotated); reserved u16 = 0; flags u8 (0x1
has-return); spelling atom u32|ABSENT; childStart u32; childCount u32.
TypeChildren (8B): label atom u32|ABSENT; type row u32.
Attributes (8B): owning declaration row u32; spelling atom u32 (the whole
application, arguments included, non-empty).
Docs (20B): declaration row u32; file atom u32; start u32; end u32
(start<=end); xml atom u32 (the full <member ...>...</member> wrapper
included).
References (28B): owner row u32; target row u32|ABSENT (in-image target);
spelling atom u32 (written spelling, non-empty); file atom u32; start u32;
end u32 (start<=end); tag u8 (1 invocation, 2 object creation, 3 member
access, 4 using directive, 5 interface-implementation binding); 3 reserved
zero bytes.

## Lowering-side conventions the producer must satisfy
(The consumer is compiler/driver/lower/csharp.rs — read its doc comments and
its tests/compiler/driver/tests/csharp_image.rs fixture for precedent.)
- Type declarations carry declared_type = their own named type row (spelling
  = the metadata fully-qualified name, same bytes as the qualified atom).
  Delegates carry declared_type = the invocation return's type row; members
  carry their declared/return type row; namespaces carry ABSENT.
- Array rows: childCount == rank; child 0 is the element row. `int[,]` ->
  2 children. NullableValue rows: child 0 = inner. FunctionPointer rows:
  children = [parameters..., result?] and flags 0x1 set iff a result child
  exists. Tuple rows: children = labelled elements. Named rows: children =
  type arguments in order.
- In-file named uses must resolve: the named row's spelling atom bytes MUST
  equal the referenced declaration's qualified atom bytes. Foreign
  System/* types keep their metadata fully-qualified spelling
  ("System.Int32", "System.String", "System.Decimal", "System.Void" ...
  these exact spellings are how the consumer recognizes primitives).
- Reference owner-relative law: a reference row's start MUST be >= its
  owner row's declStart (the consumer cannot host earlier spans). A
  file-header using directive precedes every declaration and therefore has
  no hosting row: usings declared at file level are not emitted; usings
  inside a declaration's span are emitted owned by that row. Document this
  in the producer with a comment; it is the wire's ownership grammar, not a
  degradation.
- Recursive type depth budget is 64 (the consumer's documented budget);
  deeper graphs must fail the producer loudly (OracleFailure), never
  truncate silently.
- Partial roles: type parts written `partial` are role 1; partial
  methods/properties: the bodyless part is role 1, the body-bearing part is
  role 2 (Roslyn PartialDefinitionPart/PartialImplementationPart).
- Every declaration name atom must be non-empty; the producer fails loudly
  on Roslyn emitting an empty name (existing law).
- The bound source digest is over the RAW file bytes on disk (BOM included)
  and every span is a UTF-8 byte offset into those same bytes (reuse the
  existing preamble + UTF-16->UTF-8 offset-table machinery).

## Must-prove (falsifiers)
R-PRO-1..R-PRO-4 in ../proof-matrix.md. Concretely:
1. A fixture source (committed under tests/fixtures/producer/) exercising AT
   MINIMUM: file-scoped namespace; class; struct; interface; enum + member
   with constant; record; record struct; delegate; field (const + static
   + volatile + required); property (get/init set, required, static);
   indexer; event; constructor; method (async, iterator, extension,
   static, abstract/virtual/override/sealed, extern); operator (implicit,
   explicit, checked, arithmetic); conversion; partial class (two parts);
   partial method (both halves); explicit interface implementation; generic
   type with out + in parameters and class/struct/notnull/unmanaged/new()
   /allows-ref-struct constraints; parameters covering value/in/ref/out/
   ref readonly/params/defaults; locals irrelevant. Type zoo: int[]; int[][];
   int[,]; int?[]; (int A, string)?; Dictionary<string, List<int[]>>;
   int*; delegate*<int, void>; delegate* unmanaged[Cdecl]<int, int>;
   dynamic; an unresolved error type; nullable annotations on fields.
   Docs: <summary> with <see cref/> + <paramref/>, <exception cref/>;
   attributes with constructor + named arguments (e.g. Obsolete("...", true),
   EditorBrowsable(Never)); usings (in-file, inside nothing — note the
   header-using grammar above); invocations, object creations, member
   accesses resolved inside methods; explicit interface implementation
   binding reference.
2. tests/producer_parity.rs: include_bytes! the committed fixture images;
   open each with CSharpImage::open; deep-assert the decoded cells against
   committed constants (source truth) for every fact class above. When
   COMPILER_CSHARP_COMPILER (or dotnet on PATH) exists, ALSO regenerate the
   images via the real producer and require byte equality with the
   committed fixtures; when absent, the committed-fixture assertions still
   run (no #[ignore], no skip).
3. Mutation falsifiers (in-test): for each structural class — declaration
   kind byte, flags byte, partial byte, refKind byte, variance byte, type
   kind byte, nullability byte, reserved non-zero, atom out of range,
   inverted span, coordinate out of section — mutate one committed fixture
   image, recompute the domain digest so checksum rejection cannot mask the
   structural rejection, and require the exact ImageError variant/operands.

## Environment receipts
dotnet: /var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk
(export PATH and DOTNET_ROOT). Build: `dotnet build -c Release` and
`dotnet restore --locked` must pass inside helper/. Rust gate:
`CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
cargo test -p compiler-languages-csharp --offline` — all existing tests
(protocol, image) must stay green.

## Bounds
Fixture images < 64 KiB each; <= 4 committed fixtures. No new C# or Rust
dependencies. No unsafe. The producer is deterministic: two runs on the same
inputs must produce byte-identical images.

## Forbidden
Do not touch the reader, the driver, the lowering, other lanes, the corpus
tests (another worker owns them), or any manifest outside helper/.
No silent degradation: unresolved external types are Error nodes (kind 9),
not omissions; every Roslyn fact class the fixture names must be present.

## Checkpoint & return
Commit each coherent checkpoint (formatting pass included) with message
prefix `feat(csharp-producer):`. Return: commit hash; exact commands run and
their tail output; the fact-class -> fixture-image -> assertion table; the
mutation falsifier table; smallest remaining red row.
