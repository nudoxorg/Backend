# Experimental Stylo authored-source observer

This is the standalone proof for the observer subset also vendored by the
native frontend. It instruments pinned Servo
Stylo 0.23.0 (source revision `2dca34592ced37c6aa01733e94206a767194cf9d`)
and selectors 0.42.0. `prepare.py` verifies their registry archive SHA-256,
expands them locally, and applies the patches. It refuses to overwrite vendor
files. No Tree-sitter parser is involved.

```sh
python3 experiments/stylo-observer-proof/prepare.py
CARGO_BUILD_JOBS=2 cargo test --manifest-path experiments/stylo-observer-proof/Cargo.toml --locked --offline
```

The finite experiment buffers events for top-level native style rules, commits
ownership only after Stylo inserts the actual rule, and records accepted
pre-dedup declaration names/generated property IDs plus class tokens for
class-only selector lists. On a failed rule it drops buffered events. Unsupported
nested rules and selector lists are skipped and explicitly flagged; declaration
retained/overridden disposition remains unknown. A worker owns each buffer by
host token, guards it across unwinding and suppresses reentrant independent
parses. Output count/byte limits report overflow. The tests exercise duplicates,
failed selector rollback, nested media exclusion, :is exclusion, BOM plus
Unicode/emoji/CRLF, escaped class spelling, no-op/active output equality,
reentrancy, overflow, and panic cleanup.

These events do not yet cover nested rule paths, forgiving selector rollback,
complete stylesheet syntax, or a proven mapping to a retained declaration.
The experimental buffers do not provide a hard parser memory ceiling or
in-parser cancellation. The native frontend consumes this subset and binds the exact vendored source
trees in its support fingerprint. Durable admission and supervised source reads
remain separate. These paths must never be presented as a complete source inventory.

The exact patches have SHA-256: `stylo.patch`
`01a20e7d2e8a9fd67dc2f1d97602bfef41f5807895aff807f16d9571fc76f6ca`,
`selectors.patch` `474190b7d6b7e7c2883621ec63dd841f8fa23c53097fbf7ea6e0eb9b112e045d`.
Both packages are MPL-2.0. The checked archive checksums are in `prepare.py`.
