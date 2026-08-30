# Typed identity and locality salvage ledger

This ledger records mechanisms, not compatibility obligations. The losing implementations remain
inspectable in Git; a row permits deletion only after the named destination and falsifier exist.

| Independently valuable property | Previous owner or prototype | Disposition | Current owner or destination | Falsifier or evidence |
| --- | --- | --- | --- | --- |
| Raw identity rejection retains width source, expected authority, observed byte, and all raw bytes | `nudox-id` checked `ContentId`/`ArtifactId` decoders | retain | the same checked `TryFrom` ingress and typed error operands | public identity contract tests retain the complete nested `ContentIdDecodeError::Domain` assertion |
| Built-in domain and encoding codes cannot collide | separate marker/code declarations before `fcff03f5` | supersede with proof | one private registry declaration emits both `repr` code enum and marker bindings | production-macro trybuild mutations fail with E0081 for duplicate domain and encoding codes |
| Canonical locality bytes remain the sole portable local/remote representation | original locality artifact | retain | `ValidatedLocality::bytes` and the direct writer/public parser share the same grammar | writer witness is compared with a separately parsed witness; every mutated payload cell is rejected exactly |
| Sparse random lookup uses ordered exception rows plus rank directories rather than resident sidecars | original locality artifact view | retain | typed row/provider/descriptor lanes and `rank` | exact lookup-work tests retain one binary search and bounded rank popcounts |
| Canonical scans advance a forward sparse cursor with no random lookup per row | original `LocalityCursor` | absorb | named `CursorOrdinals` and closed `ExceptionRoute` carrying actual provider/basis/descriptor borrows | scan-work tests require one sparse comparison per exception and no per-row rank query |
| Provider and descriptor faults retain exact ordinal and source | deleted `artifact/trusted_decode.rs` and `LocalityReadError` | supersede with proof | `LocalityError` at the sole validation boundary | zero provider and unknown schema mutations assert ordinal and complete nested source before any witness exists; artifact-global domain mismatch retains expected and observed codes |
| A validated immutable witness never silently omits corrupt post-validation data | `trusted_decode` attempted to contain hypothetical byte drift after validation | reject as false capability | immutable borrow ownership plus `with_validated_locality` revalidates each potentially interior-mutable `AsRef` visit | safe callers cannot mutate borrowed bytes; HRTB visit cannot cache a witness across another borrow; domain-mismatch and writer/parser differential tests cover projection |
| Projection does not re-decode provider/schema/content cells or allocate sidecars | rejected 74-line Luna draft tried infallible accessors with fallback reconstruction | absorb | `ProviderWire`, compact locality descriptor records, `PlacementLanes`, and typed `ContentPayload` projection | source scan finds no post-validation `Result`, fallback, raw decoder, `Box`, or `Vec`; second-domain round trip preserves exact content identity |
| Direct writes reject short output before changing any byte | original `PreparedLocality::write` | retain | unchanged total preflight and exact `OutputTooSmall` operands | public canonical-writer test checks prefix/tail transactional behavior |
| A private encoder cannot mint a weaker witness than untrusted bytes receive | earlier `validate::from_writer` checked only Rust lane validity | supersede with proof | direct writer traverses the complete public grammar once after emission | returned writer witness and reparsed witness project identical entries for object and dependency-set domains |
| Scalar validation defines semantics while SIMD is only an acceleration | original scalar/SIMD row validator | retain | scalar oracle plus detected `fearless_simd` level | public differential tests require identical first error and identical long-lane projection/work |
| Workflow output mismatch retains the observed typed output | pre-repair workflow diagnostic | retain and strengthen | `UnexpectedOutput` includes observed bytes | exact workflow error equality test |
| Object-pack documentation distinguishes logical identity from raw physical keys | pre-repair object-pack docs | correct, do not delete | corrected public identity documentation | doc review and identity contract tests |

The rejected unsafe projection remains visible in the pre-review diff but is not retained. Its useful
insight—authority must be proved once, then projection must be total—moved into the wire grammar:
`HeaderWireRecord` stores one content-domain code, while each descriptor stores a typed 31-byte
`ContentPayload`. This removes one byte and one authority branch per present overlay while keeping the
entire implementation safe, allocation-free, and independently reviewable.
