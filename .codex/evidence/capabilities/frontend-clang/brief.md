# Capability: frontend-clang — direct-libclang semantic frontend

## Brief

Public terminal: `ClangFrontend` in `compiler-driver` drives the direct libclang semantic
authority: exact caller source bytes -> bounded parse -> typed semantic facts (canonical 13-kind
vocabulary) with exact byte-unit spans and USR-derived stable identities; authority-unavailable is
a typed failure, never a green test or a keyword scan.

Non-negotiable laws (parity ledger):
1. Facts come from the real libclang authority; process success + keyword scanning is admission, not semantics.
2. Identity derives from source/package authority + source-relative paths (USR interning preserved from the recovered crate). Absolute paths, arena ordinals, and raw ids are not durable identities.
3. Typed cursor records only; no stringly/JSON scanning.
4. Authority-unavailable is a typed failure.
5. No unwrap/expect/panic in shipping paths; exact errors with retained causes; bounded memory (ported bounds).

Baseline (frozen, this worktree @ 95c860e4f, clean):
- `cargo test -p compiler-driver`: 13 passed, 5 failed env-only
  (`lowering::native_subset_declaration_forms`, `matrix::native_adapters_parse_real_source`,
  `rejection::native_rejection_retains_recipe_source_and_bounded_diagnostic`,
  `scanner::go_java_lowering...`, `scanner::native_subset_lowering...`)
  all `MissingHostTool { TypeScriptCompiler | GoCompiler }`.
- Host probe: clang 21.1.8 (nix, on PATH, also at /usr/bin/clang shim);
  linkable libclang = nix `clang-21.1.8-lib/lib/libclang.dylib` (reports `clang version 21.1.8`,
  pairs with the PATH clang). Apple CLT libclang exists (Apple clang 17.0.0) but pairs with no
  executable here -> rejected as authority. LIBCLANG_PATH is the single build authority; when
  unset the build compiles the graceful typed-unavailable mode.

## Coupling skeleton

module | invariant owner | public terminal | dependencies | state/control boundary
clang/mod.rs | analysis entry, bounds consts, seal, seam drive | analyze/drive (crate) | ffi, parse, source | caller scratch slices, cancellation, deadline
clang/ffi.rs | the one reviewed unsafe module: stable libclang C ABI subset | none | libclang dylib (link) | guards own CX handles
clang/parse.rs | transactional parse + journal + diagnostics + resources | ClangReport | ffi, source, identity, traversal | no fact prefix on failure
clang/cursor.rs | cursor kind -> SemanticKind mapping, spans, tokens, builtin types | visit_one | ffi, protocol, identity | main-file filter
clang/traversal.rs | C callback safety, ancestor path, capacity issues | visit_cursor | ffi, identity | no panic crosses C
clang/identity.rs | USR interner (source-authority identity), typed fact ids | IdentityInterner | ffi, protocol | scratch table + pool
clang/source.rs | byte-unit coordinate math | source_point | protocol | pure
clang/protocol.rs | canonical 13-kind vocabulary + typed facts | SemanticKind, ClangFact | none | closed enums
clang/error.rs | exact typed failures (borrowed) + owned seam mirror | ClangError, ClangFailure | protocol | no erasure
native/frontend.rs (Clang region only) | ClangFrontend registration | ClangFrontend::drive | clang | dispatch terminal
native.rs (mod + Clang arm only) | closed tool dispatch | parse_with_native_tool | frontend | exhaustive match
types/terminal.rs (additive variant) | driver terminal vocabulary | CompileFailure::ClangFrontend | clang::ClangFailure | additive only
build.rs | exact link-time libclang authority or typed-unavailable cfg | none | LIBCLANG_PATH | cargo cfg

test | weakened implementation it kills | exact oracle | retained error/source/owner
semantic: borrowed facts + identity refs | scanner-based fact faker | facts borrow source buffer; spans re-slice to names | source bytes, typed report
semantic: shadowed same-spelling fields | span-equality identity | refs[0].resolved == fields[0].span, refs differ | USR interner
semantic: comment/string bait | keyword scanner | no FakeComment/FakeString facts | real cursor traversal
semantic: wrong version seal | no-op seal | ToolVersionMismatch typed | expected + observed lens
semantic: nested owners | flat owner spans | exact enclosing extents | traversal path
semantic: caller/disk mismatch | dropped (canonical seam has a single byte authority; no on-disk second authority exists)
semantic: malformed source | partial-fact emitter | ParseRejected + first diagnostic, zero facts | transactional journal
semantic: header-owned diagnostics | spelling-based ownership | main-file filter keeps severity order | file-identity check
semantic: bounded include root | ambient include paths | -I include_root only | include_root input
semantic: external header type | silent drop | LibclangExternalIdentityUnavailable typed | typed terminal
bounds: identity/fact scratch | unbounded growth | typed provided/required | caller scratch
bounds: token capacity | unbounded tokenize | typed observed/limit | caller scratch
bounds: binding capacity limit/+1 | silent overflow | 1024/1025 admit; 33 vs 32 typed | interner capacity
cancellation: pre-cancelled | unchecked flag | Cancelled + zero facts | AtomicBool
cancellation: foreign language | — replaced: owned by the closed registry dispatch match (compiler-registry tests)
kind table (card 2) | wrong-kind mapping | source mutation changes the fact per kind; TryFrom/From round-trip; ALL order == canonical | closed match
driver journey (card 3) | shell-out stub | public compile() over real C fixture; malformed C -> ClangFrontend{ParseRejected} | typed terminal
unavailable (both modes) | green test | typed LibclangUnavailable failure | build cfg

resource | baseline | bound | measurement | rollback condition
identity scratch | caller slices | 256 KiB at seam; caller-sized at module | ClangReport caller_scratch_high_water | capacity error typed
fact journal | caller slices | 256 KiB at seam; 32-byte records | fact_scratch_high_water | capacity error typed
tokens | caller scratch/64B per token | c_uint cap | typed TokenCapacity | capacity error typed
allocation at seam | none (process pipes before) | two exact-bound zeroed Vec per analysis | allocation ledger | drop to caller-provided when spine lands
