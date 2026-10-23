# Frozen f11 TypeScript answer review

## Provenance

Reviewed `/tmp/nudox-ci-green-fixed-evidence/replay.json`: `source_sha=f11d4116d57b8d4c24f675059cd908f3dd61b17e`, artifact manifest SHA-256 `72d2073a5b379755da9e8899e66f835abf8b3182ea1d8c84f86d39ed6f9e2d49`. Both native and explicit compatibility records are present. The independent syntax report in the same directory identifies TypeScript Compiler API 5.9.3 and 21 source files; all listed source-file bytes and SHA-256 values match `/tmp/nudox-ci-green-ts-source`.

## Source-backed identity/kind/anchor check

Counts are from `/tmp/nudox-w2-ts-review-check.py`, reproducible output `/tmp/nudox-w2-ts-review-check.txt`. In each route, the 208 unique source-backed identities come only from direct search/page `records[]` entries; their identity sets are exactly equal across compatibility and native. They are not the outline count. Separately, recursively traversing all outline `roots` in the answer and continuation responses yields 1,994 unique source-path tree nodes across 20 files; `src/index.ts` is export-only/re-export-only and has no authored declarations in the independent AST.

Of those 1,994 outline nodes, 386 unique `(path,line,name)` anchors match independent AST authored declarations, with no declaration-kind mismatches (`ClassDeclaration→class`, `InterfaceDeclaration→interface`, `FunctionDeclaration→function`, `MethodDeclaration→method`). The independent AST has 18 private-method declarations (15 distinct private names); none has an exact `(path,line,name)` outline anchor, though all 15 names occur somewhere in returned outline data. Treat those as semantic-carrier occurrences, not verified authored source anchors. A separate source-line check finds 1,970/1,994 outline node names present on their stated line; the remaining 24 are import nodes whose coordinates point to the opening line of a multiline import declaration. Other returned local/parameter/import rows are not all AST declarations and should be assessed by their record kind and enclosing source context rather than counted against this AST declaration census.

Specific anchors agree in both routes:
- `QueryClient`: class, `src/queryClient.ts:61` (byte 1363).
- `CancelledError`: class, `src/retryer.ts:58` (byte 1653).
- `getQueryState`: method, `src/queryClient.ts:266`.
- `QueryState`: interface, `src/query.ts:46`.
- `DefaultError`: type alias, `src/types.ts:37`.

The pinned source says `CancelledError extends Error`, with optional `revert`/`silent` and optional `CancelOptions` constructor. `QueryState` has exactly `TData = unknown, TError = DefaultError`. `getQueryState` defaults data/error to `unknown`/`DefaultError`, applies conditional tagged-key inference, and returns `QueryState<TInferredQueryFnData, TInferredError> | undefined`. `DefaultError` falls back to `Error` because `Register` is empty here. These source facts are review expectations, not types serialized by the CLI/MCP outline/search replies: those replies contain authored identities/kinds and selected-symbol IDs, not rendered type signatures. The independent AST is likewise a syntax/name/anchor baseline, not a semantic type oracle.

## Native versus compatibility and remaining limits

The latest packet contains both routes at 21/21 compiler coverage per the run owner; this supersedes the older 19/21 compatibility evidence. Native and compatibility use different source roots and package row counts (native 3,175; compatibility 3,131), so matching source identities is useful but should not be taken as proof that semantic projections are equal. Both records have zero continuation errors; their full walks have 202 vs 196 evidence entries, and each paged outline/detail/search chain ends with `more=false`. The standalone initial outline response remains truncated, as expected; full continuation evidence is present.

The prior native-only 13/21 `DefaultError` cycle and older compatibility `DuplicateDeclarationIdentity` packet were different failures. The current f11 coverage improvement is supported by the owner’s native 21/21 diagnostic; this independent review found no source-backed identity or declaration-kind misattribution in the returned answers. I cannot independently approve semantic type correctness from this replay alone because it does not include the compiler’s resolved type answers or per-file type records. Keep this distinction explicit when using the coverage result. No checked-in TS golden contains this package’s `QueryClient`/`CancelledError` answers, so this report is a proposed review baseline, not an accepted golden or an instruction to overwrite one.

Performance is diagnostic-only and not acceptance evidence: the packet reports native CPU 56.71s / peak RSS 767,280 KiB versus compatibility CPU 42.10s / peak RSS 632,048 KiB. This review makes no performance or CI-green claim.
