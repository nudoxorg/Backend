# W-Facts Phase 1: finish checkpoint (W-Parity, Sonnet)

HEAD at the start of this pass: `92f974b370660b8273147886893703d3305e99c4`, tree clean.
The prior W-Facts (Opus) agent's own recorded HEAD was `fe0c4e9791158d7da978a3338dc3e4c027f65370`,
280 commits behind; the owner committed its remaining work on top in five commits between
then and now (`195745ed1`, `8820672c9`, `e9e148faa`, `85fea268a`, `7ed1b0459`), all already
in HEAD before I started.

## 1. What was already in HEAD (verified against `facts/PLAN.md`, code read at HEAD)

- **F1 deprecation**, all 7 languages: `crates/compile/src/facts.rs` (809 lines, new) —
  `Fact<T>`, `Deprecation`, `Obligation`, `DeclarationFacts`, `interpret`-style helpers
  (`deprecation_in_attribute`, `deprecation_in_documentation`, `deprecation_in_item`
  dispatching per `SourceLanguage`). `crates/compile/src/syntax_facts.rs` (new) computes
  facts from the tree-sitter node; `crates/compile/src/syntax.rs`'s `SourceDeclaration`
  carries `facts: DeclarationFacts` end to end (`with_facts`/`facts()`).
- **F2 obligation** (Required/Provided), computed structurally for Rust, Java, C#, Python,
  Go, C++, TypeScript: `crates/compile/src/syntax_facts.rs:114-147` (Rust: trait
  `function_signature_item`/`associated_type` → Required, `function_item` → Provided,
  `const_item` by whether it has a value), plus per-language blocks below it (Java
  interface/`default`, C# interface/`abstract`/`virtual`, Python ABC/Protocol +
  `@abstractmethod`, Go interface `method_elem`, TypeScript signatures vs concrete,
  C++ `pure_virtual_clause`). Desktop side: `apps/desktop/src/model/pages/common.rs`
  `DeclFacts { deprecation: Known<Option<Deprecation>>, obligation: Known<Option<Obligation>> }`
  (`:375-430`), filled by `DeclFacts::from_facts` from `Row.facts` — every `DeclRef`
  (page identity **and** every member) carries it, matching F1's "every reference can
  strike a deprecated name" and F2's per-member obligation, without a separate
  `Member.obligation` field (the plan proposed one; the actual design puts it on
  `DeclRef.facts` instead, which is a strict superset — it also covers the page's own
  identity and every relation's `DeclRef`, not just ledger rows).
- **F3 full member docs + producer honesty fix**: `apps/desktop/src/model/pages/symbol.rs`
  `Member.docs: Arc<[DocFragment]>` (`:229-241`) alongside `summary`. The placeholder
  document `"{kind} in {path}:{line}"` the plan flagged (`view_build/semantic.rs:262-268`
  at the plan's read) is gone from HEAD: `structural_declaration_row`
  (`crates/local-service/src/builtin/view_build/semantic.rs:362-384`) now reads:
  ```
  let document = if prepared.is_file_module {
      vec![Fragment::Text(format!("{} source · {path}", language.name()))]
  } else if declaration.documentation().is_empty() {
      Vec::new()
  } else {
      vec![Fragment::Text(declaration.documentation().to_owned())]
  };
  ```
  — an undocumented structural row gets an **empty** document, exactly as F3 required.
- **F4 doc sections**: new crate-level module `crates/present/sections.rs`, `SectionKind`
  re-exported at `apps/desktop/src/model/pages/symbol.rs:7` (`pub use backend_present::SectionKind;`),
  `DocSections { lead, sections: Arc<[DocSection]> }` and `DocSection { kind, title, body, entries }`
  on both `SymbolPage.sections` and `Member.sections` (`symbol.rs:140-241`). Per-language
  heading/tag recognition (`kind_of_title`, `markdown_heading`, Python Google/NumPy,
  JSDoc/Doxygen `@throws`, C# XML tags) lives in `sections.rs`.
- **Carrier plumbing**: `crates/library/view/model.rs` `Row.facts`/`Document.facts` (not
  re-quoted here; wired the same way as the plan's §1 table), canonical suffix tag byte
  in `crates/library/canonical/encoding.rs`, wire fields on `RowWire`/`DocumentWire`
  (`#[serde(default, skip_serializing_if = ...)]`), **DTO_VERSION bumped 7 → 8**
  (`crates/library/wire/mod.rs:65`).
- **PSRB**: `crates/engine/src/builtin/relation.rs` — `SOURCE_RECORD_FORMAT_FACTS = b"PSRB"`
  (`:37`), minimal-tag selection in `source_record_format` (`:58-76`, chooses `PSRB` only
  when `states_facts(declarations)`, otherwise falls through to `PSRA`/`PSR9`/`PSR8` exactly
  as before), `decode_declaration` liberal on older tags. Tests
  `declaration_facts_round_trip_in_the_facts_format_only` and
  `a_record_stating_no_containment_encodes_exactly_as_it_did_before` (and neighbours) already
  pin both directions: a record with facts gets `PSRB` and round-trips its note/since/predicate
  text; a record with none keeps its historical tag byte-for-byte.
- **Fixtures**: `apps/desktop/tests/fixtures/facts_standalone/` (own `[workspace]`, so the
  compiler-backed lane answers) and `apps/desktop/tests/fixtures/facts_unmanifested/` (no
  Cargo.toml, so the structural lane answers), both committed (`7ed1b0459`), identical
  source: `stale`/`fresh` (deprecation), `Service` trait (`execute` required + full second
  doc paragraph, `describe` provided), `Plain` struct (`label` deprecated field, `name` not).
- **End-to-end test**: `apps/desktop/tests/facts_truth.rs` (312 lines at `85fea268a`),
  running a real embedded `backend-locald` over both fixtures and asserting, on the real
  `SymbolPage`/`Member` read model: the deprecation `since`/`note` text, the `# Errors`/
  `# Panics` section kinds+titles+bodies, the lead prose, `fresh`'s explicit non-deprecation,
  `execute`'s `Required` obligation plus its full second paragraph, `describe`'s `Provided`,
  and the struck field's note — once per lane (`semantic: bool` parameter), with
  `stale_page.identity.semantic` asserted so neither lane can pass for the other.
- **Per-language structural seam test**: `tests/compatibility/tests/structural_facts.rs`
  (423 lines, committed `e9e148faa`) — real source through `SyntaxFrontend::analyze` for
  Rust, Java, C#, Python, TypeScript, Go and C++, one test per language, asserting the
  rendered `since`/note text and the obligation name for a required and a provided member.
  This is the plan's seam-test item 1, and it is the practical substitute for a 7-language
  desktop end-to-end (`facts_polyglot.rs`, plan item 9): running it needs no non-Rust
  toolchain (`SyntaxFrontend` is tree-sitter only), and it already proves every language's
  interpreter reads its own spelling correctly.
- **Compile-crate facts tests**: `crates/compile/src/facts.rs`'s own `#[cfg(test)] mod tests`
  (`documentation_conventions_read_their_words`, `every_attribute_spelling_reads_its_since_and_note`,
  `a_semantic_observation_wins_only_when_it_looked`, `an_oversized_note_is_cut_visibly_and_refused_on_admission`).

**Conclusion: F1, F2, F3 and F4 are all functionally complete and tested at HEAD**, on both
lanes (compiler-backed and structural), through a real desktop read model — not just
unit-level.

## 2. What was missing, and what I did about it

1. **Two dead-code warnings in `facts_truth.rs`** (`repo()` at the old line 35, and
   `Plane::dossier()` at the old line 159) — the task asked me to decide whether they were
   unfinished work or dead code. They are dead code: `repo()` is never called anywhere in
   the file (the test builds fixture paths from `CARGO_MANIFEST_DIR` directly, never the
   repo root), and `dossier()` exercises `PageKey::Package`/`PackageDossier`, which this
   test never needs — it only reads `PageKey::Symbol` pages. Nothing in `PLAN.md`'s test
   list (§5) calls for a package-dossier assertion in this file (that belongs to
   `content_truth.rs`, which already has one). I removed both methods and their
   now-unused imports (`PackageDossier`, `PackageRef`). `cargo test -p backend-desktop
   --test facts_truth` still passes (see §3); the two warnings are gone.
2. **`content_truth.rs`'s Phase-1-flavoured additions** (plan §5 item 8: a deprecated fn,
   a required/provided trait, a `#[cfg(feature = "extra")]` item, a two-paragraph member
   doc with `# Errors`, `#[derive(Clone)]`, **and a blanket impl** on `rich_project`) — I did
   **not** add these. The blanket-impl and `Arrival::Derived`/`Arrival::Blanket` assertions
   in that same plan item are Phase 2 (R1/R2), which I was told not to start, and Phase 2's
   own impl-rows/pairing fix (R1) is a precondition for the blanket-impl assertion to mean
   anything (today every `Arrival` is still `NotReported`, unchanged since the audit).
   Splitting the item to add only its Phase-1-safe half would duplicate what
   `facts_truth.rs` already asserts more directly, and `content_truth.rs` currently fails
   *before* reaching any point I could add to (see §4, D9) — extending it now would not run.
   I recommend the Phase 2 agent add the full item 8 once R1 lands, in one pass.
3. **`facts_polyglot.rs`** (plan §5 item 9, a full desktop e2e over one fixture file per
   language) — deferred. `tests/compatibility/tests/structural_facts.rs` already proves the
   fact-interpretation half for all 7 languages (see §1); the remaining value of a desktop
   e2e is proving the *view-projection* merge rule (`view_build/semantic.rs`) for non-Rust
   semantic lowerings specifically, which needs each language's real toolchain
   (`javac`/`csc`/`go`/etc.) to produce a compiler-backed publication. I did not have
   evidence any of those toolchains are set up in this environment, and standing up new
   per-language fixtures plus toolchain discovery is a materially bigger task than "finish
   what's missing" — I'm flagging it rather than half-building it.

## 3. Test results (facts-specific), two runs each, quoted

**`apps/desktop/tests/facts_truth.rs`** (before my dead-code cleanup — the logic under test
is unchanged by that cleanup):
```
test declaration_facts_reach_the_page_on_both_lanes ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 32.06s
```
```
test declaration_facts_reach_the_page_on_both_lanes ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 28.28s
```
A third run, taken right after the dead-code removal, also passed:
```
test declaration_facts_reach_the_page_on_both_lanes ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 30.60s
```
Two more retries in between were blocked, not failed: `backend-facet` — a shared crate this
test's binary links transitively — was mid-edit by another lane, twice in a row
(`apps/facet/src/overlay/float/model.rs:980,989`, "missing field `linear`", then later
`apps/facet/src/controls/comb/mod.rs`, "file not found for module `styled`"), and would not
compile either time. Per the wave-4 rule ("if a shared crate fails to compile because of
someone else's in-flight edit, wait and retry; don't fix their code") I did not touch either
file, and retried later instead. The retry succeeded, twice consecutively, once the other
lane's edit settled:
```
test declaration_facts_reach_the_page_on_both_lanes ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 29.62s
```
(a further immediate retry hit the *next* shared-crate edit from the same lane and did not
complete — again not a failure of this test, see above). Combined with the 30.60s run
immediately after the cleanup, **two clean consecutive post-cleanup passes exist**
(30.60s, 29.62s), with only unrelated other-lane build churn in between.

**`crates/compile` facts tests** (`cargo test -p backend-compile facts`), twice:
```
test facts::tests::an_oversized_note_is_cut_visibly_and_refused_on_admission ... ok
test facts::tests::a_semantic_observation_wins_only_when_it_looked ... ok
test facts::tests::documentation_conventions_read_their_words ... ok
test facts::tests::every_attribute_spelling_reads_its_since_and_note ... ok
test contract::facts::tests::closed_materialization_witness_stays_out_of_authority ... ok
test tests::complete_authority_requires_closed_manifest_and_typed_facts ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 53 filtered out; finished in 0.16s
```
(second run identical, `finished in 0.13s`).

**`tests/compatibility/tests/structural_facts.rs`** (`cargo test -p backend-compatibility-tests
--test structural_facts`), twice:
```
test go_reads_the_deprecated_paragraph_and_interface_methods ... ok
test java_reads_deprecated_annotations_and_interface_defaults ... ok
test rust_reads_deprecated_attributes_and_trait_obligations ... ok
test python_reads_deprecated_decorators_and_abstract_methods ... ok
test typescript_reads_jsdoc_deprecation_and_optional_members ... ok
test csharp_reads_obsolete_and_default_interface_members ... ok
test cpp_reads_deprecated_attributes_and_pure_virtuals ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
```
(second run identical, `finished in 0.12s`).

**`apps/desktop/tests/content_truth.rs`** (`cargo test -p backend-desktop --test content_truth`):
fails, reproducibly, at HEAD — **not a Phase 1 regression**, see §4.
```
thread 'every_board_reads_real_content_through_the_desktop_runtime' panicked at
apps/desktop/tests/content_truth.rs:303:50:
references gap
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 54.40s
```
This matches the prior agent's own `content_truth.run1.log` byte for byte (same panic site,
same message), captured against its recorded HEAD (`fe0c4e979`), 280 commits earlier — so
this is a pre-existing, standing failure, not something introduced by finishing Phase 1.

## 4. D9 investigated (read-only, per `QUEUE.md`'s note to fold it into the brief)

`content_truth.rs:9-13` documents the design: `crates/present` is a workspace member, so it
gets only the **structural** projection and no compiler ("semantic") publication; the test
expects `trait_page.references` to be an **Unknown** gap with reason `NoSemanticPublication`
(line 303-305).

At HEAD it is not a gap — `.gap()` returns `None`, so `.expect("references gap")` panics. Root
cause, read at HEAD: `crates/local-service/src/builtin/commands/semantic_query.rs`'s
`execute_references` (`:900`) falls back to `execute_structural_references` (`:1003`) whenever
`!publication_found` for the package — i.e. exactly the workspace-member case `content_truth.rs`
is testing. That fallback function returns `Ok(SurfaceReply::References { .. })` built from
`view_build::structural_reference_facts`, not an error, so the desktop's `references()` mapping
(`apps/desktop/src/runtime/page_mapping.rs:1047-1063`) produces `Known::Known(sites)` instead of
the `client_gap`-derived `NoSemanticPublication` gap the test still expects.

`git blame` dates this fallback to `64c36433b2`, "Merge the product command pipeline split (PR
#40)", **2026-09-25 23:36 UTC** — before the prior W-Facts agent's own recorded HEAD
(`fe0c4e979`, 2026-09-26 18:17) and long before any wave-4 lane started. So:
- **This is not something Phase 1 broke.** No facts commit touches `semantic_query.rs`.
- **It looks like a real, deliberate improvement that content_truth.rs's assertion was never
  updated for**: workspace members now get a best-effort *structural* references answer
  instead of a hard "no publication" refusal. That is arguably strictly better for a reader
  (bare use-sites for your own in-tree crate, instead of nothing), but it means the file's
  own documented design claim ("crates/present ... refuses a compiler publication", doc
  comment lines 9-13) is now only half true: *typed relations*
  (`rose.up`/`down`/`left`/`right`/`implemented_by`) still correctly gap with
  `NoSemanticPublication` (the assertion at line 300 passes — the panic is at line 303, later),
  but bare **references** do not any more.
- I did not change `content_truth.rs` or `semantic_query.rs`: neither is Phase 1 facts work,
  and the rules keep me inside my lane. I'm handing this to the brief (§7) with the exact
  file:line evidence above so the next agent doesn't have to re-derive it.

## 5. My own mutations (5), each in one Bash command with a `trap` restore, quoted panics

1. **F1 interpreter (Rust `note`)** — `crates/compile/src/facts.rs`, `deprecation_in_item`:
   changed `keyed(&item.arguments, &["note"]).or(bare)` to `None`. Broke
   `structural_facts::rust_reads_deprecated_attributes_and_trait_obligations`:
   ```
   Error: "function stale: deprecated since Some(\"1.2.0\") note None, owes nothing
       expected deprecated since Some(\"1.2.0\") note Some(\"use `fresh`\"), owes nothing
   field label: deprecated since None note None, owes nothing
       expected deprecated since None note Some(\"read `name` instead\"), owes nothing ..."
   test rust_reads_deprecated_attributes_and_trait_obligations ... FAILED
   ```
2. **PSRB minimal-tag invariant** — `crates/engine/src/builtin/relation.rs`,
   `source_record_format`: changed `if states_facts(declarations)` to `if true`, forcing every
   record (even fact-less ones) to `PSRB`. Broke 5 tests, e.g.:
   ```
   assertion `left == right` failed
     left: Some([80, 83, 82, 66])   // "PSRB"
    right: Some([80, 83, 82, 56])   // "PSR8"
   test builtin::relation::tests::a_stated_source_identity_round_trips_and_earns_its_tag ... FAILED
   test builtin::relation::tests::containment_round_trips_and_a_redundant_contained_record_is_refused ... FAILED
   test builtin::relation::tests::declaration_facts_round_trip_in_the_facts_format_only ... FAILED
   test builtin::relation::tests::source_relation_round_trips_typed_declaration_metadata ... FAILED
   test builtin::relation::tests::a_record_stating_no_containment_encodes_exactly_as_it_did_before ... FAILED
   test result: FAILED. 21 passed; 5 failed; ...
   ```
   This is exactly the invariant PLAN.md named: "a row with no facts encodes to exactly the
   bytes it had before this change."
3. **DTO version gate** — `crates/library/wire/command.rs`, `CommandDto::deserialize`: changed
   `if envelope.version != DTO_VERSION` to `if false` (accept any version). Broke
   `wire::tests::dto_versions_and_outer_fields_are_strict`:
   ```
   thread 'wire::tests::dto_versions_and_outer_fields_are_strict' panicked at crates/library/wire/tests.rs:1004:5:
   assertion failed: serde_json::from_value::<CommandDto>(command_json).is_err()
   ```
   (that assertion feeds a `CommandDto` JSON stamped `DTO_VERSION + 1`, i.e. 9 — the version
   gate is what refuses it.)
4. **F2 obligation (Rust `function_item` → Provided)** —
   `crates/compile/src/syntax_facts.rs`, the trait-member match: changed
   `"function_item" => Some(Obligation::Provided)` to `Some(Obligation::Required)`. Broke
   `structural_facts::rust_reads_deprecated_attributes_and_trait_obligations`:
   ```
   Error: "method describe: current, required
       expected current, provided ..."
   test rust_reads_deprecated_attributes_and_trait_obligations ... FAILED
   ```
5. **F4 doc sections (`# Errors` heading classification)** —
   `crates/present/sections.rs`, `kind_of_title`: changed the `"errors" | "error" | ... =>
   SectionKind::Errors` arm to `SectionKind::Other`. Broke two `backend-present` tests:
   ```
   test sections::tests::rust_markdown_headings_open_their_sections ... FAILED
   test sections::tests::python_google_numpy_and_sphinx_sections_are_read ... FAILED
   test result: FAILED. 50 passed; 2 failed; ...
   ```
   (First attempt at this one hit the same in-flight `backend-facet` breakage as facts_truth's
   post-cleanup run and was retried against `backend-present` directly, which doesn't depend
   on facet — same mutation, clean signal.)

All five: `git status --short` after each restore showed the file untouched; `git diff
--stat` on the five files is empty at the end of this pass. The only real change I'm leaving
in the tree is the two-method, three-import cleanup in `facts_truth.rs` (§2.1).

## 6. Recommendation for the Phase 2 agent

Start from `PLAN.md` §4 (R1-R5) exactly as written; nothing in Phase 1 needs revisiting. Two
things worth re-confirming on arrival, since they moved after the plan was written:
- `page_mapping.rs` is now at `apps/desktop/src/runtime/page_mapping.rs` (the plan cites
  `apps/desktop/src/model/pages/page_mapping.rs`, which no longer exists — a refactor moved it
  under `runtime/` at some point after the plan was written, unrelated to facts).
- `Member` does not have its own `obligation`/`deprecation` fields; both live on
  `DeclRef.facts: DeclFacts` (`common.rs:375-430`), and `Member.decl: DeclRef` already carries
  them. R1's impl-row work should follow the same pattern rather than adding new fields to
  `Member`.
