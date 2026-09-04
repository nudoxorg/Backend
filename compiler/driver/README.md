# compiler-driver

`compile_semantic` is the public coherent-result entrypoint. It enters the
selected authority and lowers it once, builds the owned semantic `Ir` and its
`RichIrCapture`, then writes and validates the compact `CompiledFragment`.
Its `CompiledSemantic` result keeps source and recipe facts on the artifact;
the owned image and capture share the same admitted fact transaction rather
than duplicating result metadata.

The result borrows only the caller's fragment-output buffer. Source bytes,
authority input, diagnostic scratch, native-work lease, cancellation control,
and deadline do not escape. Cancellation, deadline, authority, build, write,
or validation failure returns an exact `CompileFailure` and no semantic result.

`compile` and `compile_ir` remain compatibility projections. Each is narrow:
calling them separately performs separate authority transactions and does not
prove compact/owned cross-call coherence. Use `compile_semantic` when a caller
needs one trusted fused artifact, image, and capture result.

The private native-process sidecar remains distinct from semantic authority.
Every currently valid `LowerIr` profile enters a real direct language authority
(or a caller-supplied, source-bound authority image), so the fused transaction
does not add a redundant subprocess syntax pass. Native process terminals and
their caller-owned scratch lease remain isolated for any future route that
actually requires them.

Declaration stable identity is scoped by the entered declaration scope,
language profile, and explicit parentage state (`Root`, bound parent stable
identity, unrepresented authority owner, or unavailable). A structural
signature is used only for same-scope/kind/name collision siblings; source
order, spans, and ordinary type/member edits are never stable-key inputs.
Payload hashing currently covers the declaration's direct semantic basis,
ordered product/type shapes, and the order-independent set of locally Bound
member bases. It does not claim authority-complete membership when the
member-set capture marker is unavailable.
Documentation, visibility, extensions, source spans, opaque parentage, and
occurrences are not payload-hashed yet and therefore have no parity claim.
