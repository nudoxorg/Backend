# Portable binary certificate source prototype

This source prototype is based on `17f3b1f2dbee86cacb3392486ce0f8dcbd32c0c3`. Production still issues and consumes the existing `pc3` transport. No native build, owner process, replay, authenticated successor token, or context-size guarantee is claimed here.

The actual default Go graph response is `/private/tmp/nudox-root-go-current-03/0066-mcp-wire.json`, SHA-256 `d55dfef0dd30ace553a18ab73cb21101725643bd24ee180650a07521d62ed4bf` without the newline. It contains 25 records and a 12,304-character signed cursor. Its owner payload expands to the exact 77,842-byte command retained as `crates/library/wire/command/fixtures/current-go-graph-continuation.json`, SHA-256 `9cdd49cbf9c2018d2b9eeac0c6dfc87e4cba0dceb53917ac409a90a90e3f916c`.

The preimage is a decimal JSON byte array, not a hex encoding. Its canonical root contains 21,879 raw bytes; the array occupies 72,613 JSON bytes. Putting base64 inside JSON made compression worse. BQPC/1 carries the exact raw bytes instead.

## Layout

The six-byte magic/version is `42 51 50 43 00 01`. It is followed by a big-endian u32 header length, a closed canonical JSON header containing only `version`, `request_id`, and `command`, and a one-byte certificate presence tag (0 or 1). A present certificate has a big-endian u32 claim count and the ordered claim sequence.

Claim tags 1–11 explicitly identify Key, KeyBytes, KeyCommitment, RowIdentity, Version, Root, RootCommitment, Intent, Delta, Cursor, and Coverage. Schema tags 1–14 explicitly identify the current WireSchema variants; neither mapping depends on Rust enum discriminant order. Digest fields are exactly 32 untrusted bytes, expanded back to the original canonical lowercase 64-hex spelling. Text and byte fields have a big-endian u32 length; text must remain valid UTF-8. Cursor schema/sequence use big-endian u16/u64. There are no alignment bytes, optional implicit fields, deduplication, or normalized authority claims.

Both binary input and expanded command JSON retain the existing 256 KiB command cap. Lengths are checked against remaining input before borrowing, the claim count cannot exceed the number of physically possible minimum-sized claims, and unknown versions, tags, schema tags, invalid UTF-8, noncanonical header JSON, truncation, and trailing bytes refuse. The borrowed decoder never constructs an owned certificate or a generic JSON Value; it streams exact claims into the existing JSON command grammar only after validating the complete frame. Expanded bytes are counted before allocating their output.

Packing and expansion supply no authority. A future client adapter must keep the current command encoder's admission checks, then compress this frame under a distinct `pc4` prefix. Expansion must run before the unchanged standalone/owner-aware command decoders and owner revision/scope/recipe/predecessor comparisons. Existing pc2/pc3 decoding remains strict. MCP's current `mcp2` MAC must continue to cover the exact version/expiry/owner bytes and request context. No owner cursor map, persistent export cache, omitted claim, shortened ID, or new admission path is needed.

## Source measurements

The independent Python oracle restores the original command bytes exactly and produces the golden uncompressed BQPC/1 fixture, SHA-256 `524dd04c6aef532b0f9b78f495d998a8d8a706d87a89ad84318ba299388d3847`. Original claim order is Coverage, RootCommitment, three Keys, KeyBytes, Version, Root.

| Observation | Existing pc3 | Binary prototype |
|---|---:|---:|
| Uncompressed command | 77,842 bytes | 24,027 bytes |
| Python zlib compressed body | 9,188 bytes | 6,334 bytes |
| Owner token, pinned cl100k_base | 8,694 tokens | 5,987 tokens |

Combining the reviewed typed Summary projection with this binary owner payload models 22,744 bytes and 10,451 cl100k_base tokens for all 25 records. This is field-exact source accounting, not a newly signed or admitted reply: the retained MAC is not valid for the changed payload. Rust's compression output and actual complete replies still need native measurement.

The pinned tokenizer is tiktoken 0.11.0, cl100k_base, artifact SHA-256 `8613f818e5af318d868379772ce5234e3a3e8519b6c611d94cf708fdfc580fbc`. The 12,000-token target is a measured fixture target for that tokenizer, not a guarantee for every model or the repository's four-byte estimate.

Run the source oracle with the already installed pinned Python runtime:

```
/Users/mileswirht/.cache/codex-runtimes/codex-primary-runtime/dependencies/python/bin/python3 docs/operations/binary-certificate-source-oracle-20261011.py
```

Rust regressions are prepared for all claim families and schemas, duplicate preservation, exact optional-certificate spelling, strict header/framing/UTF-8 failures, independent binary/expanded caps, actual Go proof equivalence, and corrupt-preimage refusal through existing admission. They have not been run. Client cold-restart and MCP MAC/context laws require the separately reviewed adapter and native guard before acceptance.
