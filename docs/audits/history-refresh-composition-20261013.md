# History paging and owner refresh: composed source checkpoint

This branch composes the reviewed history and embedding-refresh atoms over canonical `0a7a2cf4c5b8c576e1aabd4077f53a0052df2b0a`. It is a source checkpoint, not a release or native acceptance result.

## Preserved boundaries

* DTO 29 carries both new surface families. Persisted versions 21 through 28 retain their original admission behavior. No previously released DTO-29 peer is assumed compatible.
* Semantic history pages retain full lineage, parents, metadata and exact source/catalog identity. Continuations preserve the original selection through preparation and cold replay; a changed basis refuses rather than silently reselecting.
* The admitted immutable catalog is reused behind exact copy-on-write inventory guards. The optional derived cache accounts requested owned allocations up to 32 MiB and uses weak guards to avoid pinning obsolete inventories. Over-budget requests keep full uncached proof/page behavior. This does not bound total process RSS or transient admission memory; uncached repeated pages can cost `O(P × N)`.
* Refresh captures and validates the installed compiler/query recipe before moving either owner. Pre-handoff refusal restores the prior state. Accepted worker retirement owns the request reservation independently of the caller's lifetime; dropping a caller cannot reopen mutation admission while that transition is queued or running.
* Start and Cancel are mutations. Observe is an exact-ticket read. Only Observe receives the single safe MCP reconnect and ordinary read socket timeout; existing index acknowledgment exceptions remain ahead of dynamic surface classification. A reconnect never renews the original request deadline or repeats an ambiguous mutation.
* Copied recovery actions retain all captured paths, the actual authority file and the distinction between explicit selections and HOME/XDG defaults. Unsupported path representations receive generic guidance. Passive observation cannot start an owner or broaden Start/Cancel admission.
* Canonical Projects/Tree local-read routing and the separately merged census corrections remain intact. No unrelated graph/catalog preparation was restored during composition.

## Review and validation

Root read the complete producer, service, transport, presentation and CLI/MCP changes, including the final affine-reservation and exact-replay successors. Independent Luna reviews cleared the original slices and the combined boundary review. That combined review found two additional Observe defects: MCP classified every refresh action as nonrepeatable, and client socket timeout selection used the static family mutation. Root corrected both in `d736c968cf`, with two reconnect laws and an extension of the existing actual Unix-stream timeout law. Luna independently cleared that final two-file change.

All 65 changed Rust files parsed successfully under installed rustfmt 1.11/nightly 1.99 with edition 2024. The receipt is `/private/tmp/nudox-root-composed-rust-parse-20261013.json`; source hashes and tool identity are retained. This is syntax parsing only. **Rust typechecking, native tests, Clippy, exact cold-history page union, original publication laws, model activation and installed-product acceptance remain UNRUN for this composed source.** Earlier binaries and fixture captures are not credited to it.

The separately frozen cc79 replacement remains unchanged. Its native source handoff/build must finish before this branch receives its own native cohort; successful cc79 tests cannot prove these later atoms. The active installed public preview remains historical. No installer, Homebrew artifact, macOS bundle or release was updated by this composition.

## Native GUI evidence remains separate

Historical normal GUI attempt 04 produced an accepted focused empty Library capture; its full independent audit is in `normal-gui04-root-audit-20261013.json`. Attempt 05 then genuinely opened Add with Ctrl+O. Root inspected its actual native screenshot: the Path field was focused, Cancel enabled and Add disabled for empty input. All six strict static frame captures correctly refused presentation crossings from the blinking caret. That managed flow remains failed and did not admit or publish a project.

The reviewed next driver keeps the original exact static gate, then records an explicitly nonexact dynamic interval only when a genuine accessible frame was Presented inside inward uncertainty bounds. It retains strict rejections and distinguishes both proof classes. No forced redraw, disabled caret, fabricated notification or relaxed identity checks are used. The next actual flow must enter the real HTTPie path, retain the mounted row through indexing, inspect resize/focus and query transitions, and prove publication independently. Preparing that harness does not establish those outcomes or animation quality.
