# Complete facets, authority scopes, and type-enforced laws

A versioned engine avoids work safely only when equality and change capture cover the complete observable truth. This ledger defines the semantic boundary of v2. It is grounded in the existing [semantic reader fields](/Users/mileswirht/Downloads/backend/compiler/ir/reader.rs:36), [core hash exclusions](/Users/mileswirht/Downloads/backend/compiler/ir/semantic.rs:2338), [language extension facts](/Users/mileswirht/Downloads/backend/compiler/ir/semantic.rs:1968), and [link/occurrence distinction](/Users/mileswirht/Downloads/backend/compiler/ir/semantic.rs:3139).

## 1. Every producer declares ownership and coverage

Each admitted authority result supplies:

```text
AuthorityScope = (authority/capability revision, source basis,
                  package/project/file/key-range identity, facet set)

Coverage = Complete(scope) | Partial(scope, known subset, reason)
         | Unavailable(scope, reason) | Unsupported(scope)

ScopeReplacement = (expected previous owned scope, new complete scope facts,
                    basis manifest, authority fence)
```

Captured-empty is a complete observation of an empty value/set, distinct from unavailable, unsupported, partial, and absent-by-deletion. The precise representation can use schema-specific tagged cells and shared defaults; it need not allocate an enum per row.

Only complete replacement coverage authorizes deletion of previously owned facts absent from the replacement, and only inside that authority's declared scope. A partial image never retracts another producer's rows or interprets an unavailable extension lane as empty. Scope identity includes source/config/authority basis; changing scope requires an explicit old/new scope transition and cleanup rule.

For facts with multiple producers, retain producer-qualified evidence/support. Derived canonical membership is the distinct projection of those supports. Removing one authority's observation cannot delete another authority's surviving evidence. Confidence aggregation is a derived rule over current supports, with deletion support; the historically strongest observation is not permanent truth.

## 2. Source and semantic facet ledger

The tables are the required field-family ledger. K0 expands every nested variant/field into the mechanical schema registry and rejects unassigned fields. This is especially important for the rich type grammar and foreign authority schemas: this document does not pretend that naming a `Type` relation enumerates all of their nested operands.

| Fact family | Logical key and complete value | Owner / replacement scope | Coverage and downstream dependency |
|---|---|---|---|
| Source file bytes | Workspace/path identity → exact bytes/chunk list, encoding and source version | Source/VCS authority; explicit file edit or complete file-set replacement | File absence is a witnessed fact; raw text readers depend on exact bytes |
| Directory/search-path membership | Directory/resolver scope → sorted members plus access/unavailable state | Discovery authority; complete enumerated scope | Add/remove can invalidate previously negative import/glob reads |
| Toolchain/configuration/environment | Capability or configuration key → immutable executable/SDK/options/features/relevant environment version | Configuration authority; explicit settings transition | Invalidates only recipes that read it, possibly an entire native session |
| Generated sources/build inputs | Generator/output logical key → content and generator/input manifest | Generator authority; complete named output set | Side effects isolated; outputs cannot be reused from an incomplete manifest |
| Declaration identity | Authority namespace + exact family/variant identity → identity correspondence facts | Native declaration authority; admitted declaration scope | A family is a range, not automatically a unique declaration; rekey is delete/insert unless correspondence is proved |
| Name and declaration kind | Declaration key → canonical name bytes, item kind and shape operands | Declaration authority | Name/outline/type recipes subscribe to relevant columns, not unrelated docs |
| Visibility | Declaration key → exact visibility plus capture state | Native authority; facet-covered declarations | Affects visible docs/search/export membership and policy-sensitive reuse |
| Parent/containment truth | Child key → logical parent or explicit root/unrepresented/unavailable state | Native authority; complete hierarchy scope | Do not collapse unknown parent to root; containment/query dependencies exact |
| Ordered members | Owner key → ordered member logical keys under declared order semantics | Native authority; complete member list | Outline/signature/member recipes; order changes matter even if membership equal |
| Semantic type | Declaration key → canonical type graph/component reference or explicit absent/unknown state | Native semantic authority | Type/signature consumers; retains Concrete/Computed/Unknown distinction |
| Rich type operands | Type/component key → tag, ordered roles/operands, names/literals, qualifiers, bounds and state | Semantic canonicalizer of admitted authority graph | Complete schema grammar; symbolic internal cyclic references, exact external input facets |
| Recursive product data | Product/component key → constructor/arity/ordered local and external operands under its quotient | Product canonicalizer | Keep distinct from rich TypeExpr grammar/bisimulation rules; share generic graph/layout kernels only |
| Documentation | Declaration key → ordered text/code/link/break fragments and availability | Native docs authority; complete doc facet | Rendering/text index/embedding input recipes; empty document differs from unavailable |
| Doc links | Fragment key → label plus exact local/stable/foreign target semantics | Docs authority | Resolution depends on target facts only if actually resolved, avoiding recursive hash cascades |
| Attributes | Declaration key → canonical ordered/set-valued attribute atoms as specified by source schema | Native authority | Rendering/signature/filters; schema declares whether order is observable |
| Declaration source span | Declaration key → source-file logical/version binding, half-open byte span or absence plus authority state | Source evidence authority | Source navigation/snippets; edits shifting offsets are real changes |
| Image authority/provenance | Semantic plane/scope → profile, source basis, producer/recipe and image provenance | Compiler admission | Whole-plane trust/coverage dependency; copied once in manifest rather than each hot row |
| Entity authority facts | Declaration key → captured fact availability, stable identity/parentage/source truth | Native admission | Controls which reader capability and projections are legal |
| Canonical graph relation | `(source key, kind, target descriptor)` → relation presence; evidence derived separately | Projection of producer-supported facts | Both graph directions and query joins; target foreign variant availability stays explicit |
| Graph observation | `(producer scope, logical relation, observation identity)` → site source/confidence/capture | Native evidence authority; complete observation scope | **Bag multiplicity is preserved**, even equal source spans can be separate observations |
| Compatibility edge evidence | Relation key → deterministic selected strongest representative from surviving observations | Semantic derived recipe | Recomputed incrementally with support on deletion; never substitutes for all occurrences |
| Foreign/external target | Typed foreign namespace/identity → exact target and known/unknown variant/origin | Native/resolver authority | No accidental use of a local ID convention for foreign entities |
| Atoms/text/list values | Canonical value ID → exact bytes/UTF-8 promise/ordered typed contents | Semantic canonical encoding | Physical pool IDs/dictionaries may change; logical value equality cannot depend on pool order |
| Legacy core payload | Legacy declaration key → current documented partial hash and coverage | Compatibility export | May accelerate only covered facets; never complete ObjectVersion or general cache key |

Existing `EntityVersion.family`, `variant`, and `core_payload` become identity/compatibility facts under these rules. Every `SemanticEntity` field—name, kind, visibility, parent, semantic type, members, docs, attributes, source, authority and version—has an owner above. Its current `id` is a local coordinate; it is replaced at persistent boundaries by the declared logical identity, not hashed as a new globally stable integer.

Occurrence identity requires care. If the authority supplies a stable observation key, retain it. Otherwise use a deterministic scoped representation with multiplicity and compare the complete scoped multiset once. Do not fabricate stable occurrence identity from only `(edge, span, confidence)` and lose repeated equal observations. A normalized count relation is possible when consumers do not observe individual observation identity; its bag semantics must remain explicit through export.

## 3. Language extension ledger

These are sparse typed relation families under the language authority, not an erased maximum-width union in every row. Shared mechanics handle availability, versioning, delta capture and column encoding. Each language retains interpretation and validation.

| Language | Complete extension fields already represented in source | Dependencies and authority scope |
|---|---|---|
| TypeScript | Type parameters, declared type, observed type | Checker plus syntax/source binding; observed type may be concrete/computed/unknown and must not be forced into a false computed marker |
| C# | Nullability, reference kind, constraints, async/iterator/extension effects, attributes, partial role, XML provenance | Roslyn/project/profile/source basis; XML evidence and partial declaration roles independently observable |
| Go | Signature parameters/results/variadic, type parameters, fields, method set, build constraints, constant value/group/flags | Package/module/build configuration; method sets and constants may depend beyond edited file |
| Rust | Ownership, lifetimes, where clauses, macros | Crate/features/toolchain/expansion context; macro-generated dependencies and HIR lifetimes retained honestly |
| Python | Decorators, parameter kind, dynamic confidence | Syntax plus peer checker/import environment; unknown/dynamic confidence is not inferred exactness |
| Java | Throws, annotations, overloads, record components | JDK/classpath/source/doclet basis; UTF-16 source positions translated under explicit byte-span contract |
| Clang | Const/volatile/restrict qualifiers, storage class, optional layout size/alignment, templates, includes | Translation unit/compile commands/includes/target ABI; unavailable layout is not zero-sized layout |

The type-parameter, list, source-span and atom references in this table resolve under the pinned semantic view and full canonical values. Replacing generation-local IDs by logical references is a schema migration, not a raw cast. Nested operands inherit the same complete schema and coverage requirements.

## 4. Derived and control state ledger

| Relation/object | Version key and input basis | Update/retention law |
|---|---|---|
| Name/exact index | Name normalization recipe + relevant entity/name/visibility roots | Shared arrangement; update changed names/membership only |
| Lexical postings | Tokenization/field/position recipe + indexed text versions | Retract old token support and add new; global score statistics explicitly separate |
| Graph adjacency/reachability | Edge/containment relation roots + query/closure recipe | Shared forward/reverse arrangement; finite set fixed point and correct deletion/rederivation |
| Document fragment | Render recipe + signature/docs/member/selected policy read manifest | Reuse unchanged fragments; output identity stops downstream work |
| Embedding/vector facts | Exact normalized input/context + immutable model/tokenizer/metric recipe | Reuse per input; remote receipt/coverage policy; ANN is derived approximate layout |
| Library package selection | Stable product key → selected root/basis/status and durable intent | Atomic user/product commit and fenced effect selection |
| User settings/pins | Stable intent key → typed value with chosen domain merge metadata | Durable, optionally replicated; reconstruct ephemeral demand after restart |
| ViewRow | Query/row logical key → projection version and stable order key | Rank independent from identity; bounded window deltas; precise source/coverage |
| Demand | Session/consumer/recipe/range → freshness/priority/lease | Ephemeral control relation; never durable workspace history by default |
| Work attempt | WorkKey + owner epoch/ordinal → state/resource/receipt | Bounded control/recoverable job state; attempt identity does not alter pure result identity |
| Pack/location/checkpoint | Logical root/object → validated physical layout manifest | Layout-only changes preserve logical roots; reader pins govern retirement |

Use stable order-tree/fractional ordering keys for view movement where the recipe permits it; expose dense positions only within the requested window. Inserting one top row must not mechanically rewrite every following row's canonical identity. Fractional keys may need occasional physical/order-label rebalance; if order itself is unchanged, such labels should stay layout metadata, with logical ordering determined by query sort keys and stable tie-breakers.

## 5. Native session reuse and invalidation granularity

| Authority | Reuse worth implementing | Correct invalidation/reset boundary |
|---|---|---|
| Rust | Share discovery/analysis and derived compile/IR outputs within the admitted same request; retain compatible compiler session only where API permits | Source/dependencies/features/build scripts/proc macros/toolchain; thread-bound compiler data cannot be freely sent across workers |
| Clang | Reuse compatible translation unit/precompiled header/module state | Compile command, target ABI, include search membership, macros, unsaved files and libclang capability; cross-TU scheduling follows authority safety |
| TypeScript | Persistent project/checker program and incremental source update; shared syntax/checker projection | Project/options/module-resolution/source version and checker schema; preserve declaration/overload/narrowing passes |
| Python | Warm checker/import graph and syntax scope extraction | Interpreter/checker/environment/import path and negative discoveries; syntax alone never claims checker-complete semantics |
| Go | Warm helper/package graph and reusable tool build outputs | Module/workspace/build tags/GOOS/GOARCH/toolchain/imports and oracle schema; preserve package-wide type/method-set effects |
| Java | Content-addressed doclet build and reusable JDK/classpath setup; isolated extraction runs | Doclet source + JDK/release/options/schema, source set/classpath; do not key only by embedded doclet text |
| C# | Persistent Roslyn workspace/compilation where admitted helper supports it | Project/reference/options/SDK/source/XML inputs and oracle protocol; maintain source-bound image validation |

This table chooses where to seek deep reuse; it does not assert those APIs are currently exposed or every authority is thread-safe. The [product and frontend audit](../research/structure-product.md) records current implementations and package constraints. Persistent sessions run under bounded lifetimes/RSS and invalidate by complete manifests. Unsafe or unavailable incremental APIs use existing subprocess/scope replacement. Cross-language parallelism is independent of a native library's internal thread restrictions.

Share process supervision, bounded pipe/file output, deadlines, owned temporary paths, descendant termination and typed cleanup errors. Preserve each authority's distinct wire schema and projection semantics. A timed-out or killed authority has incomplete coverage, not an empty package. Session restart and cold subprocess must match full semantic/evidence output under the same input manifest before enabling persistent reuse by default.

## 6. Rust types enforce transitions; algorithms enforce truth

The strongest type design is a short chain of sealed capabilities around the real dataflow:

```text
Bytes --validate--> Admitted<Schema, ImmutableOwner>
   --pin/read--> Snapshot<'root> + LocalRow<'dictionary, Kind>
   --prepare--> PreparedWorkspaceDelta<'expected_base>
   --durable compare/select--> CommitReceipt

AdmittedInputs + Recipe --execute--> PreparedResult
   --validate authority/basis--> AcceptedResult
   --select coherent coverage--> ViewSnapshot<'view>
```

Use private constructors and invariant fresh lifetime brands for dense local IDs. A higher-ranked closure creates a new brand for the actual owner, preventing integer handles from unrelated dictionaries/scopes from mixing. [Rust's variance rules](https://doc.rust-lang.org/nomicon/subtyping.html) explain why the brand must be invariant: a merely covariant lifetime can be shortened into an unintended common scope. Bounds checks and immutable backing proofs still matter; a brand alone does not make an arbitrary index valid.

Use GAT lending cursors for borrowed batches, typed column family markers for finite specialized kernels, and separate `Complete<FacetSet>` capabilities from partial views. Runtime coverage cannot be made complete by deserializing a phantom marker. Admission checks coverage and mints the sealed capability. If runtime schemas make a fully static facet set cumbersome, use a checked immutable coverage witness with typed accessors, not unsafe generic casts.

Keep shared immutable owners at segment granularity and output/scratch leases affine through moves. Implement initialized-length/drop logic once in the storage/batch kernel; require unwind safety where relevant and process-crash recovery separately. Use `NonZero`/niche encodings or compact tagged lanes when their domain law is real. Do not rely on Rust enum padding for durable or network representation.

The first design's [compiled Rust lifetime probes](/Users/mileswirht/Documents/ChatGPT/backend/redesign/EVIDENCE.md) remain useful evidence for the narrow generative/GAT pattern. They are not a proof of the entire v2 runtime. New unsafe kernels need Miri/fuzz/concurrency validation at their actual boundary. Advanced Rust is used to remove invalid states and repeated checks, not to encode a dynamic million-node graph into enormous generic types.

## 7. Incremental correctness matrix

| Change | Must change | Must remain reusable when its own inputs stay equal |
|---|---|---|
| Docs only | Docs facets, affected text/embedding/render recipes, actual source evidence changes | Type/header/edge membership, unrelated docs and view fragments |
| Visibility only | Visibility-dependent query/document membership and policy manifests | Raw unchanged text/type payload objects |
| Extension-only field | That language facet and consumers that read it | Unrelated language planes and generic columns |
| Same edge, one occurrence removed | Occurrence multiplicity/support and possibly representative confidence/source | Edge membership if other supports survive; unaffected graph ranges |
| Body edit with unchanged exported facts | Source/basis/provenance, any body-sensitive recipes | Equal exported semantic outputs stop downstream propagation |
| Add previously absent import/config file | Membership/negative dependency witnesses and affected authority results | Truly independent package/recipe manifests |
| Partial authority output | Explicit partial/unavailable coverage and known facts under merge policy | Facts outside complete replacement scope; no inferred delete |
| Graph cycle deletion | Exact affected reachability/SCC rederivation or incomplete state | Unaffected closed components proved independent |
| Tool/model/recipe change | Exact dependent work and outputs where changed | Raw source/content objects and independent recipes |
| Physical repack | Pack/location/layout IDs | ObjectVersion, StateRoot, WorkspaceRoot and user history |
| Remote authority compromise | Trust policy and selected provenance-dependent coverage | Independently verified/trusted outputs under explicit policy |

For recursion, the classic [DRed algorithm](https://sigmodrecord.org/1993/06/03/maintaining-views-incrementally/) deletes a safe superset and rederives surviving facts. Use a correctly scoped implementation or an exact full-scope fallback. A negative edge plus positive support counts alone is not a valid deletion algorithm for arbitrary recursive graphs.

## 8. Logs, cursors, and consistency closure

Share envelope/codec/replay mechanics across authoritative commits, derived view checkpoints and layout manifests. Do **not** globally serialize every operator update and hover event into the authoritative workspace journal. Authority class determines durability, writer ownership, replication and retention. Pure in-flight attempts and ephemeral demand remain bounded control state unless a recoverable job contract requires persistence.

A subscription cursor binds log/branch identity, schema, observed root and next sequence/chain position. `DeltaId` detects a duplicate transition but is not by itself a gap detector. A gap, discarded branch, incompatible schema or pruned base returns a typed reset to a pinned complete view root. Reopen/replay never continues a scalar cursor against an unrelated workspace history.

`Upper` records updates that can still arrive; `Since` records the oldest temporal distinctions consumers still require. Physical merge may proceed without moving Since. Closed-frontier publication is the point at which a view may claim completeness. Historical root retention and hot execution trace retention are separate budgets. Source basis, scope coverage, accepted authority, root closure and completed frontier must all agree before a result is called current and complete.
