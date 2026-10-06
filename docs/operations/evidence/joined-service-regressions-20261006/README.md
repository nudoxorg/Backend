# Joined service regression evidence — 2026-10-06

The exact `039c360d286962a6cf19488fb86caabd1d8c93de` service-lib run started 1,013 tests and reported 14 explicit failures before its owned process was stopped for the shared disk floor. It did **not** produce a complete suite verdict. The full log and resource-stop receipt are retained here.

The focused baseline logs reproduce nine of those failures independently. They distinguish invalid authority/identity fixtures, an invalid history chunk policy, fixture source smaller than handle overhead, unprepared subscription replies, stale-observation allocation accounting, and dependency-cache invalidation. The remaining lease/codec failures were investigated on a separate worker branch.

After repairs `1b1700507e`, `a3f20e27b2` and `6fd5cbac33`, the frozen source at `6fd5cbac334e5f84e6c7b60013e9de25076293a6` built its service-lib test executable successfully in 216.36 seconds. Five exact previously failing tests then passed: both semantic-shape controls, the direct shape identity certificate, immutable generation freshness, and failed advisory refresh/cold reopen with retained dependency facts. The test binary SHA-256 is recorded in `joined-dto20-root-repairs/attempt02/tests-receipt.json`.

The first repair build at `a3f20e27b2` failed because the new fixture used a nonexistent `ArtifactId::to_bytes` method. Its compiler log and receipt remain in `joined-dto20-root-repairs`; the correction uses the typed identifier's existing byte reference. There is no claim that the failed build passed.

The joined follow-up at `f47382a669` contains nine further targeted repairs, including the required nullable runtime-policy field visitor. Those repairs are awaiting a coherent warm-graph build and their exact tests. The five passing tests above were run **before** this follow-up; they are not a verdict for it.

The successful build still emits 267 warnings. This packet is neither a clean-Clippy claim, a complete service-suite pass, a real-package semantic acceptance, nor a 10,000-package pass. Full log hashes and source guards accompany each command.
