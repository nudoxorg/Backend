# Retained HTTPie native image

`context.nxfi` is the unmodified 24,375-byte NXFI payload extracted read-only
from the existing installed HTTPie owner object
`ddadc8fea429e25edf54c1b0133f661c8b3ee2e4e5b9a3fdcb4fbbbf8143408b.object`
(header offset 123). SHA-256:
`be7945026196ed937d003b1c4e19b1eb5fae2c94730eb0c4de79908bb8df81d9`.

`context.py` is the corresponding source from HTTPie CLI commit
`5b604c37c6c67e18e7c3e9aee6c88a8c22b98345` (https://github.com/httpie/cli).
The adjacent license is retained for these HTTPie fixture bytes.

The original installed CLI graph had ten generic external target rows.
Its raw receipt SHA-256 is
`523c4c28e2f6d02f715d3d1116620bfed523ea41afef76ce40af85985ab05658`.
The test retains those exact ten native keys, checks the original outgoing
Foreign target classification and exact source spans, and changes only
projected display text. The six source-backed incoming references from that
observation are a different graph direction.

This retained image is not a new compiler execution, a package completeness
claim, or independently established installed-binary-to-Git provenance.
