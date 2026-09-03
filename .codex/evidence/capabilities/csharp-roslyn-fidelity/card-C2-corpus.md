# Card C2 — real nuget corpus end-to-end (registered role: nudox_luna_implementer)

## Baseline & owned paths
Baseline commit: c4b6ed32e. You own EXACTLY:
- compiler/driver/tests/csharp_corpus.rs (new)
- compiler/driver/tests/csharp_support/mod.rs (new, test-support module)
No other path may be written. helper/**, languages/**, lower/**, lib.rs of
any crate are forbidden (the producer is another worker's card; you consume
it as built by dotnet).

## One public terminal
Twenty REAL nuget package artifacts, each lowering end-to-end:
`nuget:pkg@ver` -> locate -> bounded fetch (digest-verified) -> unpack ->
Roslyn authority image (produced by the vendored oracle) -> canonical
fragment -> durable publication -> reopen -> index build/seal -> second
generation; decoded IR asserted against committed source-truth tables.

## Locked corpus (verified live on 2026-09-03; exact `.cs` counts recorded)
1  nullable@1.3.1                      92 cs   primary: Nullable/NullableAttributes.cs (verify actual path after unpack; pick the largest file if several)
2  xunit.assert.source@2.9.3           77 cs   primary: the largest .cs
3  isexternalinit@1.0.3                10 cs
4  polyfill@2.0.0                      71 cs
5  devlooped.tablestorage.source@5.5.0 88 cs
6  tinyioc@1.4.0-rc1                    2 cs   primary: TinyIoC.cs
7  ramltoopenapiconverter.sourceonly@0.21.0 16 cs
8  ramltoopenapiconverter.sourceonly@0.8.0  15 cs
9  esp-net-source@0.6.4                46 cs
10 esp-net-source@0.2.3                32 cs
11 nullability.source@2.3.0             4 cs
12 nullability.source@2.1.0             4 cs
13 morelinq.source.moreenumerable.distinctby@1.0.2     1 cs
14 morelinq.source.moreenumerable.pairwise@1.0.2       1 cs
15 morelinq.source.moreenumerable.acquire@1.0.2        1 cs
16 morelinq.source.moreenumerable.assertcount@1.0.2    1 cs
17 morelinq.source.moreenumerable.batch@1.0.2          1 cs
18 morelinq.source.moreenumerable.generate@1.0.2       1 cs
19 morelinq.source.moreenumerable.generatebyindex@1.0.2 1 cs
20 tinyioc@1.3.0                        1 cs
Primary selection law: the lexicographically-first `.cs` at the shallowest
depth whose decoded length is the row's largest (deterministic; record the
exact relative path in the row's truth table). You must confirm each
primary path while authoring (one live fetch) and pin it as a constant.

## Contracts
- PURL: parse `nuget:ID@VERSION` (typed rejection otherwise; reject wrong
  ecosystem, missing version, double @).
- Locate: `https://api.nuget.org/v3-flatcontainer/{id-lower}/{version}/{id-lower}.{version}.nupkg`
  plus sidecar `{same}.nupkg.sha512` (base64 SHA-512). HTTP via ureq
  (already a dev-dependency) with a 30 s global timeout. Hand-roll the
  base64 decode in the support module (~25 lines, typed error) — no new
  dependency is permitted.
- Fetch: bounded by an explicit byte cap and an Instant deadline; observe
  both caps exactly (typed Cap{cap,observed} / Deadline{observed}).
  Verify SHA-512(archive) == decoded sidecar; mismatch is a typed Digest
  rejection carrying both operands.
- Unpack: a minimal ZIP central-directory reader in the support module
  (locate End-of-Central-Directory from the tail, iterate central entries,
  support methods 0 stored and 8 deflate via flate2::read::DeflateDecoder).
  Guards: entry count cap (4096), per-entry size cap (4 MiB), total cap
  (32 MiB), reject absolute/parent-traversing names (typed Path{path}),
  corrupted structures -> typed Archive{cause}. Do not trust local headers'
  sizes for bounds; use central-directory values.
- Oracle: build once per process from
  `{CARGO_MANIFEST_DIR}/../languages/csharp/helper` with
  `dotnet publish oracle.csproj -c Release --nologo -o <fresh tmp>`
  (packages.lock.json is committed there; restore resolves Roslyn 5.6.0).
  Run `dotnet exec <published>/oracle.dll --mode source --root <unpackdir>
  --assembly-name <pkg identity> --authority-image --source-binding
  <primary> --out <img>`; bound by deadline (120 s), capture stderr,
  non-zero exit or missing image -> typed rejection with stderr excerpt.
  The dotnet executable comes from COMPILER_CSHARP_COMPILER (falls back to
  PATH `dotnet`; typed failure if neither — never a skip).
- Compile: `compile()` with profile CSharp14 / Stage::LowerIr / source =
  primary bytes / authority = SemanticAuthorityInput::CSharp{image};
  output buffer 8 MiB; diagnostic 4 KiB. Identity law: the fragment's
  source ContentId must equal the primary bytes' digest.
- Journey skeleton: mirror compiler/driver/tests/python_purl_lifecycle.rs
  (read it; it is the precedent): publish_compiled over DurablePublisher,
  shutdown, reopen, open_published, fragment equality checks, index
  build/seal/encode via server_index_build/publish, then generation two:
  append `\ninternal static class NudoxGenTwoProbe { internal const int
  Marker = 2; }\n` to a COPY of the primary source, regenerate the image
  through the oracle for the modified file, compile, publish through a
  second journal over the SAME artifact store (the python file's
  journal-second precedent), reopen, and require the probe symbol in the
  second generation's entities. Finally re-validate the first generation's
  fragment bytes with FragmentView::validate (old fragments keep
  validating).
- Deep probes (per row): committed truth table with AT LEAST three exact
  symbol names expected as entities (chosen from the real package sources,
  verified while authoring) and one exact EntityKind per symbol; one row
  document-link probe where the package has XML docs (nullable,
  devlooped.tablestorage.source have them); occurrence-plane presence
  wherever the bound file contains resolved calls. You derive the truth
  tables from the actual fetched sources — never from imagination — and
  commit them as constants with the source citation in a comment.
- Transport falsifiers (live): 1 KiB cap breach; 1 ns deadline; corrupted
  archive (flip a mid-byte after download -> typed Archive); tampered
  digest (compare against a wrong constant -> typed Digest).

## Environment receipts
dotnet at /var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk;
network to api.nuget.org verified. Rust gate:
`CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
COMPILER_CSHARP_COMPILER=<dotnet path> cargo test -p compiler-driver
--offline --test csharp_corpus`. NOTE: another worker owns
languages/csharp/helper — build your proofs against the helper AS OF YOUR
BASELINE (it currently does not compile; coordinate through the manager:
run your journeys only after the manager confirms card C1's first
checkpoint landed). Until then, land the support module + PURL/transport/
unpack falsifiers and the full journey skeleton behind the locked corpus
table.

## Bounds
Total test wall time < 10 minutes on this machine; entry/size caps above are
hard. No new dependencies (ureq + flate2 + sha2 exist as dev-deps; hand-roll
base64 and zip). No unsafe. No #[ignore], no skip: every test either runs
its real journey or fails typed on a missing tool.

## Forbidden
Do not modify the oracle helper, the reader, the lowering, other lanes'
tests, or any manifest (dev-dependencies already suffice). Do not
synthesize package content: the only derived content allowed is the
documented gen-2 probe append. Do not commit downloaded artifacts.

## Checkpoint & return
Commit coherent checkpoints (prefix `test(csharp-corpus):`). Return: commit
hash; the 20-row outcome table (purl, primary path, entity count, doc links,
occurrence count, index verdict, gen-2 verdict); exact commands + tails;
falsifier results; smallest remaining red row.
