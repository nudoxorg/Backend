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

Ambient execution checkpoint: an explicit `ExecutionCheckpoint` only reaches code that holds the
database it rides on, and the deep solver walks (variance analysis, definition-dependency
closures, referenced-type visitors) ran with no database in hand or over one that carried none,
so one hot file query could spend minutes without ever observing its work slice or the package
deadline. `tsz-common::ambient_checkpoint` closes that class: the owner of a unit of work
(`ParallelCheckPlan` for a lazy file query, a file check, a library check) installs its
checkpoint for the calling thread with `enter_ambient_checkpoint`, and the shared chokepoints
call `ambient_poll()`. A poll is one thread-local read with nothing installed; with a checkpoint it
charges one work unit per `AMBIENT_POLLS_PER_UNIT` polls, reads the checkpoint once per 64, and
returns `false` once it has stopped. The rule for new code: a loop whose trip count is not
bounded by its input's syntactic size, or a recursion that is not metered by `evaluate`,
`check_subtype_inner` or `get_type_of_node_with_request` (whose entry points already poll the
explicit checkpoint), polls `ambient_poll()` and unwinds with its conservative placeholder when
it returns `false`. `RecursionGuard::enter` polls for every profile that is not already metered
at its entry point, treating a stop as an exhausted iteration budget; the referenced-type walker,
the definition-dependency closure walk and the closed-evaluation commit poll directly.

Variance work bounds: the completed-visit memo of `VarianceVisitor` is keyed by the visit
context including the type-parameter names bound by the enclosing generic signatures and mapped
types, so a type reached through many paths inside a generic method is measured once per
context (it used to be re-entered through every path, ended only by the 100k-visit cap of each
walk). Effective variance masks (`strictFunctionTypes` / method-bivariance aware) are shared
through the same universe-wide, fingerprint-validated store as declared masks, keyed by the
options that shape them, instead of being rebuilt from scratch for every relation query.
Dependency edges of the evaluation caches are derived once per (key, result): rewriting an
entry with the value it already holds no longer re-derives and re-registers its transitive
definition closure.
