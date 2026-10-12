This data and count-only Rust adaptation use OpenAI tiktoken 0.11.0 (MIT,
LICENSE in this directory). Upstream sources are `src/lib.rs` and
`tiktoken_ext/openai_public.py` at https://github.com/openai/tiktoken/tree/0.11.0.

`cl100k.ranks` retains all 100,256 original raw mergeable tokens, in rank order.
It does not contain user queries, owner state, or cache entries. The header is
`CL100K`, bytes 1 and 0, then a little-endian u32 count. Each following token is
a little-endian u16 length followed by raw bytes. Rank is its sequence index.
`provenance.json` pins the asset SHA-256, exact regex, reference fingerprint,
encoding policy and maximum token length. `reference.json` contains 266
`encode_ordinary` oracle cases produced by the pinned upstream Python wheel.

The production counter preserves upstream regex partitioning, direct-token
lookup, minimum-rank merge choice and leftmost ties. It retains count only.
A local heap of adjacent pairs replaces upstream's repeated vector minimum
scan: stale generations/adjacencies cannot merge, and candidates are bounded
by at most the initial pairs plus two per successful merge. Hash lookups span
at most 128 bytes, the retained vocabulary's maximum token size. Scratch is
O(input bytes), merge work O(input bytes log input bytes), and input is bounded
by the shared 256 KiB hard projection limit. The regex has a finite one-million
backtracking budget and must partition every UTF-8 input byte. Failures refuse
admission. The shared immutable vocabulary/regex are initialized once; there
is no per-request vocabulary rebuilding or token-result cache.

MCP admission names `cl100k_base/ordinary-v1`, 12,000 tokens. This is not a
universal model token guarantee. Special-looking strings are ordinary user
text. Admission counts serialized, escaped complete JSON-RPC JSON, including
both text and structured content, request ID and metadata; newline/HTTP
framing bytes are separately included in the existing byte budget. Existing
`estimatedTokens` and `bytesPerToken` remain an explicitly non-tokenizer
estimate. Token policy, vocabulary hash, scope and limit are additive wire
metadata. The final frame is measured again after metadata and preview fitting.

No Rust build or native run was performed while preparing this source atom.
The broker must run reference/merge laws and whole MCP/presentation regressions.
