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

Captured native web-program discovery also shares the checker's JSX pragma
owners, resolves automatic/configured/source types through native package
exports and `typesVersions`, and streams borrowed reference directives under the
admission work checkpoint. Source path/type reference observations use the
existing immutable request-outcome map, with typed reference kinds and exact
UTF-8 value offsets. Admission validates those kinds, offsets, modes and values
against the source. Captured misses produce TS6053/TS2688 in the retained checker;
admitted checks never use the legacy reference-path disk probe. Reference rows
do not enter ordinary module-resolution projections or consensus.
Every source path/type directive requires its own explicit outcome, unless the
resolved checker policy enables `noResolve`. Missing, extra, duplicate or invalid
rows refuse admission. Prepared images and query sessions also validate reference
coverage before accepting an immutable program map, including raw caller maps.
Public one-shot file/function checking and statistics APIs now return typed
resolution failures as well. The WASM bindings propagate them as JavaScript
errors without changing successful return values. Legacy outcome setters defer
mandatory coverage until checking receives its `noResolve` policy; the native
options-aware setter performs full admission immediately.

Program/configuration diagnostics have a retained channel separate from the
per-source checker result. Missing configured `types` entries retain native
TS2688 and their entry-point reason only for captured NotFound outcomes; provider
failures still refuse admission. TSZ's captured diagnostic config loader retains
native config diagnostics, and its parser diagnoses terminal `**` includes with
TS5010 against original JSONC string-token spans while keeping valid selectors.
Inherited diagnostics retain their defining config file. Config files and global
diagnostics do not become semantic source members, and diagnostic-only programs
do not acquire a fabricated root. The diagnostic vector participates in program
identity and is available through `program_diagnostics()` or the combined
`diagnostics()` iterator; per-source `CheckResult` counts retain their meaning.

The shared source directive stream also owns `no-default-lib` for native closure
discovery. A true directive in the captured source graph suppresses configured
and default libraries without rewriting `noLib`; explicit source library
references still load unless `noLib` is set. Sources are scanned before embedded
libraries are linked, so embedded libraries' own directives do not suppress
themselves. The vendored parser's legacy `has_no_default_lib` field remains
unused; this integration applies the directive at the native program's library
selection boundary. These are native source repairs with regression coverage;
they require separately admitted compilation and execution validation.
