# Card C6 — reader trusted-projection law (registered role: nudox_luna_implementer)

## Baseline & owned paths
Baseline commit: a49faa4ff9 (HEAD). You own EXACTLY:
- compiler/languages/csharp/image.rs
- compiler/languages/csharp/lib.rs (only the re-export line if an item renames)
- compiler/driver/lower/csharp.rs (only the consumer updates the API change forces)
- the crate's tests if an assertion must follow an error-carrying item type
No other path may be written.

## The law being violated (deliver-reviewed-rust-slice, durable/binary data)
"Validate once into a compact borrowed witness whose closed tags and
coordinates are typed. Trusted projection may not repeat raw-to-closed
conversion or use `unreachable!`, `expect`, fallback values, silent
omission, or an unchecked narrowing cast to recover facts validation
supposedly proved. If projection remains fallible, validation has not
produced the right representation."

## The evidence (pinned at baseline; verify before you start)
In compiler/languages/csharp/image.rs the open() pass validates EVERY row of
the parameters/type-parameters/types sections through the same closed
decodes — and then the trusted projection REPEATS the raw->closed decode
with fallback values:
- `Parameter::decode`: `RefKind::decode(row[8]).unwrap_or(RefKind::Value)`
- `GenericParameter::decode`: `VarianceTag::decode(row[10]).unwrap_or(VarianceTag::Invariant)`
- `CSharpImage::type_node`: `TypeNodeKind::decode(row[0]).unwrap_or(TypeNodeKind::Error)`
  and `NullabilityCell::decode(row[1]).unwrap_or(NullabilityCell::None)`

Both clauses fail: the conversion is repeated after validation proved it,
and an impossible arm would fabricate a semantic value (Value / Invariant /
None / Error) instead of being inexpressible.

## Repair contract
Make the projection share the validation's proof instead of re-decoding
with fallbacks. Pick the smallest shape that satisfies the law; the
manager's expected shape (you may choose a better one within the law):
1. `type_node` already returns `Result<TypeNode, ImageError>` — replace the
   two fallback decodes with typed rejections (the existing
   `ImageError::DeclarationKind { index, found, plane }` operands), so an
   impossible arm is an exact error, never a fabricated node. The
   validation pass must use the SAME decode so a validation/projection
   divergence is unrepresentable (one decode function, two callers, no
   second vocabulary).
2. `Parameter::decode` / `GenericParameter::decode` are called from
   `ParamIter` / `GenericIter` whose `Item` is currently infallible. Give
   the two iterators `Item = Result<Parameter, ImageError>` /
   `Result<GenericParameter, ImageError>` (the crate's DeclarationIter and
   AttributeIter/DocIter/ReferenceIter already yield Result — this matches
   the crate's own precedent), decode through the shared closed decode, and
   map an impossible arm to the exact typed rejection. Update the CONSUMER
   (compiler/driver/lower/csharp.rs iterates `declared.parameters.iter()` /
   `declared.type_parameters.iter()`) to handle the Result items without
   lossy defaults — a decode failure there is a ProjectionFault::Image with
   the exact error.
3. Remove the now-dead `unwrap_or` imports/arms. No behavior change on
   valid images; hostile images that previously projected fabricated
   defaults now stop at the exact typed rejection — but note that open()
   already rejects every such image, so the new arms are statically
   unreachable from `open`-approved images; add ONE test that proves the
   total decode path: for every out-of-vocabulary byte in each of the three
   sections, open() rejects with the exact variant BEFORE any projection
   runs (extend the mutation table in tests/image.rs ONLY if a class is
   missing — parameter refKind, generic variance, type kind, nullability
   mutations with recomputed digests).

## Gates
export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
export PATH=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/node-v22.14.0-darwin-arm64/bin:/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk:$PATH
export DOTNET_ROOT=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/dotnet-sdk
export NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules
export COMPILER_CSHARP_COMPILER=$DOTNET_ROOT/dotnet
- cargo test -p compiler-languages-csharp --offline — all green (image 5+,
  producer_parity, protocol 8)
- cargo test -p compiler-driver --offline --lib lower::csharp — green
- cargo test -p compiler-driver --offline --test csharp_corpus --test
  csharp_image --test csharp_render --test csharp_packaging — green
- grep receipt: no `unwrap_or` on any decode path in image.rs; no new
  unwrap/expect.

## Bounds
No wire change, no new dependencies, no unsafe. The reader's public API
change is limited to the two iterator Item types (the crate has exactly one
external consumer: compiler-driver).

## Checkpoint & return
One commit, prefix fix(csharp-reader):. Return: commit hash; the exact
pre/post signatures of the three decode paths; the consumer diff summary;
gate tails; the mutation-table additions; smallest remaining red row.
