# Captured Semver public version packet

`semantic-versions-semver-public.json` is the complete successful generic MCP `backend.surface` structured packet from the `cold-retained` route in the stopped configured Semver diagnostic. The second cold clone returned the identical packet. It retains the public envelope, budget, exact selected `SemanticVersionRecord`, all 42 published image-history proofs, freshness, and selected source frontier. It is public egress evidence, not a fabricated certificate-bearing `ReplyDto` or a claim of compiler shape completeness.

Provenance: evidence commit `94e21a934da54bee9b03d0b0c0d613101df679a5`, `docs/audits/semver-public-shape-budget-20261006/raw/two-cold-diagnostic.json`, SHA-256 `556c1b8efb2eebcc71b754e825b7bb1b118c9548d9c2592d87ec5aa2dac7dd90`. The matched runtime source is `fcbf561653f20dea2807d168524970159eea87eb`, tree `6cea1710913f58d93fa8a78cb1e81e90cf01e822`.

The fixture is compact UTF-8 JSON without a final newline, 32,798 bytes, SHA-256 `64f26ef2b94266e5ab92b7639f67573f8a6cf0138716e67404c20bae9fb04b21`. Its containing source list is 32,647 bytes, its single exact record is 32,645 bytes, and its history status is 31,131 bytes. The original named CLI/MCP route refused a 64,807-byte packet against the unchanged 49,152-byte ceiling.

The actual `SemanticVersionRecord` has no `query_proof` or `schema` field. The tests decode and serialize the complete captured source `SurfaceReply` without stripping fields or inventing authority. Existing certificate-bearing selected-row fixture gates and complete shape-export schema fixtures remain in place.
