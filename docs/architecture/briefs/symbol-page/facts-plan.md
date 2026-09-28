# W-Facts plan: carry docs.rs-grade facts from source to the symbol page

Status: PLAN, for review. No code is written. Every file:line below was read
in this pass. Where I relied on a read-only Sonnet agent, I say so, and I
spot-checked what the plan depends on.

---

## 0. The real path (it has two producers, not one)

The audit drew one chain, `lower/<lang>.rs → library Row → page_mapping → pages`.
There are two, and both reach the page:

1. **Semantic lane** (a compiler authority exists and a publication is complete):
   `crates/engine/src/driver/lower/<lang>.rs` → `FactSet` (`crates/engine/src/driver/lower.rs`)
   → IR image (`Ir` / `SemanticImageView`: per-entity `docs`, item `attributes`, 7 language
   extension planes; `crates/semantic/src/ir/reader.rs:181-194`)
   → `crates/local-service/src/builtin/view_build/semantic.rs` `append_image_rows` / `semantic_row_content`
   → `backend_library::Row` → `RowWire` → desktop.
2. **Structural lane** (tree-sitter tags; the only lane for workspace members, and for any language
   without a toolchain): `frontends/<lang>/lib.rs` tags + `crates/compile/src/syntax.rs`
   → `SourceDeclaration` (`syntax.rs:400-408`)
   → persisted `ProductSourceRecord` (`crates/engine/src/builtin/relation.rs`, formats `PSR8/PSR9/PSRA`)
   → `view_build/semantic.rs:249` `declaration_row` → `Row`.
3. **Semantic rows borrow from structural rows.** `semantic_sites` (`view_build/semantic.rs:~685-730`)
   pairs each semantic entity with the structural declaration of the same (name, family) in the
   same file bytes, by source order. A paired semantic row takes its **written signature,
   location and excerpt** from the structural declaration (`semantic.rs:~842-855`).
   Count mismatch → no pairing → encoded signature, no location.

`content_truth.rs` states it plainly: `crates/present` (a workspace member) gets the structural
projection only; `rich_project` (standalone) gets the compiler publication. So for the owner's
own repo, **every page is structural today**. Both lanes need every fact.

The page side: `Document` (the page's own docs, signature, site) is a projection of the row
(`crates/local-service/src/builtin/commands/adapter.rs:223-227`); members are outline child rows.

### Corrections to PARITY.md (verified)

| Audit claim | What is true |
|---|---|
| `java.rs:~3361` extracts `@Deprecated` and drops it | That is test code. It asserts `@java.lang.Deprecated` **is kept** as a raw atom in `JavaFacts.annotations` (`java.rs:1813-1818`), which also flows into the IR item `attributes` plane (`lower.rs:3213-3225`). Nothing *interprets* it. Arguments survive: the doclet interns `AnnotationMirror.toString()`. |
| `Arrival` is in `common.rs:~250` | It is in `apps/desktop/src/model/pages/symbol.rs:251-261`. Still never constructed except `NotReported` (`page_mapping.rs:623, 762, 802`). |
| Blanket impls over `&T` are lost by the engine | The Rust lowering **emits** them: an impl is named by its whole written self type (`rust.rs:774-795`), so `impl<'a, T> Serialize for &'a T` is a row named `&'a T`. The **desktop** drops it: `is_impl_block` needs a signature starting `nominal(` (`page_mapping.rs:713-719`). |
| Type-level where-clauses are lost | In the structural lane the written signature is cut at `{` (`syntax.rs:1083-1090`), so a type's `where` clause survives. The Rust lowering also captures type generics and where-clauses (`base_extension`/`where_rows`, agent-verified). What loses them on semantic pages is the **pairing collision** below. |
| (not in the audit) | **Semantic Rust type pages lose their written signature, location and excerpt whenever the type has an `impl` in the same file.** Impl entities map to `DeclarationKind::Type` (`view_build/identity.rs:73`), the same pairing family as `struct` (`identity.rs:40-46`). `Record Boxed` + `Implementation Boxed` ×2 against one structural `struct Boxed` is a count mismatch, so nothing pairs. `content_truth.rs:437-441` asserts the resulting `GapReason::Encoded` as expected. |
| (not in the audit) | The structural lane destroys Rust doc headings: `clean_comment` (`syntax.rs:1207-1221`) strips a leading `#` from every line, so `/// # Errors` arrives as `Errors`. |
| (not in the audit) | C# docs keep only `<summary>` in both lanes: `xml_documentation_text` (`syntax.rs:1233`) and the lowering's `summary_inner` (`csharp.rs:2358-2369`). `<exception>`, `<returns>`, `<remarks>`, `<example>` are dropped. |
| (not in the audit) | Undocumented structural rows carry a fabricated document, `"{kind} in {path}:{line}"` (`view_build/semantic.rs:262-268`; again at `view_build/query.rs:439`). Full member docs would put that string on every undocumented member. |
| (not in the audit) | Local indexing always uses **all Rust features**: `crates/engine/src/application/host/authority.rs:196` hardcodes `all_features: true`, the only production construction site. A `cfg(feature = …)` item is therefore indexed but unbadged. A `cfg(not(feature = …))` item is excluded. |

---

## 1. Hash, serialization and version map (what a new fact touches)

| Layer | Where | Rule I will follow |
|---|---|---|
| IR entity identity / payload | `crates/engine/src/driver/lower/identity.rs:966-990, 1430-1436` | Identity and payload **exclude** docs, visibility, attributes and extensions (except Function parameter conventions and Implementation trait/generic/member frames). Adding atoms to the item `attributes` plane changes **image bytes** (so `image_identity` and external-target row keys, `semantic.rs:~870-890`), and changes **no** declaration identity. That is pure content addressing. No IR format change. |
| IR image format | `crates/semantic/src/ir/semantic_image/**` | **Untouched.** I reuse the existing per-item `attributes` plane (`TreeItemInput.attributes`, `tree.rs:20`) and its availability bit (`EntityAuthorityFacts.attributes`, `authority.rs:95`; set at `lower.rs:1169`). I add no plane, no extension field and no doc-fragment variant. |
| Structural extraction | `frontends/<lang>/lib.rs` contract bytes `…/tags-vN` (`SyntaxFrontend::new`, `syntax.rs:640-657`: "explicit, bump-on-change contract") | Bump **all seven** contracts, because extraction output changes (new facts, and a `clean_comment` fix that changes persisted doc text). This is the reuse fence. Stale cached analyses must not be served under the old key. |
| Persisted source record | `crates/engine/src/builtin/relation.rs:20-60` (`PSR8/9/A`, "minimal tag" rule), `decode_declaration` `:956-995`, `shed_prose` `:649-670` | New tag `PSRB` (version 11), chosen **only** when some declaration states a fact, so every existing record re-encodes byte-identically (restart admission refuses anything else). Decoding stays liberal: older tags decode to "not observed". `shed_prose` drops facts (they become not-observed, never "absent"). |
| View row canonical bytes | `crates/library/canonical/encoding.rs:82-172` | Append-only suffix, same precedent as `identity_preimage` (`:163-170`). A new tag byte `2` precedes the facts, emitted **only when the row observed something**. Rows without facts keep byte-identical canonical bytes, so their view roots do not move. Rows with facts change their bytes, which is the deliberate content change. |
| Row wire | `crates/library/wire/reply.rs:159-173` (`deny_unknown_fields`), `row_to_wire :755`, both `row_from_wire*` paths `:776, :874` | `#[serde(default, skip_serializing_if = …)] facts`. Also `DocumentWire` (`reply_content.rs:13-22`). Size accounting: `wire/admission.rs:394-408`, `view/root.rs` `row_encoded_size`, `local-service/src/builtin.rs:602`, and the `remaining_bytes` budgets in `view_build/semantic.rs`. |
| DTO version | `crates/library/wire/mod.rs:65` `DTO_VERSION = 7` | Bump to 8. Precedent: commit `c5d9f5015` bumped 6→7 for additive reply fields. An old peer then refuses cleanly instead of failing on `unknown field "facts"`. The view journal has its own `VERSION` (`view_journal.rs:397, 587`). Old journal rows decode (serde default). I will prove a pre-change workspace reopens (§5). |
| `DeclarationKind` | `crates/compile/src/syntax.rs:57-99`; wire tag in canonical rows (`encoding.rs:125-130`), name on the wire | Phase 2 adds `Implementation = 19` (name `"implementation"`). Unknown names already fall back to `Unknown` on read. Impl rows change kind tag, which is deliberate. |
| Goldens and pins | searched: no 64-hex digest pins in engine/semantic/library/local-service/desktop tests. `crates/engine/tests/fixtures/csharp_render/fidelity.txt` renders the IR **without attributes**. `rust_render_golden.rs` pins signature and doc strings. `content_truth.rs:437-441` pins the Boxed `Encoded` gap. | Expected golden changes: **only** `content_truth.rs:437-441`, in Phase 2. The pairing fix makes Boxed's signature the written `pub struct Boxed`. I will flip that assertion deliberately and give the reason in the hunk. If any other pinned value moves, I stop and report it rather than loosen it. |

Invariant I will add as its own test: **a row with no facts encodes to exactly the bytes it had before this change** (a golden of `encode_row` for a representative structural row and a semantic row, captured before the first edit).

---

## 2. The carrier (one type, both lanes)

It lives in `backend_compile`, because `SourceDeclaration` needs it and `backend_library` already
re-exports compile vocabulary (`DeclarationKind`, `SourceDeclaration`). New file
`crates/compile/src/facts.rs`:

```rust
/// What a producer observed about one declaration beyond its name, kind and signature.
/// A fact the producer did not look for is unobserved, never "absent".
pub struct DeclarationFacts {
    observed: Observed,                  // bitset: DEPRECATION | OBLIGATION | GATES | DERIVES | IMPLEMENTATION
    pub deprecation: Option<Deprecation>,
    pub obligation: Option<Obligation>,
    pub gates: Box<[Box<str>]>,          // verbatim cfg predicates on this item, outermost attribute first
    pub derives: Box<[Box<str>]>,        // Phase 2: `#[derive(..)]` paths as written
    pub implementation: Option<ImplementationFacts>, // Phase 2
}
pub struct Deprecation { pub since: Option<Box<str>>, pub note: Option<Box<str>> }
/// What an implementor of the enclosing contract owes for this member.
pub enum Obligation {
    Required,   // must be written: Rust no body, Java/C#/TS abstract, Python @abstractmethod, Go interface method, C++ `= 0`
    Optional,   // may be omitted: TS `m?()` / `p?: T` in an interface
    Provided,   // comes with an implementation: Rust default body, Java `default`, C# DIM / virtual, C++ virtual with body, ABC concrete method
}
pub struct ImplementationFacts {        // Phase 2
    pub self_type: Box<str>,             // as written
    pub contract: Option<Box<str>>,      // as written, with its arguments: `From<&'a str>`
    pub blanket: bool,                   // the self type is (a reference to) one of the impl's own type parameters
}
```

Bounded text (`SourceDeclaration::MAX_TEXT_BYTES`), with private constructors that admit and
validate. `Known` is rebuilt from `observed` at the page: an unobserved fact becomes
`Known::Unknown(Gap { NotCaptured, "<lane> does not read <fact> for <language>" })`.

Why interpreted facts on the row, not raw attributes: the page, CLI and MCP all need
"deprecated since X: note", and must not each re-parse seven attribute grammars. The raw spellings
stay in the IR `attributes` plane for anyone who needs more later (`#[non_exhaustive]`, `#[repr]`).
One shared interpreter, `backend_compile::facts::interpret(language, attribute_text)`, is called by
both producers, so the two lanes cannot disagree on parsing.

---

## 3. Phase 1: cheap and universal (deprecation, obligation, member docs, doc sections)

### F1. Deprecation, rank 1 (every language, high reader value, low cost)

Source per language, and whether each lane sees it **today**:

| Language | Written as | Structural lane (tree-sitter) | Semantic lowering |
|---|---|---|---|
| Rust | `#[deprecated]`, `#[deprecated = "n"]`, `#[deprecated(since, note)]` | `attribute_item` siblings are already walked and skipped (`preceding_comments`, `syntax.rs:1117-1135`; `decorates_a_declaration`). **Sees it, drops it.** | **Not read** (agent-verified: no `deprecated`/`AttrsWithOwner` use in `rust.rs`; `extension_item_attributes` returns `None` for Rust, `lower.rs:3222`). Needs staging (below). |
| Java | `@Deprecated(since="9", forRemoval=true)` | `modifiers` holds `marker_annotation`/`annotation`. Visible. | **Kept raw** in `JavaFacts.annotations` and so in IR item attributes (`java.rs:1813-1818`). Only interpretation is missing. |
| C# | `[Obsolete("msg", true)]` | `attribute_list` sibling. Visible. | **Kept raw with arguments** in `CSharpFacts.attributes` (`csharp.rs:2036-2057`; lowering test `csharp.rs:4187-4217` asserts `Obsolete("use New")`). Interpretation missing. Fixture `fidelity.cs:19` already has `[Obsolete("legacy", true)]`. |
| Python | `@deprecated("m")`, `@typing_extensions.deprecated`, `@warnings.deprecated` | `decorator` siblings. Visible. | **Kept raw with arguments**: each decorator is "the decorator range minus its `@`" (`python.rs:1162-1171`), interned into `PythonFacts.decorators` and so into IR item attributes (`lower.rs:3219`). Interpretation missing. |
| TypeScript | JSDoc `@deprecated msg` | In documentation text (JSDoc lines survive `clean_comment`). | **Kept as text**: `pass_docs` emits each JSDoc line as `Text`, splitting only inline `{@link}`/`{@code}` (`typescript.rs:5136-5250`). Block tags such as `@deprecated`/`@throws` arrive verbatim. Interpretation missing. |
| Go | `// Deprecated: msg` paragraph | In documentation text. | Doc kept verbatim, line per `Text` fragment (`go.rs:3389-3411`). No flag. |
| C/C++ | `[[deprecated("m")]]`, `__attribute__((deprecated))` | `attribute_declaration` node. Visible for `[[…]]`. `__attribute__` is an `attribute_specifier`. | **Not modeled anywhere.** `DeclarationFact` has no attribute field (`frontends/clang/src/legacy/facts.rs:234-261`). Leave the semantic side `None` (unobserved). The structural site supplies it through pairing (below). |

Changes:
- `crates/compile/src/facts.rs` (new): types plus the `interpret` functions for the seven spellings
  (attribute text → `Deprecation`), and `doc_deprecation(language, doc)` for Go `Deprecated:` and JSDoc `@deprecated`.
- `crates/compile/src/syntax.rs`: `SourceDeclaration` gains `facts: DeclarationFacts` (`:400-408`, builder
  `with_facts`, accessor `facts()`). `declarations_of` (`:748-810`) calls a new
  `declaration_facts(language, definition.node, text)`, which reuses the sibling walk that
  `preceding_comments` already does (factor the shared walk into one iterator, not a second copy).
- `frontends/*/lib.rs`: bump the seven `tags-vN` contracts.
- `crates/engine/src/builtin/relation.rs`: `PSRB` encode/decode of facts after the container; minimal-tag selection; `shed_prose` → unobserved.
- **Rust semantic staging** (`crates/engine/src/driver/lower.rs`, `rust.rs`): add
  `FactSet::attach_item_attributes(ordinal, AtomListId)`, a generic item-attribute staging lane
  independent of the extension struct. `build_ir` (`lower.rs:~2848-2860`) takes
  `item_attributes` from it when present, otherwise from `extension_item_attributes` as today.
  `rust.rs` interns the verbatim text of each `deprecated`, `cfg`, `cfg_attr`, `derive` attribute on
  the declaration's own syntax (`ast::HasAttrs::attrs()`; attribute nodes sit inside the declaration
  span, agent-verified) and attaches it. Expanded declarations stage nothing. No `RustFacts` change,
  so the extension plane layout is unchanged.
- `crates/local-service/src/builtin/view_build/semantic.rs`: `semantic_row_content` reads
  `entity.attributes` (plus the availability bit) and the docs, and calls the same interpreter.
  **Merge rule:** for each fact kind, the semantic lane's observation wins when it observed
  (attributes captured, or docs captured for Go/TS). Otherwise the paired structural site's fact is
  used. Otherwise the fact is unobserved. `declaration_row` (`:249`) copies `declaration.facts()`.
- `crates/library/view/model.rs`: `Row.facts: DeclarationFacts` (`:659-693`), `Row::with_facts`,
  and `Document.facts` (`:374-393`). `adapter.rs:223-227` copies it. Canonical encoding and wire as §1.
- Desktop (`apps/desktop/src/model/pages/common.rs`): `DeclRef.deprecation: Known<Option<Deprecation>>`,
  so that **every reference** (member rows, relations, outline siblings) can strike a deprecated name,
  as docs.rs does. It is filled in `DeclRef::from_row` (`common.rs:428-438`). `SymbolPage` reads the
  page's own from `Document.facts`.

### F2. Required / optional / provided, rank 2 (contracts in six languages)

| Language | Structural lane (the AST, not "ends in `;`") | Semantic lowering today |
|---|---|---|
| Rust | In a `trait_item`: `function_signature_item` → Required; `function_item` → Provided; `associated_type` without default → Required; trait `const_item` without a value → Required, with one → Provided. The tags query already captures `function_signature_item` (`frontends/rust/lib.rs:28`). | Not captured. `Function::has_body(db)` is reachable (`ra_ap_hir-0.0.341/src/lib.rs:2620`). There is no IR plane to carry it (see below). |
| Java | Member of `interface_body`: `default` modifier → Provided; `static`/`private` → none; else Required. Class: `abstract` → Required. | **The authority image has the bits** (`frontends/java/src/legacy/image.rs:934-960`, doclet `ABSTRACT`/`DEFAULT`, `AuthorityImage.java:535-550`). The lowering drops them on purpose (`java.rs:19-21`, "Modifiers stay out of the lane"). |
| C# | Interface member with a body → Provided; without → Required; `static` non-abstract → none. Class: `abstract` → Required; `virtual` → Provided. | **Not in the authority image** (`AuthorityImage.cs` has no modifiers). Needs a .NET producer change plus regenerated `.ncaimg` fixtures. Deferred. |
| TypeScript | Interface `method_signature`/`property_signature` → Required, with `?` → Optional. Class `abstract_method_signature` / `abstract` field → Required. Concrete methods of an `abstract_class_declaration` → Provided. Stock tags already capture `method_signature` and `abstract_method_signature`. | Optional-ness is lowered into member *types* (`typescript.rs:2912, 3007`); `abstract` is not read (no `abstract` match in `typescript.rs`). Semantic rows use the structural site. |
| Python | In a class whose bases name `ABC`, `abc.ABC`, `Protocol`, `typing.Protocol`, or with `metaclass=ABCMeta`: `@abstractmethod`/`@abc.abstractmethod` → Required; else Provided. | **Required is exact from the IR**: `abstractmethod` is in the raw decorators (above). "Provided" needs the class to be an ABC/Protocol, which is a base-list fact. `view_build` takes Required from the IR and the rest from the structural site. |
| Go | `method_elem` in an interface (captured, `frontends/go/lib.rs:25`) → Required. | Interface methods are `Function` entities parented to a `Trait` (`go.rs:1886-1924`). Required follows from the parent kind. |
| C++ | `field_declaration` with `pure_virtual_clause` → Required; `virtual` with a body → Provided. | **The authority has `MethodVirtuality::PureVirtual`** (`frontends/clang/src/legacy/facts.rs:221-242`). The lowering never reads it (`clang.rs:414` is the only mention, a placeholder). |

Plan: the **structural lane computes obligation for all seven languages** from the tree-sitter AST
(node kinds plus modifiers, as in the table). **Semantic rows inherit it through the existing site
pairing**, like the written signature. For Go it is derived in `view_build` from the IR parent
kind, so it survives a failed pairing.

This does not "do it properly from the lowered AST" for the semantic lane. Obligation is not an
attribute, and the IR has no plane for it. Two lowerings (Java, C++) *hold* the fact and drop it; a
third (C#) needs a producer change. The right carrier is a small per-entity flags column in the IR
image, which is a real image-format change (builder, writer, reopen validation, wire-attack tests).
Structural pairing gives a correct answer on every paired row now. I list the IR column as **F2b,
deferred** (§6). **I want the lead's call on this trade.**

Changes: `facts.rs` (`Obligation`), `syntax.rs` (`declaration_facts` computes it from the
`definition.node` and its container node), and `view_build/semantic.rs` (Go rule and merge).
Desktop: `Member.obligation: Known<Option<Obligation>>` (`symbol.rs:190-197`), set in `member_of`
(`page_mapping.rs:503-512`).

### F3. Full member docs, rank 3 (all languages, desktop-only data path)

Every member row already carries its whole `document` (`Row.document`). `summary_of`
(`page_mapping.rs:494-501`) keeps only line 1. No wire change is needed.
- `Member.docs: Arc<[DocFragment]>` (`symbol.rs:190-197`), filled with `doc_fragments(&row.document, outline)` (`page_mapping.rs:365`). `summary` stays as it is, for the ledger line.
- **Producer honesty fix, required first:** structural `declaration_row` emits an **empty** document
  when the declaration has no documentation, instead of `"{kind} in {path}:{line}"`
  (`view_build/semantic.rs:262-268`; same fix at `query.rs:439`). File-module rows keep their label.
  This changes canonical bytes for undocumented structural rows, deliberately. I checked: no test
  pins that string. Prose does feed lexical search (`crates/library/arrangement/schema.rs:252-273`),
  but the placeholder adds only the kind word and the path, and the label already carries the path.
  I will confirm with `content_truth`'s search assertions.
- Doc text quality in the lanes (the same fixes serve F4):
  - `clean_comment` (`syntax.rs:1207-1221`) strips the comment marker only (`///`, `//!`, `//`, `/*`, `*`, and `#` only for `#`-comment grammars). Rust `# Errors` survives.
  - C# `xml_documentation_text` (`syntax.rs:1233`) keeps `<returns>`, `<exception cref>`, `<remarks>`, `<example>` as tagged lines (`Returns: …`, `Throws T: …`) instead of dropping every element but `<summary>`. The C# **lowering**'s `summary_inner` has the same loss. Fixing it there is a lowering change with no format change, and I include it (it is the same parse).

### F4. Doc sections, rank 4 (all languages, zero hash impact)

Detected at presentation, over fragments that are already hashed. It is a pure function of
(language, fragments), so no new stored fact is needed. It lives in `backend_present`, the
presentation crate shared by CLI, MCP and desktop (`crates/present/page.rs` already folds
`Fragment`s into `Prose`).

- `crates/present/sections.rs` (new): `sections(language, &[Fragment]) -> Vec<Block>`, one
  generic model:
  `SectionKind { Errors, Panics, Safety, Examples, Returns, Parameters, Deprecated, Other }`,
  plus a title as written, plus tag-style **entries** (`subject`, `body`) for `@throws T …`,
  `Raises:\n  T: …`, NumPy `Raises\n------`, `<exception cref="T">`, and `@param`.
  | Language | Recognized |
  |---|---|
  | Rust | markdown `# Errors`, `# Panics`, `# Safety`, `# Examples`, any other `#`/`##` heading → Other |
  | Python | Google `Raises:`/`Returns:`/`Args:`/`Examples:`; NumPy underlined headers |
  | TS/JS, Java, C/C++ (Doxygen) | `@throws`/`@exception`/`\throws`, `@returns`/`@return`, `@param`, `@example`, `@deprecated` |
  | C# | the tagged lines produced by the F3 XML fix |
  | Go | `Deprecated:` paragraph; Go has no other convention |
- Desktop: `DocFragment::Section { kind, title }` and `DocFragment::Entry { subject }` markers
  (`symbol.rs:95-112`), emitted by `doc_fragments` when it knows the language (the page's and each
  member's `DeclRef.language`). Existing renderers that ignore the markers still render the text in
  order. W-Shell styles them later. I do not edit `bodies/symbol.rs`.
- The alternative I rejected: a producer-side `Fragment::Section` on the wire. That costs a wire
  change, a canonical tag and an IR `DocFragment` variant (an image format change), and buys nothing
  that a deterministic parse of the same bytes does not.

---

## 4. Phase 2: Rust-specific (gates, impl rows, arrival, feature set)

### R1. First-class impl rows, plus the pairing fix, rank 5 (largest Rust win per line changed)
- `DeclarationKind::Implementation = 19` (`syntax.rs:57-99`, `name()`, `from_wire_tag`, `from_name`).
  Desktop: `KindFamily::of` puts it under Type (`common.rs:331-358`).
- `view_build/identity.rs:73`: `ItemKind::Implementation → DeclarationKind::Implementation`, and its
  **own pairing family** (`declaration_family`, `:37-57`). This alone restores the written signature,
  location and excerpt on every semantic Rust type that has an impl in its file (`content_truth.rs:437-441` flips).
- Structural lane: add `(impl_item type: (_) @name) @definition.implementation` (`frontends/rust/lib.rs:20-30`).
  The name is the whole written self type, the lowering's own naming rule (`rust.rs:774-795`), so the
  two lanes pair. The written signature is the header up to `{`, which already includes generics, the
  trait **with its arguments**, and a multi-line `where` clause (`syntax.rs:1083-1090`). Docs on the
  impl block come with it.
  **Containment, a decision for the lead:** `containment.rs` makes "the nearest *captured* enclosing
  definition" the parent (`crates/compile/src/containment.rs:10-16`). An uncaptured `impl_item`
  instead attaches its methods to the self type by name (`:145`). Capturing `impl_item` therefore
  re-parents every Rust impl method under its impl row. That is what the semantic lane **already
  does** (span containment, agent-verified), so the two lanes would finally agree. It would also
  empty the type page's `does` ledger unless `page_mapping` gathers methods through the type's impl
  rows, and I would land both in the same change. The alternative keeps methods attached to the
  type and gives each method a back-reference to its impl row. The lanes then keep disagreeing.
  **I recommend re-parenting**, with the page gathering by impl (see below).
- `ImplementationFacts { self_type, contract, blanket }` from tree-sitter fields (`type`, `trait`,
  `type_parameters`). Semantic impl rows inherit it through pairing.
- Desktop `page_mapping.rs`: `is_impl_block` (`:713-719`) becomes `kind == Implementation`, which
  admits `&T`/`T` self types. `derive_impl_blocks` (`:725-807`) uses `ImplementationFacts` instead of
  the leaf-name sniff. The type page's `does` ledger groups methods by impl block, titled by the
  written header (`Members` gains `impls: Arc<[ImplGroup { header: DeclRef + SignatureText, members }]>`).
  This is the "grouping by condition" D-Page asked for.

### R2. Arrival, rank 6
- `Arrival` (`symbol.rs:251-261`) gains `Derived`.
- **Direct**: the relation came through an impl row that is not blanket and whose self type names the centre.
- **Blanket**: `ImplementationFacts.blanket`. It shows on the trait's `implemented_by`. A type page
  cannot list blankets that apply to it without a trait-solver query; that is R2b, deferred.
- **Derived**: the centre's `derives` names the trait leaf. `derives` comes from `#[derive(..)]`
  (structural sibling walk, and the Rust staging in F1). The semantic lowering can confirm with
  `Impl` `AnyImplId::BuiltinDeriveImplId` (`ra_ap_hir lib.rs:~4830`); that confirmation is optional and
  deferred.
- **Auto**: deferred (§6).
- The joint for impl-derived relations comes free: `Relation.via` already holds the impl `DeclRef`,
  whose path and line become real once R1 lands.

### R3. cfg gates, rank 7
- `DeclarationFacts.gates`: verbatim predicate text of each `#[cfg(p)]` on the item, and of
  `#[cfg_attr(docsrs, doc(cfg(p)))]`. Structural: the `attribute_item` walk. Semantic: the F1 Rust
  staging (`cfg`/`cfg_attr` text), interpreted by the shared interpreter. Inherited gates (an item
  inside a `#[cfg] mod`) are composed on the desktop from the outline ancestors, not duplicated per row.
- Page: `SymbolPage.gates: Known<Arc<[Gate { predicate, from: Option<DeclRef> }]>>`, own plus ancestors'.
- C/C++ `#if`: out of scope. Go `//go:build`: the IR carries it only for shadowed variants
  (`go.rs:2028-2094`), so it is not a general gate. Out of scope, noted.

### R4. Which feature set the index used, rank 8
- The fact is already fixed: local Rust indexing is `all_features: true` (`host/authority.rs:196`).
  The runtime hashes the policy (`application/runtime.rs:344-372`, `package_authority_fingerprint`)
  but never publishes it. Plan: report the Rust feature policy on the existing local compiler
  capability row, so the package dossier can say "indexed with every feature on". Per-row gates then
  distinguish "gated, and indexed" from a declaration that is absent.
- **The honest caveat for the page:** under all-features, a `cfg(not(feature = "x"))` branch is
  *not* indexed. The page can say "indexed with all features", and must never claim that an absent
  item does not exist.

### R5. Type-level bounds and where-clauses, rank 9 (mostly free after R1)
Written signatures already carry inline bounds and `where` clauses (both lanes, see §0). R1's pairing
fix is what makes semantic type pages show them. The only remaining gap is structured bounds for
hover. `RustFacts.where_clauses`/`free_predicates` exist in the IR but are rendered only into the
hidden canonical document. Surfacing them structurally is **deferred**; the written text serves the
reader now.

---

## 5. Tests (rendered content, never counts) and crates

Every fact is asserted as **text**: the note string, the `since` string, the predicate, the header,
the section body.

**Seam tests (fast):**
1. `backend-compile`, `crates/compile/src/tests.rs`: real source per language through
   `SyntaxFrontend::analyze` (Rust `#[deprecated(since = "1.2.0", note = "use `fresh`")]` → since `1.2.0`,
   note `` use `fresh` ``; Java `@Deprecated(since="9")`; C# `[Obsolete("legacy", true)]`; Python
   `@deprecated("use g")`; TS `@deprecated use g`; Go `Deprecated: use G.`; C++ `[[deprecated("use g")]]`),
   obligation per language, cfg text `feature = "serde"`, and a Rust `# Errors` heading surviving `clean_comment`.
2. `backend-engine`, `crates/engine/src/builtin/relation.rs` tests: a PSR round trip that carries the
   deprecation note **text**. A record with no facts re-encodes **byte-identical** to its PSRA/PSR9/PSR8 bytes.
3. `backend-engine`, Rust lowering: extend `crates/engine/tests/rust_render_golden.rs`, or the
   `rust_semantic_lane.rs` style, with a fixture that has `#[deprecated(...)]` and `#[cfg(feature = "x")]`.
   Assert that the IR item attributes contain those exact spellings, and the availability bit is captured.
4. `backend-library`: canonical `encode_row` golden for a no-facts row (captured **before** the first
   edit), a round trip `row_to_wire → row_from_wire` that preserves the note/since/predicate text, and
   `Document` likewise.
5. `backend-local-service`: `view_build` tests that a semantic Java/C# row gets its deprecation note
   from IR attributes, that the merge rule prefers the semantic observation, and that structural rows
   carry `SourceDeclaration` facts and an empty document when undocumented.
6. `backend-present`: `sections` over real doc text per convention, asserting section kinds, titles,
   entry subjects and bodies.
7. `backend-desktop` `page_mapping` tests (`page_mapping.rs:1785+`): `Member.docs` keeps paragraph 2
   (the class of loss in memory: count-based tests cannot see field loss), `DeclRef.deprecation`
   note text, `Member.obligation`, `Section` markers, `Arrival::Blanket/Derived/Direct` on real
   impl-row shapes, and gates composed from ancestors.

**End-to-end (real source → real lowering → real owner → desktop page model):**
8. `apps/desktop/tests/content_truth.rs` (Rust, both lanes). Add to `rich_project` (semantic lane):
   a deprecated fn, a trait with one required and one provided method, a `#[cfg(feature = "extra")]`
   item (with the feature declared in its `Cargo.toml`), a two-paragraph member doc with `# Errors`,
   `#[derive(Clone)]`, and a blanket impl. Assert on the page: the note text, `Required`/`Provided`
   by method name, the predicate `feature = "extra"`, the Errors body, `Arrival::Derived` for Clone,
   and `Arrival::Blanket`. For the structural lane, assert the same on a structural fixture.
   *Open: `content_truth` indexes `crates/present`, which has none of these. I propose a second small
   workspace-member fixture rather than editing `crates/present`'s real code.*
9. **Non-Rust end-to-end:** a new `apps/desktop/tests/facts_polyglot.rs`, following `content_truth`'s
   harness, over a fixture with one file per language (as `tests/journeys/fixtures/polyglot` does).
   Assertions are **lane-agnostic**: the fact's text must arrive whichever lane answers. The test
   records which lane answered (`DeclRef.semantic`), so a skipped toolchain cannot pass silently.

**Mutation proof** (one Bash command each, with `trap` restore and `touch`, two identical runs):
drop the interpreter's `note` → test 1/8 panics quoting the missing note; `shed` facts in `row_to_wire`
→ test 4 panics; revert `summary_of`-only → test 7 panics on paragraph 2; drop the impl family →
`content_truth` panics on the Encoded signature.

**Crates built and tested with `-p`** (single root workspace, `apps/*` are members, so it is one
build world), via `.local/devenv/cargo`: `backend-compile`, `backend-frontend-{rust,java,csharp,python,typescript,go,clang}`
(check only), `backend-engine`, `backend-library`, `backend-local-service`, `backend-present`,
`backend-desktop`, and `backend-cli`/`backend-mcp` (check only, since they consume `Row`/`Document`).
No `--workspace`, no `cargo clean`. At most two cargo processes at once.

---

## 6. Ranked summary and what I defer

| Rank | Item | Value | Cost | Phase |
|---|---|---|---|---|
| 1 | F1 deprecation, 7 languages | high, universal | S-M (one carrier, both lanes) | 1 |
| 2 | F2 obligation (structural AST, inherited by semantic) | high on contracts | S | 1 |
| 3 | F3 member docs, plus producer honesty and doc-text fixes | high | S | 1 |
| 4 | F4 doc sections (presentation parse) | high | S | 1 |
| 5 | R1 impl rows, plus the pairing fix | very high for Rust semantic pages | M | 2 |
| 6 | R2 arrival: Direct, Blanket, Derived | medium | S after R1 | 2 |
| 7 | R3 cfg gates | medium-high for crates.io deps | S | 2 |
| 8 | R4 feature policy reported | medium (honesty) | S | 2 |
| 9 | R5 type bounds (free via R1) | medium | none beyond R1 | 2 |

**Deferred, each with its reason:**
- **F2b, obligation in the IR** (Java modifier bits, C++ `PureVirtual`, Rust `has_body`, C# producer).
  Needs an IR image column, which is a format change. Structural pairing covers paired rows until then.
- **C# `Obsolete` and obligation in the .NET producer image.** `Obsolete` is already covered (attributes keep their arguments); obligation needs `AuthorityImage.cs` plus regenerated fixtures.
- **C++ deprecation in the semantic lane.** libclang attributes are not modeled (`facts.rs:234-261`). The structural site covers `[[deprecated]]`.
- **Auto traits** (Send/Sync/Unpin/UnwindSafe/RefUnwindSafe/Freeze). `Type::impls_trait` is reachable
  (`ra_ap_hir lib.rs:5827`), but it costs six solver queries per nominal type, and it needs a carrier
  for *negative* facts ("not UnwindSafe", which D-Page wants). That is an IR plane, so later.
- **Blankets that apply to a type** (R2b). Needs the trait solver per type × blanket impl.
- **The joint per relation**, beyond impl-derived relations. `GraphRelation` is `Copy`/`Hash` and deduplicated
  (`semantic_query.rs:428-527`). The spans are in the image's link occurrences. The design is a
  sidecar `(GraphRelation, span)` list on the related reply, filled from `LinkOccurrence.source`,
  with `Relation.joint: Known<FileSpan>`. It is a graph-reply change, and it is next after Phase 2.
  Incoming uses already have spans (`ReferenceSite.span`).
- **Feature unification, "on because of X".** This is a package-graph fact (cargo metadata resolve:
  `packages[].dependencies[].features`, `uses_default_features`, and `resolve.nodes[].features` for the user's workspace root). It belongs to the package dossier, not symbol facts, and is a separate lane.
- **Deref target methods, release history, dyn-compatibility, `#[doc = include_str!]` crate docs**
  (the Rust lowering reads only `///`/`//!`, agent-verified). Out of this lane's scope.
- **C/C++ `#if` gates.** Out of scope, as briefed.
- **`ItemKind::Variant → DeclarationKind::Constant`** (`view_build/identity.rs:69`). A small, adjacent defect; I flag it and do not fix it here.

## 7. Risks and questions for the lead
0. **R1 containment:** re-parent Rust impl methods under their impl rows in the structural lane (the lanes agree, and the page gathers by impl), or keep them attached to the type?
1. **F2 carrier:** is structural-pairing obligation acceptable for the semantic lane in Phase 1, with the IR column deferred?
2. **DTO bump 7→8:** confirm. Separately, I will prove that a workspace written before the change reopens, with the journal replayed and the PSR records re-admitted byte-identically.
3. **The structural fixture for `content_truth`:** a new small workspace-member crate, or should I add items to an existing one?
4. `backend_present` gains `sections.rs`. Tell me if another lane owns `crates/present`.
