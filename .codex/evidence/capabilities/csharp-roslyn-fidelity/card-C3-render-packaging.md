# Card C3 — rendering goldens + packaging integration (registered role: nudox_luna_implementer)

## Baseline & owned paths
Baseline commit: ccd935916. You own EXACTLY:
- compiler/driver/tests/csharp_render.rs (new)
- compiler/driver/tests/csharp_packaging.rs (new)
- goldens under compiler/driver/tests/fixtures/csharp_render/** (new)
No other path may be written.

## Context
The lane now has: a v3 producer verified byte-exact against committed
fixtures (compiler/languages/csharp/tests/fixtures/producer/), a corpus of
20 real nuget artifacts running the full journey
(compiler/driver/tests/csharp_corpus.rs, 30/0), and typed projection
terminals. Trunk landed render truth (compiler/ir/render.rs — read it: the
rich compatibility view's Display, write_visibility, prefix-free unknown
visibility) and the Ir view APIs the rust/typescript/python render tests
already consume.

## Task 1 — render goldens (R-REN-1)
Precedents: compiler/driver/tests/rust_render_golden.rs,
typescript_render.rs, python_render.rs — mirror the winner's structure.
1. Build the Ir through the real pipeline from the COMMITTED fidelity
   fixture (include_bytes! ../../languages/csharp/tests/fixtures/producer/
   fidelity.ncaimg + fidelity.cs — same include pattern as the corpus
   conformance test) and from the committed unicode fixture; compile_ir
   with CSharp14 + the authority image, then render the rich view exactly
   the way the precedent tests do.
2. Assert byte-exact goldens committed under fixtures/csharp_render/
   (include via include_str! and compare; store the expected text; a
   mismatch fails with a typed diff error). The goldens must show:
   - interface as trait-shaped, class/record/struct records, enum, delegate
     as function;
   - `+` operator and `Widget` constructor named from their source slices;
   - property/indexer/event members with their declared types;
   - prefix-free unknown visibility law (trunk's write_visibility) wherever
     the C# lane leaves visibility Unknown — no invented visibility;
   - type rows: primitives by width/signedness, string, tuple/arrays,
     annotated `T?`, foreign types as typed unknowns retaining spelling;
   - doc summary text fragments and at least one local doc link.
3. Determinism: render twice, compare both to the golden.
4. Old fragments keep validating: re-run FragmentView::validate on the
   committed image's fragment bytes inside the render test as a final
   assertion.

## Task 2 — packaging integration (R-INT-1)
Codify the producer packaging contract that everything above depends on,
as an ordinary integration test:
1. Oracle build contract: `dotnet restore --locked-mode` +
   `dotnet build -c Release` inside
   {CARGO_MANIFEST_DIR}/../languages/csharp/helper succeed with zero
   warnings (TreatWarningsAsErrors is on); packages.lock.json exists and
   pins Microsoft.CodeAnalysis.CSharp 5.6.0 exactly (parse it; assert the
   direct dependency and resolved version); assert helper/obj and helper/bin
   are NOT tracked by git (`git ls-files` empty for those paths) so build
   output never enters the source tree.
2. Round-trip contract: build the oracle once, emit an authority image for
   a temp-dir copy of the committed fidelity fixture, and require byte
   equality with the committed fidelity.ncaimg (this is the packaging
   invariant: the committed bytes are exactly what the pinned
   Roslyn-closed build emits).
3. Typed environment contract: with COMPILER_CSHARP_COMPILER pointing at a
   deliberately nonexistent path, the oracle runner reports the exact typed
   tooling-unavailable failure (mirror the protocol.rs
   probe_dotnet_path law); when the variable is unset and PATH dotnet
   exists, the journey still runs. No skip, no #[ignore]: a missing dotnet
   in the environment fails these tests typed — the corpus tests already
   require the toolchain the same way.
Gate env: dotnet + COMPILER_CSHARP_COMPILER as in the corpus card;
CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp.

## Gates
- cargo test -p compiler-driver --offline --test csharp_render --test csharp_packaging — green
- cargo test -p compiler-driver --offline --test csharp_corpus --test csharp_image — stays green
- cargo test -p compiler-languages-csharp --offline — stays green
- cargo test -p compiler-driver --offline --lib lower::csharp — stays green

## Bounds
No new dependencies, no unsafe, goldens < 64 KiB total, <= 4 golden files.
Rendering asserts the CURRENT render truth; if rendering loses a fact the
producer retains, record the row as a finding and report — do not edit
render.rs.

## Checkpoint & return
Commits prefixed test(csharp-render): / test(csharp-packaging):. Return:
commit hash(es); the golden inventory (file -> what it pins); exact
commands + tails; any rendering findings; smallest remaining red row.
