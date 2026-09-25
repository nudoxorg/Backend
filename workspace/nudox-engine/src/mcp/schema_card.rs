//! The compact reference card `graph_schema` serves by default.
//!
//! `schema.graphql` (see [`crate::mcp::SCHEMA_SDL`]) is byte-identical to
//! what the graph plane parses — LR-7 forbids touching it or the constant
//! that mirrors it. But 81% of that file by byte count is the `Symbol`
//! interface's 13-scalar/13-edge doc block, copy-pasted onto all 14
//! implementors because GraphQL SDL cannot say "as declared on the
//! interface". Verbatim it is ~15,129 cl100k_base tokens (measured) and
//! contains zero worked query examples for a directive-driven Trustfall
//! dialect most models have never seen — every agent that wants to write
//! one graph query pays the full cost.
//!
//! This card is a hand-maintained, DERIVED summary of the same file: every
//! type, scalar and edge stated once instead of fourteen times, the
//! semantic rules that make a query silently wrong if missed, and worked
//! examples that `tests/mcp/schema_card_queries.rs` extracts from
//! [`SCHEMA_CARD`] itself and parses against the live schema on every test
//! run, so a drifted or broken example fails loudly rather than shipping.
//!
//! It is not generated and it is not *the* schema — when it and
//! `schema.graphql` disagree, `schema.graphql` wins. `graph_schema`'s
//! `full: true` argument (or the `nudox://schema` resource) always returns
//! the genuine, verbatim SDL.

/// The compact `graph_schema` default response.
///
/// ~1,150 cl100k_base tokens (measured) against the full SDL's ~15,129 — a
/// 92% reduction. Covers every queryable type, scalar and edge in
/// `schema.graphql` exactly once, the semantic rules a query gets silently
/// wrong without, and worked Trustfall queries that parse against the real
/// schema. Complete for writing a query; ask for `full: true` only for what
/// this card deliberately omits (exact doc-comment wording, directive
/// grammar edge cases).
pub const SCHEMA_CARD: &str = r#"# graph_query reference card

Complete queryable surface, stated once each. Full `schema.graphql` SDL:
call `schema` again with `full: true`, or read `nudox://schema`.

Root (`type RootSchemaQuery`): `Packages: [Package!]!`, `Symbols: [Symbol!]!`.

`Package`: `lineage`, `name`, `ecosystem` (String!), `members: [Symbol!]`.

## `Symbol` interface — declared ONCE, inherited by all 14 concrete kinds

Scalars (13): key, keyTier, keyIsContentDerived, name, signature, path, kind,
isPublic, documentation, isDeprecated, visibility, deprecationNote,
deprecationSince.

Edges (13): package, members, parent, usages, mentions, implementedBy,
subtypes, returnedBy, acceptedBy, heldBy, signatureTypes, occurrencesOf,
location.

Concrete types add only: Function +isAsync +receiverKind. Trait
+supertraits(keys) +implementors. Impl +ofTrait(key) +selfTypeStr.
Field/Const/Static/Param +typeStr. Alias +targetStr.
Record/Enum/Variant/Module/Reexport/OtherSymbol: nothing extra.

`*Str` fields (typeStr, targetStr, selfTypeStr) are RENDERED TEXT for
display — never pass to a key lookup. `ofTrait` and `supertraits` lack the
`Str` suffix and ARE `ecosystem:name#introhex` keys, despite the resemblance.

`Occurrence`: targetKey, referenceKind, confidence, spanStart, spanEnd
(Int!, RELATIVE to the owner's declaration start, not file-absolute — add
`owner.location.byteStart` for an absolute offset), owner: Symbol! (never
null), target: Symbol (nullable).

`SourceLocation` is one object with 3 variants keyed by `kind`. `location`
is NEVER null — read `kind` first: Declared = file, byteStart, byteEnd,
startLine, startColumn, endLine, endColumn all set. BytesOnly = file,
byteStart, byteEnd set, no line/column. Unlocated = only unlocatedReason
set: Synthesized | MacroExpanded | ProducerRecordsNoLocation |
OutsideDocumentedPackage.

Directives (all `on FIELD` except `@filter`, which also allows
INLINE_FRAGMENT): `@filter(op:String!, value:[String!])` repeatable ·
`@tag(name:String)` · `@output(name:String)` · `@optional` ·
`@recurse(depth:Int!)` · `@fold` · `@transform(op:String!)`.

## Semantics a query gets silently wrong without these

- `mentions` is REVERSE — who points at me (trait an impl implements,
  supertrait bound, base class/interface). `signatureTypes` is FORWARD:
  every symbol named in THIS declaration's own signature.
- HEAD vs WHOLE-EXPRESSION: `mentions`, `implementedBy`, `subtypes`,
  `Trait.implementors` match only the HEAD — `impl Display for Vec<Point>`
  is reachable from `Vec`, NOT `Point`. `returnedBy`, `acceptedBy`,
  `heldBy`, `signatureTypes` match EVERY nominal in the expression, so
  `-> Option<Point>` IS reachable from `Point.returnedBy`.
- `keyTier`: Structural — key stops resolving ⇒ declaration very likely
  gone, re-search by name. Span — an unrelated edit above the declaration
  may have moved the key; it's probably still there. Ordinal — a producer
  reordering may have moved it; assume nothing about the source changed.
  Unrecorded — no seal report ran; treat as "may have moved" and know it.
- Generic bounds / where-clause predicates are NOT indexed (`T: Display`
  does not make the function `Display.acceptedBy`).
- An edge into an UNLOADED package returns EMPTY, never an error — "not
  indexed here", never "does not exist".
- `visibility`: exactly six values — Public, Private, Protected, Internal,
  Package, Crate. `isPublic` only distinguishes Public from the other five.
- Cross-language: `Trait.implementors` is Rust-`impl`-only. Java/C#/C++/Go
  implementors/subclasses land in `subtypes` instead — querying only
  `implementors` on a non-Rust package silently returns nothing.

## Worked queries (parse-tested against the live schema)

All implementors of a trait, by name — `args: {"name": "Display"}`:
```graphql
{
  Symbols {
    ... on Trait {
      name @filter(op: "=", value: ["$name"])
      implementors { key @output name @output path @output }
    }
  }
}
```

Every function returning a given type, with visibility —
`args: {"typeName": "Point"}`:
```graphql
{
  Symbols {
    name @filter(op: "=", value: ["$typeName"])
    returnedBy {
      ... on Function { key @output name @output isPublic @output }
    }
  }
}
```

What this symbol's own signature references (forward) —
`args: {"key": "cargo:demo#<introhex>"}`:
```graphql
{
  Symbols {
    key @filter(op: "=", value: ["$key"])
    signatureTypes { key @output name @output kind @output }
  }
}
```
"#;
