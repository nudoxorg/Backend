# Local TSZ integration patch

This tree vendors the ten TSZ crates selected by the backend at upstream
revision `153b25adb393f134c61ddde7a14cc1ff1954fc2a` (see
`UPSTREAM_REVISION`). The upstream `LICENSE.txt`, crate manifests, source,
tests, and regression fixtures are retained.

The local patch adds immutable, hashable `ProjectSemanticOptions`, carries that
value through `TypeDatabase`/`QueryCache`, and exposes native parallel merge and
check entry points that accept the same project policy. Declaration-scoped
type-parameter origin is no longer activated by a process environment variable;
its independent reduction and HKT follow-up policies remain separately selected.
The declaration-scoped option also requests stable home-declaration election
when the merged `DefinitionStore` is built.

The backend root `Cargo.toml` patches this exact Git source to these paths. Do
not update the upstream revision or omit a workspace crate without reviewing
the lockfile, license, regression tests, and option plumbing together.

Per-file work isolation: `tsz-common` adds `FileWorkSlice`, a file-local allowance
carved out of the shared program checkpoint, and `ProjectExecutionBudget` now drains
its allowance to exactly zero on exhaustion. `tsz-core` adds
`PreparedProjectCheckerImage::with_file_work` / `ProjectCheckerSession::with_file_work_allowance`:
each file check or file query runs under its own slice, a file that spends it is
reported (never published), and the partially written image is discarded and rebuilt
without it by the caller. The checker's module-export lookups no longer repeat one
program-wide answer once per binder, and the request consensus for a source/specifier
pair is memoized; both made one named import cost work proportional to the file count.

The recovered React/generic variance hot-loop fix uses `with_ambient_checkpoint`
to lend a borrowed checkpoint only during a synchronous callback. Its private
restoration guard cannot escape or be forgotten by callers. Variance recursion
guards and instantiation polls observe the same file slice as explicit solver
queries. Variance completed-visit memo keys include bound generic parameter
names, so shared subtrees under the same complete context are visited once.
Shared effective-variance masks, partial dependency-walk publication and raw
lifetime-erased public guards are intentionally not adopted.
