# Cold reader one — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_clippy_cold_reader_one`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Cold-reader calibration for card digest `84dbe298d851af1057e9abfdae7fd7893d52c37c3c6917e03830930f35100a6f`:

- Custody verified: commit `5d197c858b1ea2caa3af8a3a5476446e2e46aa1d` produces the exact required SHA.
- First terminal is narrow and clear: an ordinary public `tests/coordinates.rs` consumer proves the actual exported crate rejects exactly one `EntityId`-as-`TypeId` use with exactly one `E0308`, while the literal `EntityId::new(7)` → `TypeId::new(7)` mutant compiles and makes the predicate fail.
- Scope is unambiguous: `src/lib.rs` is read-only and digest-pinned; the test file is the sole writable implementation path. No compiler, manifest, dependency, API, conversion, unsafe, allocation, or unlisted-path repair is permitted.
- Budget is coherent: 93-line skeleton / 110-line hard cap leaves 17 unused lines; 14-line baseline plus 79 added lines matches the forecast. Crossing the reserve, 20%/25-line variance, or adding public surface is a stop.
- Exact-one rlib behavior is causal: clean preparation and the fixture both require one resolved `libnudox_ir_vocab-*.rlib`; zero or multiple candidates must fail before `--extern`, avoiding stale/shadow artifact acceptance.
- Target binding is appropriately tied to the running test executable’s `target/debug/deps` directory, matching the manager’s pinned `CARGO_TARGET_DIR=domains/ir/target` gates.
- The known `raw`/public `new(u32)` rebrand weakness is correctly preserved as an explicit baseline limitation, not silently repaired or claimed disproven.

Cheap misread/rejection: a worker could add a “helpful” raw validation/conversion API, modify `src/lib.rs`, use a local shadow type or doctest, accept any compiler error, or leave multiple rlibs and select one. Each is directly rejected by the card’s negative space and evidence predicate.

No material ambiguity or blocker found in the card. The only unverified items are implementation-time facts: skeleton byte identity/content, actual test fixture behavior, current toolchain diagnostics, fresh rlib digest/path, raw stderr, literal mutation result, and full gate results. Those cannot be established during card-only calibration.

Verdict: `CALIBRATION PASS` / builder authority may proceed, contingent on the other independent Luna reader, plausible-misreader, and separate Terra reviewer agreeing on the same boundaries. Calibration alone does not accept implementation.
