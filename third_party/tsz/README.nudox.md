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

Program-wide facts and lazy sealing: `tsz-checker` derives the answers that depend only on
the sealed binders and the compiler-owned module-resolution outcomes once per program
(`SharedProgramFacts`: whether any file augments a module, which names are ambient modules,
and a concurrent memo of the request consensus for each importer/specifier pair) and shares
them by `Arc` with every file checker and every cross-file delegation child, instead of each
call rescanning every binder or each child context recomputing its parent's memo.
`PreparedProjectCheckerImage::seal_for_queries` seals the same views without running any file
or library check, for a caller that reads semantic facts through lazy per-file queries and
never reads checker diagnostics.
