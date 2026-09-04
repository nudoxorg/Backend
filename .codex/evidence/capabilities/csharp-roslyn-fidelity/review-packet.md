# Review packet — csharp-roslyn-fidelity candidate d2fed1732

You are the independent Terra reviewer. Apply
.opencode/skills/review-rust-gem/SKILL.md hostilely, from the snapshot at
/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/csharp-review-snapshot
(read-only). You receive contracts and evidence, not builder rationale.
Return ranked findings (blocker/major/minor) and cleared suspicions. Make no
edits. If you cannot reproduce or locate a claim, mark it UNVERIFIED — do
not convert uncertainty into approval.

## Scope under review (lane paths only; other lanes' files in the snapshot are context)
- compiler/languages/csharp/** (reader image.rs, oracle.rs, helper producer, tests+fixtures)
- compiler/driver/lower/csharp.rs, compiler/driver/native/work.rs (one match arm), compiler/driver/types/{lowering,terminal,compile}.rs (CSharpProjection surface), compiler/driver/lower.rs (funcptr arm + occurrence bound + type-parameter plumbing), compiler/driver/tests/{csharp_corpus,csharp_support,csharp_render,csharp_packaging,native_compile/*,*}.rs
- .codex/evidence/capabilities/csharp-roslyn-fidelity/* (contracts)
- .gitignore (two helper lines)

## Frozen contracts to review against
1. Reader law (image.rs): version-3 wire; open() validates envelope, digest,
   every section; trusted projection must not repeat raw->closed conversion
   or use fallback values (ParamIter/GenericIter items are Result;
   type_node's arms are typed rejections; one shared decode per vocabulary).
2. Producer law (helper/): emits exactly the v3 wire; deterministic;
   byte-exact committed fixtures under
   compiler/languages/csharp/tests/fixtures/producer/ bound by the clean
   relative invocation (cwd = fixtures dir, --root . --source-binding
   <file>.cs).
3. Lowering law (driver/lower/csharp.rs): every projection fault retains
   typed operands through CSharpCollectError::Projection ->
   CompileFailure::CSharpProjection; no evidence-erasing fold; the
   wire-saturation gaps (variance, params) are pinned by
   wire_saturation_gaps_stay_image_retained_pending_trunk_cells with the
   escalation packet at .codex/.../escalation-wire-saturation.md.
4. Corpus law (driver/tests/csharp_corpus.rs): 20 REAL nuget artifacts;
   sha512 sidecar digest verification; bounded fetch; central-directory zip
   reader with caps + traversal rejection; full journey
   compile->publish->reopen->index->gen-2 per row; typed terminals (2
   TinyIoC rows at the shared 256-entity index bound; nullable row at the
   16-slot attribute list) carry both operands.
5. Render/packaging law: byte-exact goldens over the committed fixtures;
   packaging pins locked restore, Roslyn 5.6.0, untracked build outputs,
   and the byte-exact round-trip under the documented invocation.

## Claims to verify (reproduce, do not trust)
- cargo test -p compiler-languages-csharp --offline: 18 tests green (image
  6, producer_parity 4, protocol 8) — toolchain: dotnet required for
  producer_parity's regeneration test; without dotnet it prints a receipt
  and replays committed fixtures.
- cargo test -p compiler-driver --offline --lib lower::csharp: 19 green.
- cargo test -p compiler-driver --offline --test csharp_corpus: 30 green
  (requires dotnet + network).
- cargo test -p compiler-driver --offline --test csharp_render
  --test csharp_packaging: 18 green.
- `git ls-files` in compiler/languages/csharp/helper shows no bin/ or obj/.

## Hostile questions (attack these explicitly)
H1 Mix-fields: pick two valid declarations in any committed fixture image
   (e.g., the operator row and the interface row); can a hand-built image
   mixing their coordinates/atoms pass open() and project incoherently?
   If yes, name the exact law that fails to hold.
H2 Trusted projection: after C6, is there ANY decode path in image.rs that
   can produce a fabricated semantic value from an out-of-vocabulary byte?
   Walk every decode site. The remaining `unwrap_or(u32::MAX)` sites are
   error-operand constructions on failing paths — confirm none of them can
   fabricate a SUCCESS path.
H3 Owner-order: the producer's reference rows use owner-relative spans; can
   a producer-emitted reference violate reference.start >= owner.declStart
   through the record-positional synthesis or the explicit-interface rows?
H4 The funcptr arm in driver/lower.rs: has_result && child_count==0 is a
   typed Dangling rejection; is the zero-arity case (child_count==0,
   !has_result) truly legal through intern_tuple_elements and the product
   constructor?
H5 The corpus transport: zip central-directory reader — try to defeat the
   bounds (declare sizes smaller than local headers, overlapping entries,
   absolute paths, name with backslashes, ZIP64). The digest fallback
   (x-ms-meta-SHA512 header when the .nupkg.sha512 sidecar 404s) — is the
   fallback registry-authoritative and typed, or a silent degradation?
H6 The escalation packet's line-pin evidence (semantic.rs:1211-1226,
   extension_pools.rs:17, lower.rs hardcoded Invariant) — verify each line
   claim in the snapshot.
H7 Determinism: two oracle runs byte-identical; goldens asserted twice.

## Manager-disclosed residuals (verify and rank)
- go/java/rust lib test reds in the shared driver test target predate this
  candidate (go: 18 identical Version{4} fixture/reader drifts; rust: 6
  semantic reds reported by the unblock card; capacity tests affected by
  this candidate's authorized occurrence bound 1024->8192).
- The C# renderer drops producer-retained facts (nullability, params,
  effects, attributes) — render truth is trunk-owned; recorded as findings.
- The wire-saturation gaps remain image-retained pending the trunk fork.
