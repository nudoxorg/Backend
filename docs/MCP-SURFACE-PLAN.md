# docs/MCP-SURFACE-PLAN.md — redesigning the agent-facing surface

Status: **proposal**. Nothing here is implemented. Written 2026-08-15.

This plan covers three things that turned out to be one thing:

1. the **tool surface** — 13 tools where §L6 specified 6, with duplicated
   arguments and duplicated prose;
2. the **response envelope** — markdown that is already disciplined in places
   and wasteful in others; and
3. the **lookup/address scheme** — the 64-hex `IntroId` key, which is the
   weakest part of the surface and the reason several of the other problems
   cannot be fixed locally.

They are one thing because the address is what every tool takes and every
response emits. Fixing the tools without fixing the address moves tokens
around; fixing the address changes what the tools can be.

**Provenance.** Every number below is either measured with a **real BPE
tokenizer** (marked *measured*), read out of source (cited `file:line`), or a
heuristic estimate (marked *estimated*).

**Measurement method.** tiktoken, reporting `cl100k_base` and `o200k_base`
together. Neither is Anthropic's tokenizer (which is not public), but they are
the standard defensible proxies, and agreement between two independent
vocabularies is what makes a claim robust. Where they disagree, both are shown.

**Do not use `heart::cost::estimated_text_tokens` for any compactness claim.**
It counts an alphanumeric run as one token, so a 64-character hex key measures
as **1** token instead of ~50 — a 50× error on the single largest cost in the
payload. The existing `assert_compact` test in `tests/mcp/token_budget_matrix.rs`
is therefore measuring compactness with an instrument blind to what dominates
it. Fixing that instrument is part of this work.

**Decisions taken** (2026-08-15): re-exports follow **Option A** (§4.10).
Optimize for **agent effectiveness, not billing** — the per-`tools/call`
revenue consideration that appeared in an earlier draft of §3 is explicitly
*not* a constraint on this design.

---

## 1. What it costs today

### 1.1 Fixed cost, before the agent does anything

*Measured* at `HEAD` (1a665a3) with tiktoken, sources read via `git show` so
concurrent edits cannot contaminate the baseline:

| Item | bytes | cl100k | o200k | naive¹ |
|---|---:|---:|---:|---:|
| 13 tool descriptions | 10,695 | 2,342 | 2,334 | 2,269 |
| argument doc comments → JSON Schema `description`s | 4,756 | 1,115 | 1,104 | 1,067 |
| server `INSTRUCTIONS` | 1,853 | 420 | 419 | 406 |
| **resident before any call** | **17,304** | **3,875** | **3,856** | 3,742 |
| `graph_schema` response (SDL) | 60,831 | **14,872** | 14,694 | 12,613 |

¹ `heart::cost::estimated_text_tokens`. It tracks closely on prose — where a
word really is about one token — which is exactly why its 50× error on hex went
unnoticed.

Plus JSON Schema structural overhead (property names, `type`, `required`),
not counted above.

Longest descriptions, in cl100k tokens: `index_package` 440 ·
`select_version` 261 · `diff_versions` 260 · `graph_query` 247 ·
`search_symbols` 232.

Reproduce: `scratchpad/baseline_surface.py` (see §10).

### 1.2 The `graph_schema` cliff

`schema.graphql` is 60,517 chars ≈ **15,129 tokens**, served verbatim.

*Measured* by line-classifying the file:

| Category | chars | % |
|---|---:|---:|
| doc comments | 49,270 | 81.4 |
| field definitions | 10,261 | 17.0 |
| type/brace structure | 595 | 1.0 |
| directive declarations | 304 | 0.5 |
| blank | 87 | 0.1 |

**~72% of the file is byte-identical repeated text.** GraphQL SDL cannot say
"as declared on the interface", so the `Symbol` interface's 13-field doc block
is copy-pasted into all 14 implementors. One 4,921-char block (lines 25–111)
is pure human narrative recounting what an older doc comment used to say and
why it was wrong.

The file contains **zero worked query examples**. For a Trustfall dialect most
models have never seen, examples are worth more per token than SDL.

A terse reference card covering every type, scalar, edge, and the
query-breaking semantic rules came in at **~1,017 tokens** — a 93% reduction.

`graph_schema`'s description claims it documents "the cost of each traversal".
It does not: there is one incidental sentence about `signatureTypes` and
nothing for the other 13 edges. **That is a false claim in the surface**, not
merely a verbose one.

### 1.3 The address

`ecosystem:name#<64 lowercase hex>`. *Measured*, both tokenizers agreeing:

| form | bytes | cl100k | o200k |
|---|---:|---:|---:|
| `cargo:serde#<64 hex>` — today | 76 | **54** | **54** |
| `cargo:serde@1.0.196::serde::de::Deserializer::deserialize_map[method]` | 70 | **20** | 21 |
| bare path `serde::de::Deserializer::deserialize_map` | 40 | **8** | 8 |

The hash costs **54 tokens**; a fully-qualified, human-readable, *version-
pinned* address costs **20** — a **63% reduction** while carrying strictly more
information. The bare path is 8.

This is worse than the earlier char/4 estimate suggested (which put the key at
~30), because high-entropy hex defeats BPE merges almost completely: 64 hex
characters cost 50 tokens, i.e. **1.28 chars per token** against ~4 for
identifier text.

---

## 2. Inventory: what is out of place

### 2.1 Surface drift

`docs/GUI-LOCAL-PLAN.md` §L6 specifies **six** tools. **Thirteen** shipped.
No consolidation pass ever ran.

### 2.2 Redundancy in the input surface

- `SearchSymbolsArgs` and `SemanticSearchArgs` are **field-identical**
  (`query`, `kinds`, `packages`, `limit`, `cursor`). Only the prose differs.
- `limit` + `cursor` is copy-pasted across **six** arg structs, each with its
  own near-identical description string, so the pair's schema is regenerated
  six times.
- The pagination explainer ("check `next_cursor`, pass it back as `cursor`,
  treat it as opaque") appears in **six** tool descriptions.
- The key-stability "HONESTY NOTE" is written out in full in **at least three**
  places: `list_versions`'s description, `select_version`'s description, and
  `SymbolNotFound`'s help text.
- `truncated` is *always* `next_cursor.is_some()` — derived, never independent
  (`do_get_occurrences` literally computes it that way). Every paginated result
  carries both.

### 2.3 The two searches are one engine call

*Verified* at `src/mcp/tools/mod.rs:246` / `:265` and `:561` / `:570`:

`do_search` and `do_semantic_search` both issue **the same**
`self.engine.search(query, generation)` with the same `SearchQuery`.
`do_search` then discards every `SECTION_SEMANTIC` row; `do_semantic_search`
discards everything that is not `SECTION_SEMANTIC`.

The engine already runs a hybrid search. The MCP layer splits it into two
tools that each throw away the other's half, and then asks the agent to guess
which half it needed. Unifying them is nearly free.

### 2.4 Dead and inert code

The dead surface is larger than a grep suggests. *Verified by rustc* with
`-W dead-code`:

- **`src/mcp/markdown.rs` (515 lines) is entirely dead.** Zero call sites
  outside its own tests. rustc names: `EMPTY_MARKER`, `NEWLINE_MARKER`,
  `escape_markdown_text`, `escape_table_cell`, `code_span`,
  `exact_fenced_snippet`, `truncation_marker`, `pagination_line`,
  `status_line`, `TableError` (never constructed) and its accessors,
  `MarkdownTable` (never constructed) and all of `new`/`push_row`/
  `column_count`/`row_count`/`render`, `render_table_row`,
  `render_separator_row`, `compact_code_content`, `longest_run`,
  `is_invisible_format`, `push_unicode_escape`, `safe_language_tag`.
- **Every `format_*` wrapper in `result_format.rs` is dead** —
  `format_search_result`, `format_compact_symbol_doc`, `format_symbols_result`,
  `format_usages_result`, `format_packages_result`,
  `format_list_versions_result`, `format_select_version_result`,
  `format_diff_versions_result`, `format_index_package_result`,
  `format_query_result`, `format_schema_result`, `format_occurrences_result`,
  `format_semantic_search_result`. Thirteen functions, one per result type,
  none reachable.
- **`semantic_format`'s status constructors** `ready`, `building`,
  `unavailable`, `no_match` are never called.
- `result_format.rs` also carries **seven unreachable match arms**
  (lines 248, 291, 360, 644, 660, 842, 859) — "multiple earlier patterns match
  some of the same values", i.e. wildcard arms shadowed by earlier ones.

Consequence: the three live renderers (`result_format`, `occurrence_format`,
`semantic_format`) each reimplement escaping and fencing, and disagree — only
`semantic_format` backtick-wraps keys — while a complete, tested, unified
implementation sits unused beside them.
- `OccurrenceStatus::{Partial, Unavailable}` are never constructed at the MCP
  boundary; `result_format.rs:459` hardcodes `Complete`. So `status: complete`
  is a constant line on every occurrences response, and can appear alongside a
  `next:` cursor.
- **No producer emits an owned `Reexport` symbol** — *verified*:
  `Kind::Reexport` is constructed only in `workspace/ir/vcs/lower.rs:169`
  (from a wire kind) and in content hashing. The schema declares
  `type Reexport implements Symbol`, search assigns it weight, and
  `graph/adapter.rs:293` has a vertex arm for it — all unreachable for
  producer-generated packages.

### 2.5 Typing gaps

- `kinds: Option<Vec<String>>` — the 13 valid values live only in prose; the
  JSON Schema says `array<string>`. A client cannot validate locally. (Partly
  mitigated: the error injects `data.validKinds`.)
- `GetSymbolsArgs.keys` 1–32 is enforced at runtime only; no
  `minItems`/`maxItems` in the schema.
- **`limit: 0` silently means 500.** Documented nowhere in the public surface.
- `packages: Some(vec![])` errors; `packages: None` does not. Not expressible
  in the schema.
- `from_version`/`to_version` ordering is unenforced — swapping them returns a
  coherent diff of the wrong direction, with no error.

### 2.6 The top-severity failure: empty means nonexistent

`parse_search_filters` validates only the *syntax* of a package filter, never
membership in the loaded set. A search restricted to an unindexed package
returns an empty hit list with **no signal**. The graph plane has the same
property — the schema's own note says an edge into an unloaded package "yields
an empty edge, not an error."

So "not indexed here" and "does not exist" are indistinguishable, and the
agent's next move after an empty result is to write code against an API it has
concluded is absent. Today the only way to tell them apart is to have called
`list_packages` first and remembered the answer.

**This is the single highest-severity item in this document and it is
independent of everything else. Ship the fix first.**

### 2.7 Global mutable state

`select_version` mutates which generation **all** other tools answer from.
Under concurrent agents, or a single agent that switches to check something and
forgets, every subsequent answer is from the wrong generation with no marker in
any response.

### 2.8 Protocol underuse and one likely-stale test

`structuredContent` is never populated — `metered()` emits only a text block.

`workspace/gui/tests/mcp_endpoint.rs:418` and `:579` assert
`structuredContent.hits` **exists**; `tests/mcp/real_crate_memchr.rs:428`
asserts `structuredContent` is **absent**. The markdown switch (`ca9c379`) is
newer than that GUI test's last edit (`a4fb13b`). **Not run** — flagged for
verification, not asserted broken. See §5.4 for why the resolution is to keep
`structuredContent` off, not to turn it on.

### 2.9 Cosmetic and documentation

- `purl` spells Go as `golang` and C/C++ as `clang`, but ecosystem tags are
  `go` and `cpp` (`src/packages/purl.rs:140-148`, with a dedicated test). So
  you index `pkg:golang/github.com/pkg/errors@v0.9.1` and then filter with
  `go:github.com/pkg/errors`. Deliberate and correct — but documented nowhere
  in the tool surface.
- `DiffVersionsResult` `Rekeyed` rows carry `to_key` **twice** (once in the
  `key` column, once inside the `detail` string) — three full keys per row.
- `tests/mcp/real_crate_dogfood_workflows.rs:31` documents
  `cargo test -p nudox-mcp`; the crate is now `nudox-engine`.

### 2.10 What is already good — do not break it

1. **Boolean-omission discipline** — never prints `field: false`; absence *is*
   the false case. Pinned by tests.
2. `pagination()` emits nothing when there is no next page.
3. `PackagesResult`'s single-column table; `name`/`ecosystem` derived from
   `lineage`, not repeated.
4. **Occurrence grouping** — `occurrence_format` hoists repeated target/owner
   identity into headings, the only renderer that amortizes key cost.
5. Byte-exact fenced passthrough for source and schema, with fence sizing.
6. `render_signature_query`'s `//`-comment-over-code block instead of a wide
   table with an embedded multi-line signature.
7. Scores opt-in and off by default in semantic search.
8. No column padding anywhere.
9. Errors are JSON-RPC protocol errors carrying structured `data{kind, help}`.
   Account errors are uniformly actionable; `GraphQueryFailed` branches its
   help on whether a position is available. This is a genuinely good pattern.

---

## 3. Guardrails any redesign must satisfy

From `tests/mcp/schemas.rs` and `schema_source.rs`:

- Every result type's root schema must declare `"type": "object"`.
  An internally-tagged enum without `#[schemars(extend("type" = "object"))]`
  **panics the server at construction**, taking down every endpoint test and
  the GUI screenshot harness. This has happened twice.
- Every argument field must carry a `description`.
- Keys must serialize as plain JSON strings, never objects.
- `CompactSymbolDoc` must not carry `head`/`timeline`/`sections`/
  `section_plan`/`breadcrumb`.
- `signature` must be absent when `source` is present.
- `SCHEMA_SDL` must stay byte-identical to `schema.graphql`.
- Version-switching schema descriptions must contain "not guaranteed" /
  "indistinguishable from".
- `get_symbols`'s default format must stay `source`.

**Not a guardrail: billing.** `docs/auth.md` and `account/ledger.rs` bill one
flat `tool_call` per `tools/call`, so reducing round trips is revenue-negative.
**Decided 2026-08-15: optimize for agent effectiveness, not revenue.** Fewer,
better round trips is the goal even where it bills less. If per-call billing
later proves load-bearing, move the meter to a unit that tracks real cost
(symbols returned, bytes served) — do not reintroduce round trips to protect it.

---

## 4. The address (the centerpiece)

The proposed address form, up front so the rest of this section has something
concrete to argue about:

```bnf
address      ::= pkg-coord [ "::" sym-path ] [ "#" key ]
pkg-coord    ::= ecosystem ":" pkg-name [ "@" version ]
ecosystem    ::= "cargo"|"npm"|"maven"|"nuget"|"pypi"|"go"|"cpp"
sym-path     ::= segment { sep segment }
sep          ::= "::" | "." | "/"        ; all normalize to a segment boundary
segment      ::= name [ qual-block ]     ; qualifiers attach to ANY segment
               | qual-block              ; ANONYMOUS segment — impl blocks
name         ::= ident | "'" quoted "'"  ; quoted = synthesized / non-identifier
qual-block   ::= "[" qual { "," qual } "]"   ; split at nesting depth 0 only
qual         ::= kind | disambiguator | locator
disambiguator::= "(" [type-list] ")" | "/" digit+          ; FnOverload
               | ["!"] "impl" [generics] type ["for" type] ; TraitImpl
               | "~" digit+                                ; positional, generation-scoped
locator      ::= "at" file ":" line                        ; INPUT ONLY, never emitted
key          ::= 8..64 lowercase hex     ; <64 = package-scoped prefix
```

`[]`, `()`, `<>` nest and must balance — that is what lets
`readValue[method(byte[],Class<?>)]` parse. `::` is the package/symbol
boundary because it appears in no ecosystem's package name (Maven's
`group:artifact` uses single colons; npm `@scope/name` and Go `host/path` use
`/`). The version separator is the **last** `@`, so `npm:@types/node@20.11.0`
parses. purl is accepted on input and normalized (`golang`→`go`,
`clang`→`cpp`) but **never emitted**, since the emitted form must match the
key preimage's ecosystem tag.

**`sym-path` always means the physical path** (§4.9). An agent may *type* a
public path — §4.10 Stage 2 catches it — but what comes back is always
physical. One address form, one meaning.

**The legacy form is a strict subset:** `cargo:serde#3f1a…` parses as
pkg-coord + empty sym-path + key. Every key in every existing result and test
is already a valid address, so there is no cutover at any stage.

Worked examples, corrected for *physical* paths:

```
cargo:serde@1.0.203::de::Deserializer[trait]
    public_path serde::Deserializer — address ≠ what the agent types; Stage 2 bridges.

cargo:memchr@2.7.4::memchr::memchr::memchr::Memchr[struct]
    The real fixture. Physically nested and repetitive, NOT hand-composable.
    This example exists to kill the illusion that physical paths are typeable.

cargo:serde@1.0.203::serde::de::Deserializer::deserialize_str[method]
cargo:serde_json@1.0.117::serde_json::value::de::[impl Deserializer for Value]::deserialize_str[method]
    The "trait method vs inherent method of the same name" hard case. Same leaf,
    same kind, distinguished purely by the impl ancestor. Because every Rust impl
    block is named "impl", the name carries zero information and is elided — the
    bracket IS the segment (anonymous segment in the grammar above).
    *** REQUIRES SEAL ITEM 3 — unwritable today. ***

maven:com.google.code.gson:gson@2.11.0::com.google.gson.Gson.toJson[method(Object,..)]
maven:…::com.google.gson.Gson.toJson[method(JsonElement,..)]
    Two of Gson's EIGHT toJson overloads. *** REQUIRES SEAL ITEM 3. ***
    Until then: eight hash-only rows an agent cannot tell apart. This is the
    strongest single argument for item 3.

nuget:Dapper@2.1.35::Dapper.DapperRow.'this[]'[property(String)]
    C# indexer; bare name is not an identifier. The quoting is a deliberate
    signal: emitted for you, do not compose by hand.

go:std@1.22.0::encoding/json::Encoder::Encode[method]
    Go forbids same-named methods on T and *T, so no collision. Pointer-ness is
    a signature detail, never an address component. physical == public.
    Composable from docs — the best-case ecosystem.

npm:express-serve-static-core@4.19.2::Express::Request[interface, ~1]#8c4e…
    Declaration-merged TS interface: Span/Ordinal tier, hash mandatory.
    The ~n + # pair is the "unstable, do not cache across versions" signal.

pypi:requests@2.32.3::requests.sessions.Session.get[method]
cargo:tokio@1.38::sync::mutex::Mutex[struct]    public_path tokio::sync::Mutex
```

**The four hardest cases the design has to handle — Rust impl members and
Java/C#/TS overloads — are exactly the four that cannot be addressed at all
until seal item 3 ships.** That is the finding, and it drives the sequencing
in §8.

A note on what is deliberately *not* renderable: byte offsets. `[span
4120..4890]` would look addressable, get cached, and rot. Span/Ordinal
declarations get `~n` (positional, generation-scoped only) plus a mandatory
hash. `at file:line` is accepted on input — agents see file:line everywhere and
will type it — but never emitted, so it never becomes something they cache.

### 4.1 Why the key is weak

Five distinct problems, needing different fixes:

1. **Not constructible.** An agent that already knows "serde's
   `Deserializer::deserialize_map`" must still spend a search round trip to
   learn 64 hex characters. Every lookup costs a minimum of two calls.
2. **Not verifiable.** Neither the agent nor a human reading the transcript
   can distinguish a correct key from a plausible hallucinated one.
3. **Carries no version.** It resolves against whatever generation
   `select_version` last set.
4. **Not uniformly stable.** See §4.2.
5. **Expensive.** ~25–30 tokens of high-entropy hex, repeated per row and
   passed back per argument.

### 4.2 What identity actually is

*Verified* at `workspace/ir/model/src/intro.rs:159-173`:

```
IntroId = blake3("nudox.intro.v5"
                 ‖ encode_str(ecosystem) ‖ encode_str(package_name)
                 ‖ u16le(kind_discriminant)
                 ‖ encode_segments(ancestor_names)
                 ‖ encode_str(leaf_name)
                 ‖ disambiguator_bytes)
```

**The key is a hash of the path.** Not of the file, not of byte offsets, not
of the signature — of `(package, kind, ancestor names, leaf name,
disambiguator)`. For the common case the disambiguator is a single `0x00`
byte, so the key is a pure function of information an agent can type.

The disambiguator ladder (`seal.rs:376-412`, escalation at `:420-543`):

| Disambiguator | When | Natural rendering | Tier |
|---|---|---|---|
| `None` | `(kind, path, name)` unique | *(none needed)* | Structural |
| `FnOverload(sig skeleton)` | ≥2 functions collide | parameter types | Structural |
| `TraitImpl(trait, self, …)` | **every** impl block | `Trait for SelfType` | Structural |
| `Span{start,end}` | other collisions | byte offsets — meaningless | **unstable** |
| `Ordinal{…, index}` | degenerate skeletons | emission order — meaningless | **unstable** |

**The alignment that makes this work:** the two tiers that need an opaque hex
suffix are exactly the two whose identity does not survive a version bump. The
ugly suffix doubles as a staleness warning rather than being dead weight.

`KeyTier` is `Structural | Span | Ordinal` (`seal.rs:229-259`), plus
`Unrecorded` at the engine layer (`store/package.rs:466-483`) when no seal
report reached the view. `is_content_derived()` is true only for `Structural`.

### 4.3 Four facts that constrain the design

These were established after the first draft and materially narrow it.

**(a) The hashed path is the *physical* path, not the pretty one.**
`seal.rs:307-321` walks the raw parent chain collecting `Symbol.name` with no
alias-shortening. A real fixture records memchr's path as
`memchr.memchr.memchr.Memchr`. So "hash the typed path" only works for the
physical path — precisely the one the display layer exists to hide.

**(b) There are two disagreeing `path` implementations.**

| | source | separator | alias-shortened | includes leaf |
|---|---|---|---|---|
| MCP `path` | `chunk::head::projection` + `compact_symbol` | `::` | yes (`shortest_alias_module_path`) | yes |
| GraphQL `Symbol.path` | `nudox_ir::reflect::moniker_path` | `.` | no | yes |

Both hardcode their separator globally, so Java renders with `::` in MCP and
Rust renders with `.` in GraphQL. **There is no canonical path string today.**

**(c) Re-exports are structurally invisible to name search.** A `pub use`
lowers to `EntryInner::Reference`, whose `.discriminant()` is always `None`,
and `collect_name_hits` does `let Some(disc) = … else { continue; }`. Pinned by
the real test `memchr_reexport_aliases_are_invisible_to_name_search`, whose
comment says a user "can never be shown the short, public path". So an agent
typing `serde::Deserializer` gets nothing today.

**(d) CORRECTED 2026-08-15 — Rust impl blocks are NOT all named `"impl"`.**

An earlier draft of this plan asserted that every Rust `impl` shares the name
`"impl"`, and concluded that impl members were therefore unaddressable by path.
**That is false**, and it was disproved by real baseline output before anything
was built on it.

`workspace/compiler/languages/src/rust/ra/item.rs:1524` sets
`name: impl_display_name(ctx, imp)`, and `impl_display_name` (`:1633`) renders
`impl {Trait}{args} for {SelfTy}`. Real paths observed in a live `get_symbols`
response over serde 1.0.196:

```
serde::serde::de::impls::impl Deserialize<'de> for Mutex<T>::deserialize
serde::serde::de::impls::impl Deserialize<'de> for NonZero<u8>::deserialize
serde::serde::de::impls::impl Deserialize<'de> for [T; 25]::deserialize
serde::serde::de::impls::impl Deserialize<'de> for Box<T, Global>::deserialize
```

The impl segment is fully distinguishing. What remains true is narrower and
still matters: the sealer applies `Disambiguator::TraitImpl` to **every** impl
unconditionally, regardless of whether its name collides — so an impl member's
key is never `Disambiguator::None`, and the Stage-1 pure-hash fast path cannot
construct it. Stage 3 (name plan + path-suffix filter) finds it instead, and
because the names *are* distinguishing it should usually find exactly one.

**Consequence: impl-member addressability is substantially better than this
plan originally assumed.** The unconditional `TraitImpl` is belt-and-braces,
not evidence of collision. Whether `f_struct` is TraitImpl-dominated — and
therefore path-distinguishable *today*, with no sealer work — is a question
for gate zero (§4.13).

Genuine collisions do exist elsewhere and are unaffected by this correction:
Gson's eight `toJson` overloads and Dapper/CsvHelper indexers all named
`this[]` carry the bare method name, so every overload renders identical path
text.

And the disambiguator that would resolve them is **hashed and never exposed as
text** — GraphQL surfaces only `keyTier`. Any readable suffix is new surface
that must be plumbed out of the sealer.

### 4.4 The escape hatch that is already paid for

`Symbol.aliases` is populated by **all seven producers** — *verified* by grep
across `typescript/emit.rs`, `go/`, `java/lower.rs`, `rust/ra/`,
`csharp/lower.rs`, `clang/lower.rs`, `python/`. It is already used for display
shortening.

**Inverting it yields a cross-language alias → symbol index for free**, which
makes the idiomatic public path (`serde::Deserializer`) resolvable without
touching `EntryInner::Reference` entries and without reviving the dead
`Reexport` vertex. This is the cheapest available fix for §4.3(c).

### 4.5 Stability: the known, unfixed defect

- `unchanged_declarations_keep_their_intro_id_across_versions`
  (`tests/store/corpus_contract.rs:1830-2029`) is committed **deliberately RED
  and `#[ignore]`d**. Key stability is a known defect, not an assumption.
- *Measured 2026-08-08*, 79 package versions / 446,947 declarations: **32,339
  ordinal-keyed groups across 49 of 79 packages**. By kind: Param 13,601 ·
  Field 3,845 · Const 3,809 · Function 3,395 · Module 2,772 · Reexport 1,480 ·
  Static 1,335 · Trait 1,310 · Alias 600 · Record 129.
- **24 declarations provably changed their published key between real
  releases** (log, memchr, jackson-databind, lodash).

**That measurement is stale.** The producers that hardcoded degenerate `0..0`
spans were fixed on 2026-08-15 and it has not been re-run. Treat 32,339 as an
upper bound. **Re-running the census is a prerequisite** — every stability
claim and token estimate in this plan scales with the unstable share.

### 4.6 Highest-leverage engine change — CORRECTED by gate zero

An earlier draft named `Param` as the top target, on the strength of the
2026-08-08 census (Param = 13,601 of 32,339 ordinal groups, the largest bucket
by far). **That number was stale and the conclusion was wrong.**

Re-measured 2026-08-15 over 22 of 23 corpus packages / 77,873 declarations
(§4.13), the unstable population is only 2,084 declarations, and its
composition has inverted:

| kind | count | % of `f_unstable` |
|---|---:|---:|
| **Field** | **1,901** | **91.2%** |
| Reexport | 118 | 5.7% |
| Impl | 35 | 1.7% |
| Module | 14 | 0.7% |
| **Param** | **10** | **0.5%** |
| Function | 6 | 0.3% |

The 2026-08-15 producer fix (removing hardcoded degenerate `0..0` spans)
worked, and it worked almost entirely on `Param`: **13,601 → 10**.

**So the proposal stands but the target changes.** A positional
(`MemberIndex(u16)`, content-derived, `Structural`) disambiguator applied to
`Param` would now fix essentially nothing — 0.5% of an already-small 2.68%
bucket. Applied to **`Field`** it addresses **91% of the entire unstable
population**.

This is a good example of why gate zero was a prerequisite rather than a
formality: acting on the stale number would have bought a rounding error.

### 4.7 The constraint that decides the design

**Rendering a disambiguator at query time is impossible, not merely hard.**
blake3 is one-way and the skeleton bytes are *hashed, not stored* — the text
`(String, Class)` or `impl Display for Foo` exists nowhere in the store. So any
readable disambiguator is a **seal-format change requiring a full corpus
re-seal**, not a query-layer feature.

This is worth writing down because it is the most natural idea anyone
approaching the problem fresh will have, and it costs a day to rediscover.

### 4.8 Verdict: path as the *resolution and display* surface, not identity

**Committed: path-as-secondary. The hash remains identity, forever.**

The steelman for path-as-primary is real — for `Disambiguator::None`
declarations the address is *provably* as exact as the hash, because it is the
preimage; and for maven/pypi/go/nuget the physical path *is* the idiomatic
path. It still loses on three individually fatal grounds:

1. **Not total.** The hash is defined for 100% of declarations. The address is
   undefined for null-`path` symbols, undefined-in-practice for Span/Ordinal,
   and unrenderable today for FnOverload/TraitImpl. An identity scheme
   undefined for a large minority is a display convention with pretensions.
2. **Rust's most-queried symbols are the least addressable.** Chain §4.3(d):
   every impl is named `impl` → two `impl Display for A` / `for B` in one
   module give their methods identical `(kind, ancestors, leaf)` → their
   signature skeletons are identical too, so `FnOverload` cannot separate them
   → the ladder escalates to `Span`, then `Ordinal`. So a large class of Rust
   methods is simultaneously unrenderable, unstable, **and** the thing agents
   ask for most. Ship path-as-primary and Rust regresses.
3. **Key stability is a known-RED defect** (§4.5). Promoting path to primary
   before that is fixed creates two identity notions that can disagree about
   "same symbol" with no arbiter.

But ~95% of what people want from path-as-primary — readable, typeable,
composable, self-healing on typo, distinguishes not-indexed from not-found,
survives a version bump when the key does not — belongs to the **resolution
and display** surfaces and is available without touching identity.

| Layer | What it is | Changes? |
|---|---|---|
| **Identity** | `IntroId`, 64-hex | never; accepted and available forever |
| **Handle** (what the agent echoes) | hash today; address for sealer-certified declarations from S3 | shrinks as certified coverage grows |
| **Resolution** | address grammar, four-stage resolver, typed statuses | new — ship immediately |
| **Display** | `physical_path` + `public_path` + rendered disambiguator | new — ship the parts needing no re-seal |

### 4.9 Split the two paths; they are two concepts with one name

Neither existing implementation is canonical. They are different concepts that
were accidentally merged, and must be named honestly and never conflated:

```
physical_path : the raw parent chain — exactly the segments hashed into IntroId.
                cargo:  serde::de::Deserializer
                maven:  com.fasterxml.jackson.databind.ObjectMapper
                Authoritative for identity. This is what an address's path means.

public_path   : the shortest path by which the declaration is reachable from the
                package root through re-exports/aliases.
                cargo:  serde::Deserializer
                Authoritative for nothing; a convenience, and may be non-unique.
```

`moniker_path` becomes canonical for `physical_path` — it already walks the
preimage chain, so it is the only one of the two that cannot silently drift
from the key. `chunk::head::projection`'s alias-shortening is **not deleted**;
it is promoted to being the `public_path` producer, which is the one thing it
already knows how to do.

The single change either needs is `PathStyle::for_ecosystem` (`cargo`/`cpp` →
`::`; `maven`/`pypi`/`npm`/`nuget`/`go` → `.`), which fixes Java rendering with
`::` and Rust with `.`.

**Ordering hazard:** aliasing `Symbol.path` to `physicalPath` changes Rust from
`.` to `::`, a wire-visible break for anything filtering `path` in
`graph_query`. Ship **filter-side separator normalization first** — normalizing
`::`, `.`, `/` to segment boundaries on both sides of the comparison — so old
queries keep matching.

And add a debug assertion, on in tests:
`hash_preimage_segments(decl) == split(physical_path(decl))`. Without it, drift
between the derived path and the preimage is undetectable by construction —
which is exactly the situation today's two implementations created.

### 4.10 How `serde::Deserializer` resolves — no new seal data, no test inversion

This is the make-or-break question, and the constraint was narrower than it
first appeared. `collect_name_hits` skips `EntryInner::Reference` entries, which
makes the *re-export declaration* invisible. It does **not** make the
*definition* invisible — a name search for `Deserializer` finds it today. What
fails is a search for the *path* `serde::Deserializer`, and what is missing
from the output is the short public path.

So the fix is not "make re-export rows searchable." It is **"attach the public
path to the definition, and let the definition answer for its aliases."**

Four-stage resolver:

| Stage | Mechanism | Cost |
|---|---|---|
| 1 · physical exact | brute-force kind (~25) × root-elision (~3) = ~75 blake3 over ~100B | ~15 µs, **no index** |
| 2 · alias index | invert existing `Symbol.aliases` at load → `alias_path → [IntroId]` | one HashMap/package, **no new seal output** |
| 3 · name-plan + suffix filter | plan on `name` (**already a plan hint**), post-filter on physical/public path suffix | **no FullScan** |
| 4 · fuzzy | edit distance, facets | opt-in |

Stage 3 is the important structural point: **do not plan on path.** Plan on
`name`, then use the path as a post-filter over the small name-hit set. This
sidesteps the missing `path` plan hint entirely and demotes it from
prerequisite to optimization.

**DECIDED 2026-08-15: Option A.** Two designs were produced independently and
disagreed here. Both agree on the alias index; they differ on whether re-export
declarations become independently addressable. Option A is chosen and is what
the implementation follows — recorded with both options because the reasoning
matters if B is ever revisited.

**Option A — leave search semantics alone.**
`memchr_reexport_aliases_are_invisible_to_name_search` stays green and
unmodified. Its complaint — a user "can never be shown the short, public path"
— is answered by attaching `public_path` to the *definition*, not by changing
what search returns. No behaviour change, no test inversion, no risk.

**Option B — make re-exports first-class.** Give `EntryInner::Reference` a
synthetic `Reexport` discriminant (the kind already exists in the schema) so
re-export rows stop being `continue`d past in `collect_name_hits`. The pinned
test flips from asserting invisibility to asserting the alias is found and
redirects; that inversion becomes the acceptance criterion. This is what makes
`cargo:memchr@2.7.4::memchr::Memchr[reexport] → …::Memchr[struct]` addressable
as a distinct entity with a `resolves_to` edge.

**Recommendation: A first, B only if a concrete need appears.** A delivers the
user-visible outcome (typing the idiomatic path works) at zero regression risk.
B is strictly more correct but inverts a deliberately-written test, changes
search results for every package with re-exports, and would also revive the
currently-dead `Reexport` vertex (§2.4) — three behaviour changes for a want
nobody has articulated yet. Revisit B if agents need to ask *"what is
re-exported from this crate root"* as a first-class question.

**Honest limits.** Stage 2 covers only what `Symbol.aliases` recorded;
multi-hop re-export chains may need a load-time BFS over `Reference` edges from
the package root (which never touches `collect_name_hits`, so search semantics
are undisturbed). And the "agent composes an address from docs knowledge and
hits in one call" story is **dead for cargo and npm** — docs show public paths,
the index holds physical ones, so those land in Stage 2/3. It survives intact
for **maven, pypi, go, nuget**, where physical ≈ public. The fast path's value
is ecosystem-dependent; it should not be sold as universal.

### 4.11 What must come out of the sealer

Not "expose the raw disambiguator" — expose a **rendered, uniqueness-verified**
address, because the sealer is the only component that both holds the types
needed to render and holds every key in the package, so it can *prove*
uniqueness rather than estimate it.

| # | Field | Cost | Unlocks |
|---|---|---|---|
| 1 | `physical_segments` — the exact hashed segments | trivial | kills preimage/path drift permanently |
| 2 | `kind_alias` — stable string for the u16 | trivial | `[trait]`, `[method]` |
| 3 | `disambiguator_render` — **only** for `FnOverload`/`TraitImpl`; `None` for Span/Ordinal | **real work** | Java/C# overloads; **all Rust impl members** |
| 4 | `addressable: bool` + `canonical_address` — sealer-verified to round-trip | moderate | safe handle promotion, no guessing |
| 5 | `public_paths`, shortest first | moderate | re-export reachability is package-global; compute once |
| 6 | `key_prefix_len` — minimal collision-free hex prefix per package | trivial (sort + adjacent scan) | **the token win** — see §4.12 |
| 7 | `key_tier` reaching the view without `Unrecorded` degradation | plumbing | correct stability signalling |

Items 1, 2, 6, 7 are near-free. Item 5 is moderate. **Items 3 and 4 are the
real project** — and they are exactly what path-as-primary would have required
wholesale, which is why it is the wrong bet to open with.

### 4.12 The token win is not where it looks

*Measured* with tiktoken on a realistic `serde::de::Deserializer::deserialize_map`
row (full signature retained in both):

**Identity fields alone** — `key | path | kind` today vs. one address:

| | bytes | cl100k | o200k |
|---|---:|---:|---:|
| `cargo:serde#<64hex>` + path + kind | 130 | 65 | 65 |
| `cargo:serde@1.0.196::…::deserialize_map[method]` | 69 | **20** | 21 |

**−69%**, and the address additionally carries the version, which the three
fields it replaces did not.

**Whole row**, current markdown-table rendering vs. the proposed two-line
record:

| | bytes | cl100k | naive |
|---|---:|---:|---:|
| current table row | 182 | 89 | 45 |
| proposed 2-line record | 171 | **53** | 61 |

**−40% per row; a 20-hit search goes 1,780 → 1,060, saving 720 tokens.** The
residual is almost entirely signature text, which is intended — the signature
is the primary identity and is never truncated.

> **Why the instrument had to be fixed first.** Read the `naive` column: it
> scores the current row at 45 and the redesigned row at 61. The estimator
> currently in `heart::cost` would have reported this change as a **36%
> regression** while the real tokenizers report a 40% improvement. It cannot
> see that a 64-character hex run costs 50 tokens, so it prices the thing being
> removed at almost nothing and the readable text replacing it at full freight.

Earlier drafts of this section estimated the S1 "address alongside hash" step as
token-neutral. That was based on a char/4 model that under-priced hex; with real
measurement the address is a large win *immediately*, because it replaces three
fields rather than being added beside them.

**Be honest about S1: it is token-neutral.** Its win is entirely behavioural —
no empty results, self-healing typos, fewer round trips. Anyone selling S1 on
token count is selling nothing.

**The token win is S2**, and it is *independent of the tier distribution*
because prefix shortening applies to every row. Sealer item 6 — one sort and
one adjacent-pair scan — deserves to jump the queue ahead of the expensive
item 3.

The larger, unmodelled win is eliminated round trips: a composed address that
resolves in one call replaces a search result worth 600–1,500 tokens. Ten
avoided searches ≈ **6,000–15,000 tokens**, dwarfing the address text entirely.

### 4.13 Gate zero — RESULTS (measured 2026-08-15)

Ran over the real crates.io corpus, **22 of 23 packages / 77,873
declarations**. (`log-0.4.33` could not be lowered offline — its `cargo
metadata` needs `sval v2.19.0`, which is not vendored. Environmental, not a
producer defect; excluded from all totals.)

Reproduce:
```
NUDOX_CENSUS_WRITABLE_CORPUS=<writable copy of result/> \
  cargo test -p nudox-languages --test rust_disambiguator_census \
  --features seal-census -- --ignored --nocapture \
  disambiguator_tier_census_over_the_crates_io_corpus
```

```
Disambiguator variant distribution:
  None          67299  (86.42%)
  FnOverload        0  (0.00%)
  TraitImpl      8490  (10.90%)
  Span           1085  (1.39%)
  Ordinal         999  (1.28%)
Path-addressability buckets:
  f_none        67299  (86.42%)
  f_struct       8490  (10.90%)
  f_unstable     2084  (2.68%)
```

Three results that change the design's conclusions:

**`FnOverload` never fires — zero occurrences corpus-wide.** Rust has no
name-based overloading, so `f_struct` is **100% `TraitImpl`**. The
FnOverload-rendering half of sealer item 3 (§4.11) buys nothing for cargo and
should be scheduled only when a non-Rust ecosystem is in the corpus, where
Gson's eight `toJson` overloads live.

**`f_unstable` is 2.68%, not the ~7% the stale census implied** — and its
composition inverted (§4.6): Field 91.2%, Param 0.5%.

**The `f_none` / `f_struct` split is the answer to §4.3(d).** memchr 2.8.3
alone: `f_none` = 84.55%, `f_none + f_struct` = **96.24%**. That brackets the
independently-measured 96.1% path-resolution rate (§4.15) almost exactly —
confirming that path-addressability today comes substantially from `TraitImpl`
declarations whose **rendered name** distinguishes them even though their
**key** is not `Disambiguator::None`. One completed per-package check of the
impl-uniqueness instrumentation: `bytes-1.11.0`, **176/176 `TraitImpl`
declarations already unique by `moniker_path`, zero collisions.**

Two more, both clean: **null-`path` declarations = 0** across all 22 packages,
so §4.16's degenerate case does not arise in practice; and pass-2.5 escalation
**never exceeded 2 rounds** (`MAX_ROUNDS = 8` is a defensive bound, never
approached), matching the code's own argument that `Ordinal` is
collision-free by construction.

*What this gate was for, and how the prediction fared.* It was posed as: if
cargo's `f_none` falls below ~40%, sealer item 3 stops being an optimization
and becomes a prerequisite for the scheme meaning anything in Rust. The answer
is `f_none = 86.4%`, comfortably above that line — **item 3 stays an
optimization for cargo**, and the FnOverload half of it is dead weight there
entirely. The worry was reasonable and the measurement retired it.

### 4.14 DEFECT FOUND BY GATE ZERO: `keyTier` over-reports stability

*Measured 2026-08-15.* The census reads the `Disambiguator` actually hashed
into each declaration. Production derives `keyTier` from
`SealReport::forced_keys` instead. **They disagree on 1,044 declarations.**

| `KeyTier` | true (census) | production (`forced_keys`-derived) |
|---|---:|---:|
| Structural | 75,789 (97.32%) | 76,833 (98.66%) |
| Span | **1,085 (1.39%)** | **41 (0.05%)** |
| Ordinal | 999 (1.28%) | 999 (1.28%) |

`forced_keys` records only pass-2.5 *escalations*. It misses the "other
collision" arm in pass 2 (`seal.rs` ~line 403), which mints
`Disambiguator::Span` directly without ever entering pass 2.5. So **1,044
Span-keyed declarations are reported to agents as `Structural`.**

This is not cosmetic. `keyTier` is the field the schema card tells an agent to
read *before* caching a key across a version switch, and `Structural` means
"content-derived, safe to cache". For these 1,044 declarations that claim is
false, and the failure mode is the one this whole plan is trying to eliminate:
the key silently stops resolving after a bump, indistinguishable from deletion.

Fix belongs in `KeyProvenance::from_seal_report` — it needs the full
per-declaration tier, not the escalation list. The census instrumentation
already computes exactly that.

### 4.15 MEASURED: path resolution over a real package

*Measured 2026-08-15* against real memchr 2.8.3 (1,819 declarations lowered
through the real Rust producer), via
`cargo test -p nudox-engine --test mcp_address_resolution -- --ignored`:

```
checked 1819 rendered addresses against real memchr-2.8.3
test every_rendered_address_with_its_hash_resolves_to_itself_in_memchr ... ok
path-only round trip over 1819 memchr declarations: 1748 uniquely resolved, 71 honestly ambiguous
test path_only_round_trip_finds_or_honestly_flags_every_declaration_in_memchr ... ok
```

**96.1% of real declarations resolve uniquely from the path alone**, with the
remaining 3.9% reported as `Ambiguous` and **zero wrong answers**. Every
rendered address round-trips to the intro it was rendered from.

This is the number the whole design rests on, and it is better than §4.13's
pessimistic framing predicted — largely for the reason in §4.3(d): impl
segments carry distinguishing names, so they are path-addressable today
without any sealer work.

Caveat: one package, one ecosystem. memchr is small and Rust-only. The
cross-ecosystem picture is still gate zero's job.

### 4.16 The closure property that makes this safe

When `path` is null, the emitted address degrades to
`cargo:serde@1.0.203#3f1a…` — the legacy form. **The worst case of the new
scheme is exactly today's behaviour.** That is what makes S0–S2 safe to ship
before gate zero reports.

---

## 5. The tool surface

### 5.1 Thirteen to eight

| today | becomes | why |
|---|---|---|
| `search_symbols` + `semantic_search` | **`search`** | one engine call already (§2.3); the split manufactures a coin-flip |
| `get_symbol` + `get_symbols` | **`read`** | always plural; the singular variant's only effect is to invite N sequential calls |
| `find_usages` + `get_occurrences` | **`refs`** (`direction: in\|out`) | inbound and outbound halves of one relation |
| `list_packages` + `list_versions` | **`packages`** | one question: what is loaded, at what versions |
| `diff_versions` | **`diff`** | distinct task, kept |
| `index_package` | **`index`** | plus implicit triggering (§5.3) |
| `graph_query` | **`graph`** (+ `dryrun`) | escape hatch, kept |
| `graph_schema` | **`schema`** (`topic?`) | the ~1k-token card, not the 15k SDL |
| `select_version` | **deleted** | version becomes an address coordinate (§2.7) |

Eight tools. *Estimated* fixed surface **~1,200–1,500 tokens** against 4,373
today — but that is the least important line in this table. What matters is
that an unindexed package can no longer masquerade as an absent symbol, a
wrong version can no longer answer silently, and the 15k-token schema call no
longer exists.

### 5.2 Naming

Research across the GitHub MCP server, Serena, mcp-language-server, Context7,
DeepWiki, and Sourcegraph found **no converged convention** — bare `verb_noun`
dominates for fine-grained tools, `resource_verb` + a `method` argument shows
up specifically where a large REST API is being wrapped, and explicit
protocol prefixes (`lsp_*`) appear where a server maps 1:1 onto an external
protocol.

Anthropic's own guidance recommends consolidating tools that represent **one
conceptual task usually chained together** (their example: replace
`list_users` + `list_events` + `create_event` with `schedule_event`) — not
collapsing unrelated operations behind one `op:` enum. Practitioner consensus
and Sourcegraph's published rationale both push back on a single mega-tool.

So: **selective consolidation, not blanket consolidation.** Each merge above
joins two tools that answer one question. No `op` dispatch tool.

There is **no rigorous benchmark** isolating "N narrow tools vs. 1 tool + enum"
as the sole variable. Anyone citing one should be asked for it. What *is*
measured is that reducing resident tool-definition tokens helps: Anthropic's
Tool Search Tool moved Opus 4 tool-selection accuracy from 49% → 74% in a
many-tools setting — evidence for lazy loading, not for schema consolidation.

### 5.3 There is no empty result

Every tool returns a typed status. At minimum:
`ok` · `not_indexed` · `indexing` · `no_match` · `filtered_out` · `ambiguous`.

- A query naming an unindexed package returns `not_indexed` **and starts the
  index job**, returning `indexing` with a stage — never an empty list.
- `no_match` carries near-misses as usable addresses, and a `widen` line only
  when the server has *already run* the widened query and knows it returns
  something. Never suggest an action that might itself come back empty.
- `ambiguous` lists candidates with their disambiguated addresses, so the
  retry is guaranteed to succeed.

### 5.4 `index`, and the shape of long-running work

Today's poll-by-recall is the worst option: the same tool returns either a
ticket or a result, so the agent cannot parse the response shape reliably, and
having no clock it typically polls once, sees "running", and proceeds as
though the package exists.

**Recommendation: blocking-with-budget.** `index(purl, wait)` blocks up to the
budget and returns either the finished result or a job handle; `index(job:…)`
resumes waiting. For a turn-based agent, blocking costs zero extra turns and
zero decisions. Emit progress notifications as an optimization; polling stays
the contract.

Rejected: fire-and-forget via notifications/resources — protocol-correct but
unreliable in practice, since a notification arriving between turns is not
dependably actionable and clients vary in whether they surface it.

The decisive part is not the tool shape but §5.3: **any query against an
unindexed package must auto-start indexing**, converting the worst failure
mode into a visible wait.

---

## 6. The response envelope

### 6.1 Do not send `structuredContent`

The MCP spec's dual-emission guidance exists for **backwards compatibility**.
SEP-1624 states clients "SHOULD NOT forward both fields verbatim to models as
semantically distinct inputs" and to "prefer `content` for lower token cost".

More pointedly: Claude Code is **reported** to have changed between 2.0.10 and
2.0.22 so that `structuredContent` takes priority and the text block is
discarded (issue closed as not-planned, so treat it as intended). If that
holds for the pinned client, declaring an `outputSchema` means every decision
in this section is discarded at render time and the model receives
pretty-printed JSON at roughly 2–3× the tokens.

Sending both is never additive: either the client forwards the JSON (the
markdown is 100% waste) or it forwards the text (the JSON is 100% waste).

**Rule: do not declare `outputSchema` on model-facing tools.** Programmatic
clients get a different door — a `format: "json"` argument or a distinct tool
name — not a second payload on the same call. This also resolves §2.8: the GUI
test is not merely stale, it is pulling against the envelope.

*Verify the Claude Code precedence claim against your pinned version.* The
rule above is robust either way.

### 6.2 Layout

*Estimated*, same 50 logical rows through six renderings:

| layout | 5 rows | 20 rows | 50 rows | tok/row |
|---|---:|---:|---:|---:|
| markdown table, full keys *(today)* | 536 | 2,000 | 5,021 | 99.7 |
| markdown table, hoisted + short key | 176 | 645 | 1,602 | 31.7 |
| **one line per record** | 118 | 497 | 1,274 | **25.7** |
| TSV in a fence | 122 | 501 | 1,278 | 25.7 |
| indented outline | 123 | 517 | 1,324 | 26.7 |
| **2-line record, signatures intact** | 304 | 1,137 | 2,874 | 57.1 |

Table chrome alone (pipes + header + rule, content held constant) costs
**+58 tokens at 5 rows and +328 at 50**. It never breaks even at any row
count — the per-row cost falls as the header amortizes, but the floor is the
pipes themselves.

**And there is a correctness argument on top of the token one.** This server
indexes npm and pypi, where modern signatures are full of union pipes:

```
def loads(s: str | bytes | bytearray, *, cls: type[JSONDecoder] | None = None) -> Any
```

In a markdown table cell each `|` must be escaped, so the model reads
`str \| bytes` — **not the signature** — and echoes the corrupted form into
code or into the next tool call. For a server whose stated invariant is that
the signature is the primary identity, that is disqualifying.

**Decisions:**

- **Signatures never go in a table cell.** They go on their own indented
  continuation line: byte-exact, arbitrarily long, zero framing cost.
- **Fixed-schema rows: one line per record**, space-delimited, no header, no
  pipes. Field order lives in the tool description.
- **Caller-chosen columns (`graph`): TSV in a fence** with a one-line `cols=`
  header — the only place a header is justified, because the schema genuinely
  varies.
- **Markdown tables: never.** Most expensive at every N, and corrupts the one
  field that must not be corrupted.

### 6.3 The legend moves into the tool description

A ~366-token format legend in the `description` (prompt-cached with the tool
list) replaces ~40–60 tokens of framing per response. Break-even at ~7
responses; every response after that is pure payload.

### 6.4 Signals, not sentences

| signal | rendering | ~tok | means |
|---|---|---:|---|
| more rows | `next=c2Ynp` | 4 | pagination; data complete |
| index building | `~sem:building(62%)` | 8 | syntactic edges complete, semantic partial |
| index degraded | `idx:syn` | 3 | no semantic layer |
| field elided | `…+4213B` inline | 5 | re-read with `body=full` |

`~sem:building(62%)` must state **which edge classes are complete**, or the
agent cannot tell whether a missing caller is real.

Absence of `next=` means no more pages. **Never emit `next=none`** — absence is
free; a negative marker costs 4 tokens on every terminal response.

Truncation is signalled **at the point of truncation**, never as a
response-level flag that forces a re-read to locate.

### 6.5 Errors deserve tokens

An error costing 120 tokens that resolves in one retry beats a 20-token error
that costs three. Rules:

1. Carry the resolution **as data the model can paste back**, not as advice.
   `did-you-mean fn` beats "kind must be one of the valid values". A literal
   `retry search {q:"…", pkg:"…"}` line is an executable next call.
2. Ambiguity errors list **all** candidates with disambiguated addresses, so
   the retry cannot ambiguate again.
3. Distinguish **malformed** from **well-formed but absent**, and for the
   latter say *why* (generation change), so the agent re-searches rather than
   re-checking its transcription.
4. Print the full enum list — 12 kind names ≈ 14 tokens, all single-token
   words. Never "see documentation".

The existing `data{kind, help}` structure (§2.10.9) already does much of this
and should be kept.

---

## 7. Alternatives considered and rejected

### 7.1 Tool surface

**One polymorphic `nudox(op, args)` dispatch tool.** Saves only JSON envelope
boilerplate if per-op docs stay in the description (~15–25%), or ~2,000 tokens
if they move behind a `help` call — at the cost of a round trip and a new
failure mode (calling an op without reading help). Also loses argument
validation (the union degrades to `args: object`), tool-name-based permission
allowlisting in the host harness, per-tool telemetry, and the tool name as a
retrieval cue. **Rejected**: optimizes the number that does not matter.

**Navigation-as-a-graph** — one `explore(from, edge, hops)` with a ~14-edge
vocabulary subsuming `find_usages`/`get_occurrences`/most `graph_query` use.
Genuinely attractive: replaces a 15k SDL with a ~60-token vocabulary for the
common case and makes multi-hop one call. **Partially adopted** as `refs`
with a `direction` argument; the full edge-enum version is a good follow-up
once `schema` has shrunk and the addressing work has landed.

**Query DSL** (`kind:trait pkg:cargo/serde retry`). Strong pretraining prior;
models write GitHub-style filter syntax correctly more reliably than they
populate nested schemas. **Deferred, not rejected** — it needs a strict parser
that errors with did-you-mean on unknown filters, because silently ignoring
`lang:` (which models will invent) converts a typo into a wrong answer.

**Intent tools** (`how_do_i_use`, `who_breaks_if_i_change`). Highest ceiling on
answer quality, highest maintenance cost, and they need escape hatches anyway
so the tool count goes *up*. **Rejected for now**; revisit by adding two or
three once the core is stable.

**Corpus-as-filesystem** (`ls`/`read`/`grep` over a virtual tree). Excellent
addressing idea, wrong global metaphor — real languages do not form a tree
(overloads, multiple impls, macro-generated items), so disambiguators reappear
at the leaves. **Path idea adopted (§4), framing rejected.**

**REPL / `eval(expr)` pipelines.** Best round-trip economics of anything
considered; unacceptable learnability cost and it resurrects session state.
**Rejected.**

### 7.2 Addressing

**Prefix-truncated hashes.** Birthday bound for N = 446,947:

| hex chars | bits | P(≥1 collision) |
|---:|---:|---:|
| 8 | 32 | ≈ 1.0 (certain) |
| 10 | 40 | 9.1% |
| 12 | 48 | 3.6×10⁻⁴ |
| 16 | 64 | 5.4×10⁻⁹ |

Note that 8 hex — the intuitive "git short hash" — is **certain to collide** at
this corpus size. **Rejected as primary**: 12–16 hex buys ~55% of the token
cost while keeping 100% of the unreadability, zero composability, and zero
typo recovery.

**Adopted in a narrower and better form:** have the sealer compute a
**minimal collision-free prefix per package** (it holds every key at seal
time), giving zero collision probability *by construction* — empirically ~8–9
hex for a 50k-declaration package. Use it only for the unstable-tier suffix
where hex is unavoidable, cutting that penalty from ~28 tokens to ~5. This is
the highest-leverage follow-up and is independent of everything else.

If a fixed truncation is ever used instead, it must not ship before
**tombstones** — retaining deleted introhexes for the last M generations —
otherwise a deleted symbol's prefix can silently start resolving to a
different live symbol across a rebuild. That is the one path where this design
would violate the invariant the current parser was built to protect.

**Per-session interned handles (`s1`, `s7`).** Cheapest possible (~2 tokens)
and the worst identity: meaningless after context compaction, unusable across
parallel agents or in logs, and — unlike a hex prefix — structurally
uncheckable, so a hallucinated `s37` is indistinguishable from a valid handle
until lookup. **Rejected as primary.** Adopted narrowly as an integer `ref` on
result rows, valid only for the immediately preceding call.

**Content-addressed signatures.** Unstable under *any* edit (a renamed
parameter changes it), and it inverts the problem — you would need the
symbol's content to name the symbol you are fetching. **Rejected as an
address.** **Adopted as a sibling `content_digest` field**, which is the right
instrument for what the deliberately-RED stability test is actually trying to
detect: that test conflates "the code changed" with "the disambiguator moved",
and a digest separates them.

**SCIP / LSIF monikers.** Descriptor grammar already solves the kind-suffix
problem (`#` type, `.` term, `().` method). **Rejected as primary** — optimized
for machine emission, not for an agent to type, and its package coordinate
model does not match the ecosystem tags. **Adopted as an optional `scip`
interop field**, and its kind-suffix vocabulary is worth stealing if bracket
syntax proves unpopular.

**purl as the address coordinate.** **Rejected** precisely because purl says
`golang`/`clang` where keys say `go`/`cpp` — a purl-shaped address would not be
the key preimage, destroying the single-hash fast path. **Accepted on input**
with a normalization table and a test.

**Rendering the disambiguator at query time.** **Impossible, not rejected.**
blake3 is one-way; the skeleton bytes are hashed, not stored. Recorded here
explicitly because it is the most natural idea for anyone approaching the
problem fresh, and rediscovering that it cannot work costs a day.

**Making the alias-shortened MCP path the preimage.** Would give beautiful,
typeable, docs-matching addresses. **Rejected:** it changes every key in the
corpus (full re-seal, every stored key invalidated), and alias-shortened paths
are **not unique** — two re-exports can share one short path. It trades a
working identity scheme for a prettier non-identity.

**Inverting `memchr_reexport_aliases_are_invisible_to_name_search`** to make
re-export rows searchable. **Deferred, not rejected** — §4.10 achieves the
same user-visible outcome via `public_path` on the definition, with no
behaviour change and no test inversion. Only needed if the re-export
declaration must itself be findable as a distinct entity, which is a lesser
want carrying real regression risk.

**Status quo.** Real virtues: exactness, **totality**, zero migration, no
parser, no ranking function, every test green. Exactness and totality are
properties the new scheme must preserve — which is precisely why the hash is
never removed. Rejected only because it cannot distinguish "not indexed" from
"does not exist", forces a search round trip before any read, and cannot be
composed by an agent that just read a documentation page. Note the first of
those is fixed by §5.3 alone, with no addressing work at all.

---

## 8. Sequencing

Ordered by severity-per-unit-of-work. Each stage is independently shippable.

**Stage 0 — gate zero (§4.13).** Re-run the disambiguator census
post-2026-08-15, reporting `f_none` / `f_struct` / `f_unstable` **split by
ecosystem and kind**. Also: run `workspace/gui/tests/mcp_endpoint.rs` to
confirm the `structuredContent` staleness (§2.8); confirm parameter *order*
drives Param collisions (§4.6). Stages 8–9 must not be sized before this lands;
Stages 1–7 are safe without it by the closure property in §4.16.

**Stage 1 — typed statuses (§5.3).** Independent of everything else, fixes the
top-severity failure. No schema redesign required.

**Stage 2 — cheap deletions.** Remove dead `markdown.rs` (§2.4); delete the
constant `status: complete` line; drop the duplicated `to_key` in rekeyed diff
rows; fix the `graph_schema` "cost of each traversal" claim; document the
`golang`→`go` / `clang`→`cpp` mapping; document `limit: 0`.

**Stage 3 — `schema` card (§1.2).** Replace the 15,129-token SDL response with
the ~1,017-token card plus worked query examples. Largest single token win
available, low risk, no addressing dependency.

**Stage 4 — response envelope (§6).** Layout change, legend into descriptions,
signals not sentences. Keep `structuredContent` off.

**Stage 5 — tool consolidation (§5.1).** 13 → 8. `search` unification is
nearly free (§2.3). `select_version` deletion depends on Stage 6.

**Stage 6 — addressing, S0: resolution only, zero output change.** Address
parser; the four-stage resolver (§4.10); alias index inverted from existing
`Symbol.aliases`; `PathStyle::for_ecosystem`; **filter-side separator
normalization first** (§4.9); the preimage assertion. Every tool now *accepts*
an address wherever it accepts a key. **No existing test changes.**

**Stage 7 — addressing, S1: additive emission.** Results gain `address`,
`physical_path`, `public_path`, `key_tier`, `stability`, and a response-level
`scope` header. The 64-hex `id` stays present and stays the documented handle.
GraphQL adds `physicalPath`/`publicPath`; `Symbol.path` deprecated but
wire-unchanged for one release. **Token-neutral by design (§4.12)** — ship it
for the behaviour, not the budget.

**Stage 8 — addressing, S2: the cheap seal items (1, 2, 6, 7).** This is the
**60% identity-token cut**, and it is independent of the tier distribution.
Highest value-per-effort item in the document.

**Stage 9 — addressing, S3: the real project (seal items 3, 4, 5).**
Disambiguator rendering and sealer-verified `addressable`. Only now does the
primary handle move from hash to address, and only for certified declarations.
**Schedule against gate zero's cargo number** (§4.13).

**Stage 10 — version discipline.** `select_version` demoted to "default for
unversioned addresses only", then a per-call `version`, then delete the global
mutable selector.

**Separately, and worth its own case — `MemberIndex` promotion (§4.6).** Params
(13,601) and Fields (3,845) are up to **54%** of the ordinal population,
converted to Structural by one ladder change rather than by any addressing
work. It compounds with everything above: the address scheme's value scales
directly with the content-derived fraction of the corpus.

---

## 9. Implementation status and measured results (2026-08-15)

Measured with tiktoken over real responses captured by
`tests/mcp_dump_responses.rs`. Baseline captured from a **detached worktree at
HEAD**, so concurrent edits could not contaminate it. Both dumps go through the
same harness and the same `MarkdownResult` path.

```
--- FIXED SURFACE (resident before any tool call) ---
tools                                          13 ->     9
descriptions + arg schemas + instructions   3,693 -> 2,982   (−19.3%)

--- PER-RESPONSE (serde 1.0.196 + memchr 2.8.3) ---
case                     before    after    delta
graph_schema             14,872    1,175    −92.1%
search_memchr_20          1,391      797    −42.7%
search_deserialize_20     1,314      937    −28.7%
search_deserialize_50     3,270    2,419    −26.0%
get_symbols_8_source      1,180    1,180      0.0%
get_symbols_8_signature   1,180    1,180      0.0%
graph_query_20            1,390    1,390      0.0%
list_versions                42       42      0.0%
list_packages                23       23      0.0%
find_usages_30               10       10      0.0%
TOTAL                    24,672    9,153    −62.9%

combined (surface + responses)  28,365 -> 12,135  (−57.2%)
```

The change is not only cheaper, it is the first version an agent can actually
read. Two real `search("memchr")` rows, before:

```
| 1 | pub mod memchr | cargo:memchr#4f506f476f313c338390c3327ee4a2d7b0fe2512d880836e0352e06ef650b591 |
| 2 | pub mod memchr | cargo:memchr#511559962ba13ea1272ca98826ef7610882638902b0c10b84e4d1e62c5998624 |
```

and after:

```
cargo:memchr::arch::all::memchr[module]
  pub mod memchr
cargo:memchr::arch::aarch64::neon::memchr[module]
  pub mod memchr
```

Identical declaration text, separated in the baseline only by 64 characters of
hash; separated now by the thing that actually distinguishes them.

**A note on measuring the surface.** These figures extract descriptions, field
docs and instructions from **source**, the same way on both sides. Measuring
the "after" from a live `tools/list` response instead — which includes full
JSON-Schema structural overhead — and comparing it against a source-derived
"before" mixes methodologies and understates the delta (it yields ≈ −5.6%). Use
one method on both sides.

Reproduce: `scratchpad/proof.py <baseline_dump> <after_dump>`.

### 9.1 What shipped

* **Schema card** (§1.2) — `graph_schema` returns a ~1,150-token card by
  default; `full: true` and `nudox://schema` still serve byte-exact SDL.
  `schema_source.rs` untouched, LR-7 intact.
* **Address system** (§4) — `src/mcp/address.rs`: grammar parser/renderer,
  `PathStyle::for_ecosystem`, three-stage resolver, typed `ResolveOutcome`
  (including `PackageNotIndexed`, which is §2.6's fix). Wired into
  `SearchResult` and `CompactSymbolDoc`; `get_symbol`/`get_symbols` accept an
  address anywhere they accepted a key.
* **Search envelope** (§6.2) — one record per line, declaration indented
  beneath, no table.
* **Gate-zero census** (§4.13) — `seal-census` feature +
  `tests/rust/disambiguator_census.rs`.

* **Tool consolidation, 13 → 9** — `search` (lexical+semantic in one pass),
  `read`, `refs` (`direction: in|out`), `packages`, plus `diff`, `index`,
  `graph`, `schema`, `select_version`. Verified by a live `tools/list` probe,
  not by counting attributes. The pagination contract and key-stability caveat
  moved out of six and three descriptions respectively into `INSTRUCTIONS`,
  paid once. `select_version` was deliberately kept: folding it in needs
  per-call version support that does not exist yet (§8 Stage 10), and removing
  it would drop capability rather than consolidate it.
* **`refs` accepts an address or a key**, resolved once at the MCP layer before
  the Trustfall binding, so the graph adapter never learns about addresses.
  Non-`Resolved` outcomes surface as `AddressUnresolved` with candidates —
  never a silent empty result.
* **Crate-root elision** (§4.15) — by *verified* resolution, not a blanket
  rule: each candidate depth is checked through the real resolver and accepted
  only if it resolves back to the same intro. That matters precisely for
  memchr, which genuinely contains a module named `memchr` inside the crate
  `memchr`.
* **`keyTier` fixed** (§4.14) — `SealReport` gained `non_structural_keys`,
  populated from the *actual minted `Disambiguator`* at both mint sites rather
  than from the pass-2.5 escalation list. Sparse by construction: it records
  only the ~2.68% non-`Structural` set, so `PackageView`'s resident cost is
  unchanged from what `forced_keys` already cost.

### 9.2 What did not

* Re-export chain collapse (heuristic (a) of §4.10) — needs `RawRef` target
  resolution with no cheap accessor; falls through to honest `Ambiguous`.
* `get_symbols`' `format=source` vs `format=signature` still produce identical
  output for real serde symbols, with no source-materialization warning
  logged. Observed, not diagnosed.
* The 13 dead `format_*` wrappers in `result_format.rs` and the entirety of
  `markdown.rs` (§2.4) are still dead.

### 9.3 Defects found and fixed, each with a falsifiable guard

1. **Schema card named three fields that do not exist** — `SourceLocation`'s six
   positional fields were contracted to `byteStart/End`, `startLine/Column`,
   `endLine/Column`. Guard: `the_card_names_every_type_and_field_the_sdl_declares`,
   **verified red on the original defect before the fix was restored**.
2. **Whitespace-only path segment accepted** — `"cargo:serde@1.0.196:: "` parsed
   into a segment literally named `" "`, which hashes into the preimage and
   presents as "symbol does not exist" rather than "malformed address". The
   guard was `s.is_empty()`; it needed `s.trim().is_empty()`.
3. **`shortest_alias_module_path` dropped non-module ancestors** — on real serde
   it turned the associated type `serde::de::IntoDeserializer::Deserializer`
   into public path `serde::Deserializer`, which is the *trait*, a different
   declaration. Pre-existing; affects `get_symbol`'s displayed path, not only
   addresses. Guard: `alias_shortening_never_drops_a_non_module_ancestor`.
4. **The renderer emitted addresses its own parser rejected** — a Rust impl
   segment contains a lifetime apostrophe, the same character that delimits a
   quoted segment. The grammar specified `\'` escaping; no code implemented it.
   The escape-blind quote scan existed in **three** places and each needed the
   same fix. Guard: `a_quoted_segment_containing_an_apostrophe_round_trips`.
5. **Table cells corrupted both identity fields** — `escape_table_cell` doubles
   `\`, so a copied address no longer parses; it also escapes `|`, so
   Python/TypeScript union signatures are read and echoed wrong. Fixed by
   §6.2's record format, which needs no escaping at all.
6. **Two corpus tests had never run** — `serde` and `syn` could not lower from
   the read-only nix store (`build.rs` needs to write `Cargo.lock`). Fixed with
   a writable copy rather than `accept_missing_build_script_cfgs`, which the
   producer correctly warns would describe "a different crate".
7. **`keyTier` over-reports stability on 1,044 declarations** (§4.14). Found by
   the census; **not yet fixed**.

### 9.4 Two regressions caught only by measurement

* The first address wiring emitted the address **beside** the 64-hex key, so
  search responses grew (3,306 → 5,354 bytes). Collapsing to one identity
  column turned a regression into −22.6%.
* The schema-card work grew the fixed surface **+7.2%** by adding a 262-token
  `graph_schema` description that re-inventoried the card's own contents.
  Trimmed to neutral.

Both looked like improvements and were one measurement away from shipping as
costs. Note also that the naive estimator scored the *whole* change as −52.7%
against the real −61.2%, having earlier scored a single redesigned search row
as a **36% regression** where the real tokenizers showed a 40% improvement.

### 9.5 Pre-existing failures, unrelated

Both verified by running them in the **untouched detached worktree at HEAD**,
not by reasoning about them:

* `versions::timeline::tests::a_visibility_change_alone_is_reported_as_such` —
  `SignatureChanged` where `VisibilityChanged` is expected.
* `mcp_universal_producer_pipeline::real_rust_source_reaches_precise_usages_and_compact_batched_mcp`.

Everything else is green: `cargo test -p nudox-engine --lib --features fixtures`
is 436 passed / 1 failed (the timeline case above), and the MCP suites are
`mcp_schemas` 23, `mcp_tool_integration` 41, `mcp_address_adversarial` 13,
`mcp_schema_card_queries` 3, `mcp_token_budget_matrix` 1, `nudox-ir` lib 223 —
all passing.

## 10. Open questions

1. **Does per-call billing survive this?** §3. If not, the meter needs a new
   unit before Stage 5.
2. ~~Which path form becomes canonical?~~ **Answered (§4.9):** neither —
   they are two concepts sharing one name. `moniker_path` becomes
   `physical_path`; MCP's alias-shortening is promoted to `public_path`.
   What remains open is the deprecation window for `Symbol.path` and whether
   filter-side normalization fully covers existing `graph_query` callers.
3. **Is the ordinal share still ~7%, and what is it for cargo specifically?**
   Every estimate here scales with it, and §4.13 predicts cargo is materially
   worse than the global figure because every impl is `TraitImpl`.
4. **Does the Claude Code `structuredContent` precedence hold** for the pinned
   client version?
5. **Should `refs` grow into the full `explore(edge)` vocabulary** (§7.1) once
   `schema` has shrunk?
6. **Re-exports: Option A or B?** §4.10. Recommendation is A, but it is a real
   fork and should be decided deliberately, not by whichever gets implemented
   first. (Naming follows from it: `public_path` under A, `import_path` reads
   better under B, where the re-export is itself a thing.)
7. **What replaces the deliberately-RED stability test?** §7.2 suggests two
   green assertions (content digest for drift, tier for addressability) in
   place of one proxy that conflates them.
