# Source declaration ranking checkpoint — 2026-10-08

Search now prefers a source declaration over an inferred helper type with the same name. The decision uses the selected canonical row's source availability and closed compiler evidence, not signature text or an ecosystem heuristic. Captured declaration sites retain this preference when source bytes are unhydrated or stale. Compiler external targets remain a separate lower tier.

Lexical name and package placement remain ahead of the new provenance criterion. Semantic composition includes the retained lexical prefix before a stable provenance sort and truncation, so provider candidates cannot evict an actual definition before that definition is considered. Inferred types and external targets remain available in the corpus and continuation pages.

## Reviewed source

The original four atoms are `7386400023`, `f47b3f191f`, `b507dfb416`, and `2a030eee2f`. Root replayed them without conflicts onto coworker canonical `82886bd75348eb58ca2522ac0cd82f3f9cbd529b`, producing `b2242bd57a`. All six changed files are byte-identical to both the original final source and the native-tested composition `234abddf8b1cd82f0517a268c78c291cafae5b59`. Cargo.lock and every unrelated canonical file remain unchanged. The native composition includes the three virtual Cargo workspace files already integrated in PR #75.

Root read the production changes and controls, checked the final import/source-order repairs, ran `git diff --check`, and independently checked the retained raw evidence with [root-audit.py](../operations/evidence/source-ranking-20261008/root-audit.py). [root-audit.json](../operations/evidence/source-ranking-20261008/root-audit.json) records the source correspondence and hashes of 108 evidence files. They are retained losslessly in `raw-evidence.tar.gz` (1,722,194 bytes instead of 34,258,292 bytes unpacked); `archive.json` binds its hash and size. Summary JSON files are also directly readable beside the archive.

## Actual native results

Four native macOS gates passed: source declaration ranking **4 passed**, existing declaration placement **1 passed**, semantic lane composition **1 passed**, and stable paging/input-change rejection **1 passed**. Total: **7 passed, 0 failed, 0 ignored**. Each gate used a fresh fleet admission and a managed remote build permit. The same Cargo-emitted test image was used, SHA256 `5e172dea3291f8af021fda90dd8ceb7630613ddc96fe8145db23ad4e6c1ec2e3`; the first gate compiled it and subsequent gates reused it. Root checked the Cargo event/receipt binding and raw log counts; Root did not separately rehash a local copy of the remote executable.

The native Python control compiles 34 actual source files, projects their compiler IR into product rows, and exercises the CLI/MCP search projection seam. It reproduces at least 25 inferred types named `get_app_settings` and verifies the actual function in `core/config.py:7` ranks first, retains its source/signature, and leaves inferred matches reachable through continuation. Additional typed controls cover TypeScript, Python, Go, Rust, unavailable source bytes, semantic candidates, and external targets. These typed controls are not native end-to-end compilation of each language.

Earlier attempts remain in the evidence: three capacity refusals before launch, a missing test-trait import compile failure with zero tests, and a three-pass/one-fail fixture whose unordered source frontier was rejected before native compilation. The final source-order repair changes ordering only; source bytes and assertions are preserved. [graph-return.json](../operations/evidence/source-ranking-20261008/graph-return.json) supersedes the original proof's pending-return note without rewriting it: the graph was returned to its prior clean owner with empty leases and no surviving owned compiler.

## Limits and remaining work

This is a source-ranking checkpoint, not an installed release or a real Mealie/HTTPie application acceptance claim. It does not establish embedding-provider availability, broad corpus coverage, instant cold search, or production readiness. The separate a9cf CLI/MCP composition has 183 primary native passes plus two nested passes, but excludes this ranking change; its evidence must not be relabelled as testing this source. Current TypeScript compiler authority fixes still have eight failing admission tests in their earlier coherent native run. Go/Rust public application runs and current matched release packaging remain separate gates.

Keep TypeScript, Python, Go and Rust in the same runtime matrix: first-home setup, add, asynchronous publication, exact and Tantivy search, document/source/reference reads, plain and paged graph traversal, dependencies, stdio/HTTP MCP, retry, cancellation, offline use and cold restart. Missing compiler/provider capability must stay an actionable typed refusal rather than an empty success.
