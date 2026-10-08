# Native Go source selection checkpoint

The old authority image could apply an active package's call spans to a compiler-ignored platform file, causing ordinary multi-platform Go projects to fail lowering. The helper now uses the same Go build context and environment for go/packages and go/build.MatchFile, retains actual compiler-owned external-test namespaces, and emits a source-specific inactive image with zero active declarations or references. A dormant malformed body records unavailable dormant declarations without poisoning the active package.

The compiler's actual selected and ignored sets remain authoritative. Target, architecture, cgo and tag contradictions refuse; no Rust filename table selects source. Ignored-file witnesses include the actual package/file identity and exact target policy. Header parsing is separate from dormant declaration parsing. The union of compiler-returned ignored operands across same-directory test variants lets the existing external-test package own its source; it never invents a `_test` import path.

## Source correspondence

Native source is `753a79517fc5d936b57b33464fe81df31b7796b2`, tree `b846cec89189499a02330dde6e6902785dd79e5f`, on canonical predecessor3bb. Root reviewed all production changes and the compiler-produced regressions, including the final native test repairs. All ten postfiles replay exactly on current canonical `96d82a14eea7dbe2167c0c962b5887b6fab67c4b`;17,944 other existing entries and Cargo.lock are preserved. The lock SHA256 is `de731929bbf72c5220e59c0543aaddd06fcc2bc16899741a9d6cb5546e60780e`.

The Rust-side engine change adds a genuine lower/admit test; it changes no engine production code. The frontend Rust change updates an existing test's expected compiler-selected constraint witness. The existing build script recursively embeds non-test Go sources, including the new selection.go; no development checkout path is required at runtime.

## Actual native controls

The Go1.26.4 helper was genuinely compiled and linked on the remote Mac with `CGO_ENABLED=0`, `-mod=vendor`, `-p=2`, `GOMAXPROCS=2`, an owned fresh HOME/cache and explicit offline policy. Eight parent tests and four platform subcases passed, zero failed/skipped:

- Native target, architecture, tags and inactive image selection, including Darwin/Windows build contexts.
- Both cgo policies and rejection of contradictory selection.
- The exact tracked rest-server listener pair from v0.14.0, with its recorded upstream origin.
- The closed parent target policy and exactly one authority source owner.
- Native external-test package ownership and foreign call identity.
- A compiler-ignored malformed body alongside known active documentation, inferred return type and call facts.

The earlier c5 source genuinely passed six controls and failed two. The successor repairs actual same-directory ignored-file ownership; its test additionally calls the required documentation finalization before asserting docs. A final test-only correction uses the existing empty foreign-package field for a local call while separately asserting the actual owning package. Original failed evidence is retained, not replaced.

Root independently verified all99 source input hashes and Git objects (1,340,654 bytes), the raw compiler/linker process observations, compile/test receipt linkage, all raw output hashes, eight actual parent PASS lines/four subcases, before/after source/tools/image hashes and kernel retirement/permit release. Image SHA256 is `581a5fe147ddea39f12d547d3a5a7b365f9a10d8e5bd98b0ec9025303aabfea7`. Compile10.163s and test4.200s are single observations, not comparative benchmarks.

Raw archive: `/Users/mileswirht/Downloads/sol61-go-build-selection-20261008/753a-native-compile01-test01-raw.tar.gz`, SHA256 `ea6504f28d74172bf73028c46917cf8acb2def8f9a46f565a76bef58e90d42e7`. [Root's checked evidence record](go-native-selection-evidence-20261008.json) SHA256 is `dc62d794b11f6acc4dd59d91602404ef82be0112eb5e39a23df9f75fbffb29e3`.

## Remaining acceptance

The two genuine Rust-side selected-loader/cold and engine lower/admit controls are still unrun, queued behind the reserved Rust and cold-search gates. This combined canonical tree is not built; no current installed CLI/MCP whole-Go-project pass is claimed. A compiler-ignored external-test source with no active external-test package still needs explicit per-file availability rather than an invented package owner; that separate repair remains open. These native helper passes do not certify current TypeScript, Python or Rust, or a new installer.
