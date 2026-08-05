// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::path::PathBuf;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{DocLink, Entry, Node, Symbol, Visibility},
    kind::Kind,
    kinds::Module,
    view::IrView,
};
use nudox_store::package::{PackageView, Provenance};

use super::doc_link_table::DocLinkTable;
use super::prose::{expand_shortcut_links, is_symbol_path};
use super::walk_doc;
use crate::wire::{InlineRun, LinkTarget, ProseBlock, RenderSection, SectionId};

// ── Helpers ───────────────────────────────────────────────────────────────

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test"), PackageName::new("test-walk"))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn sym_with_doc_links(name: &str, doc: &str, links: Vec<DocLink>) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: doc.to_owned(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: links.into_boxed_slice(),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Build a two-entry `PackageView`: a root module and one child function.
///
/// `child_name` is the name of the child function (e.g. `"with_state"`).
/// Returns `(PackageView, child_intro)`.
fn build_pkg_with_child(child_name: &str) -> (PackageView, IntroId) {
    let root_id = intro(1);
    let child_id = intro(2);
    let mut table = PristineIntroTable::new();

    let root_sym = Symbol {
        name: "root".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        root_id,
        Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let child_sym = Symbol {
        name: child_name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        child_id,
        Entry::new(
            child_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);
    (pkg, child_id)
}

// ── DocLinkTable tests ────────────────────────────────────────────────────

/// A resolved `DocLink` for `with_state` inside `root` must map to the
/// correct `SymbolKey` via both the full path and the bare leaf.
#[test]
fn doc_link_table_resolves_leaf_and_full_path() {
    let (pkg, child_id) = build_pkg_with_child("with_state");

    // Simulate what the Rust producer stores:
    // target = "Router::with_state", label = Some("Router::with_state")
    let doc_links = vec![DocLink {
        target: "Router::with_state".to_owned(),
        label: Some("Router::with_state".to_owned()),
    }];

    // Build a fake symbol just for the table builder.
    let sym = sym_with_doc_links("Router", "", doc_links);
    let root_entry = Entry::new(
        sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let table = DocLinkTable::build(&root_entry, &pkg);

    // The expected key is child_id under our test lineage.
    let expected_key = StableRef::new(lineage(), child_id);

    // Resolution via full target string.
    assert_eq!(
        table.resolve("Router::with_state"),
        Some(&expected_key),
        "full path must resolve"
    );

    // Resolution via bare leaf.
    assert_eq!(
        table.resolve("with_state"),
        Some(&expected_key),
        "bare leaf must resolve"
    );
}

/// A link whose leaf is not in the corpus must not resolve.
#[test]
fn doc_link_table_cross_crate_resolves_to_none() {
    let (pkg, _) = build_pkg_with_child("local_fn");

    let doc_links = vec![DocLink {
        target: "tower::Service".to_owned(),
        label: Some("Service".to_owned()),
    }];

    let sym = sym_with_doc_links("MyStruct", "", doc_links);
    let entry = Entry::new(
        sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let table = DocLinkTable::build(&entry, &pkg);

    // "Service" is not in the corpus.
    assert_eq!(table.resolve("tower::Service"), None);
    assert_eq!(table.resolve("Service"), None);
}

// ── expand_shortcut_links tests ───────────────────────────────────────────

/// Build a minimal `DocLinkTable` from a hand-built `(name, SymbolKey)` map
/// for use in `expand_shortcut_links` tests without a full `PackageView`.
fn make_table(entries: &[(&str, IntroId)]) -> DocLinkTable {
    let mut inner = HashMap::new();
    for (name, id) in entries {
        let key = StableRef::new(lineage(), *id);
        inner.insert(name.to_string(), key.clone());
    }
    // A hand-built table stands in for a symbol that *declared* links, so
    // `declared` is true whenever any entry was supplied.
    let declared = !inner.is_empty();
    DocLinkTable { inner, declared }
}

/// `[with_state]` resolves to a Symbol link when the table contains it.
#[test]
fn expand_resolves_simple_shortcut() {
    let table = make_table(&[("with_state", intro(2))]);
    let runs = expand_shortcut_links("[with_state]", &table, false, false);

    assert_eq!(runs.len(), 1);
    match &runs[0] {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
        } => {
            assert_eq!(&**text, "with_state");
            assert_eq!(key.intro, intro(2));
        }
        other => panic!("expected Link, got {other:?}"),
    }
}

/// `[tower::Service]` (cross-crate, not in table) emits plain text without
/// brackets.
#[test]
fn expand_cross_crate_strips_brackets() {
    let table = make_table(&[]); // empty table — nothing in corpus
    let runs = expand_shortcut_links("[tower::Service]", &table, false, false);

    assert_eq!(runs.len(), 1, "must produce exactly one run");
    match &runs[0] {
        InlineRun::Text { text } => {
            assert_eq!(
                &**text, "tower::Service",
                "brackets must be stripped; inner text must be preserved"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

/// Plain text with no brackets is returned unchanged as a single Text run.
#[test]
fn expand_plain_text_unchanged() {
    let table = make_table(&[]);
    let runs = expand_shortcut_links("hello world", &table, false, false);
    assert_eq!(runs.len(), 1);
    match &runs[0] {
        InlineRun::Text { text } => assert_eq!(&**text, "hello world"),
        other => panic!("expected Text, got {other:?}"),
    }
}

/// `[!NOTE]` must not be treated as a shortcut link (its inner text is not
/// a valid symbol path due to the `!` character).
#[test]
fn expand_callout_lead_not_treated_as_link() {
    let table = make_table(&[]);
    let runs = expand_shortcut_links("[!NOTE]", &table, false, false);
    // Must come back as a single Text run with the original brackets.
    assert_eq!(runs.len(), 1);
    match &runs[0] {
        InlineRun::Text { text } => {
            assert_eq!(
                &**text, "[!NOTE]",
                "callout lead must be preserved verbatim"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

/// Backtick-quoted shortcut `` [`with_state`] `` resolves correctly; the
/// display text preserves the backtick as written.
#[test]
fn expand_backtick_quoted_link_resolves() {
    let table = make_table(&[("with_state", intro(2))]);
    let runs = expand_shortcut_links("[`with_state`]", &table, false, false);

    assert_eq!(runs.len(), 1);
    match &runs[0] {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
        } => {
            assert_eq!(&**text, "`with_state`");
            assert_eq!(key.intro, intro(2));
        }
        other => panic!("expected Link, got {other:?}"),
    }
}

/// Unresolvable backtick-quoted link emits `InlineRun::Code` (not `Text`),
/// preserving the monospace rendering intent.
#[test]
fn expand_backtick_cross_crate_emits_code() {
    let table = make_table(&[]);
    let runs = expand_shortcut_links("[`tower::Service`]", &table, false, false);

    assert_eq!(runs.len(), 1);
    match &runs[0] {
        InlineRun::Code { text } => {
            assert_eq!(&**text, "tower::Service");
        }
        other => panic!("expected Code, got {other:?}"),
    }
}

/// Mixed text: `"See [with_state] for more."` splits into three runs.
#[test]
fn expand_mixed_text_splits_correctly() {
    let table = make_table(&[("with_state", intro(2))]);
    let runs = expand_shortcut_links("See [with_state] for more.", &table, false, false);

    // Expect: Text("See "), Link("with_state"), Text(" for more.")
    assert_eq!(runs.len(), 3, "must produce 3 runs");
    match &runs[0] {
        InlineRun::Text { text } => assert_eq!(&**text, "See "),
        other => panic!("expected Text, got {other:?}"),
    }
    match &runs[1] {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
        } => {
            assert_eq!(&**text, "with_state");
            assert_eq!(key.intro, intro(2));
        }
        other => panic!("expected Link, got {other:?}"),
    }
    match &runs[2] {
        InlineRun::Text { text } => assert_eq!(&**text, " for more."),
        other => panic!("expected Text, got {other:?}"),
    }
}

/// Strong wrapping is preserved on plain-text fragments produced by
/// `expand_shortcut_links` when `in_strong = true`.
#[test]
fn expand_strong_wrapping_preserved() {
    let table = make_table(&[]);
    // Cross-crate link inside a `**...**` span — the emitted Text run for
    // the stripped inner text must be `Strong`, not plain `Text`.
    let runs = expand_shortcut_links("[Unknown::Thing]", &table, true, false);
    assert_eq!(runs.len(), 1);
    match &runs[0] {
        InlineRun::Strong { text } => {
            assert_eq!(&**text, "Unknown::Thing");
        }
        other => panic!("expected Strong, got {other:?}"),
    }
}

// ── is_symbol_path tests ──────────────────────────────────────────────────

#[test]
fn is_symbol_path_accepts_valid_paths() {
    assert!(is_symbol_path("Foo"));
    assert!(is_symbol_path("Router::with_state"));
    assert!(is_symbol_path("std::collections::HashMap"));
    assert!(is_symbol_path("_private"));
    assert!(is_symbol_path("root.child"));
}

#[test]
fn is_symbol_path_rejects_invalid() {
    assert!(!is_symbol_path(""));
    assert!(!is_symbol_path("https://doc.rust-lang.org"));
    assert!(!is_symbol_path("has space"));
    assert!(!is_symbol_path("!NOTE"));
    assert!(!is_symbol_path("1starts_with_digit"));
}

// ── walk_doc integration tests ────────────────────────────────────────────

/// End-to-end test: a symbol whose documentation contains `[child_fn]`
/// (a rustdoc shortcut link) and whose `doc_links` field carries the
/// resolved target must produce an `InlineRun::Link` in the output prose,
/// not a bare text run with brackets.
///
/// This is the canonical regression test for the original bug:
/// `[Router::with_state]` rendering as literal text with brackets.
#[test]
fn walk_doc_shortcut_link_becomes_symbol_link() {
    // Build a two-entry package: root module + child function.
    let root_id = intro(1);
    let child_id = intro(2);
    let mut table = PristineIntroTable::new();

    // The child function is the link target.
    let child_sym = Symbol {
        name: "child_fn".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        child_id,
        Entry::new(
            child_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );

    // The root symbol has documentation with a `[child_fn]` shortcut link
    // and a `doc_links` entry that resolves it.
    let root_sym = sym_with_doc_links(
        "root",
        "See [child_fn] for details.",
        vec![DocLink {
            // The target matches how the producer stores it (may be bare
            // leaf or a qualified path; we test the bare leaf case).
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
    );
    table.insert_live(
        root_id,
        Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let root_entry = pkg.view().entry(root_id).expect("root must exist");
    let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

    // We expect exactly one prose section (the doc comment text).
    // The members section (with child_fn) is also present.
    let prose_section = output
        .sections
        .iter()
        .find_map(|s| match s {
            RenderSection::Prose { blocks, .. } => Some(blocks),
            _ => None,
        })
        .expect("must have a Prose section");

    // The paragraph block must contain the link.
    let paragraph_runs = match prose_section.first().expect("must have blocks") {
        ProseBlock::Paragraph { runs } => runs,
        other => panic!("expected Paragraph, got {other:?}"),
    };

    // Locate the Link run in the paragraph.
    let link_run = paragraph_runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .expect("paragraph must contain a Link run for the resolved shortcut");

    match link_run {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
        } => {
            assert_eq!(
                &**text, "child_fn",
                "link text must be the inner shortcut text"
            );
            assert_eq!(
                key.intro, child_id,
                "link must point at the child function's IntroId"
            );
        }
        other => panic!("expected Link with Symbol target, got {other:?}"),
    }
}

/// End-to-end test: the backtick-quoted form `` [`child_fn`] `` must also
/// produce an `InlineRun::Link`, not a bare `Code` run.
///
/// pulldown-cmark emits the backtick form as:
///   Text("[")  /  Code("child_fn")  /  Text("]...")
///
/// The event-stream lookahead in `build_prose_blocks` handles the `Code`
/// inner event specifically — this test verifies that branch.
#[test]
fn walk_doc_backtick_shortcut_link_becomes_symbol_link() {
    let root_id = intro(1);
    let child_id = intro(2);
    let mut table = PristineIntroTable::new();

    let child_sym = Symbol {
        name: "child_fn".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        child_id,
        Entry::new(
            child_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );

    // Documentation uses the backtick form: [`child_fn`]
    let root_sym = sym_with_doc_links(
        "root",
        "See [`child_fn`] for details.",
        vec![DocLink {
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
    );
    table.insert_live(
        root_id,
        Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let root_entry = pkg.view().entry(root_id).expect("root must exist");
    let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

    let prose_section = output
        .sections
        .iter()
        .find_map(|s| match s {
            RenderSection::Prose { blocks, .. } => Some(blocks),
            _ => None,
        })
        .expect("must have a Prose section");

    let paragraph_runs = match prose_section.first().expect("must have blocks") {
        ProseBlock::Paragraph { runs } => runs,
        other => panic!("expected Paragraph, got {other:?}"),
    };

    let link_run = paragraph_runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .expect("paragraph must contain a Link run for the backtick shortcut");

    match link_run {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
        } => {
            assert_eq!(
                &**text, "child_fn",
                "link text must be the inner shortcut text (without backticks)"
            );
            assert_eq!(
                key.intro, child_id,
                "link must point at the child function's IntroId"
            );
        }
        other => panic!("expected Link with Symbol target, got {other:?}"),
    }
}

/// When `doc_links` is empty (no resolved links), bracketed text that looks
/// like shortcut references must not be corrupted — the fast path must leave
/// text completely unchanged.
#[test]
fn walk_doc_empty_doc_links_preserves_bracketed_text() {
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();

    let root_sym = Symbol {
        name: "root".to_owned(),
        visibility: Visibility::Public,
        documentation: "See [SomeType] for info.".to_owned(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        // No doc_links — the shortcut is not resolved.
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        root_id,
        Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let root_entry = pkg.view().entry(root_id).expect("root must exist");
    let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

    let prose_section = output
        .sections
        .iter()
        .find_map(|s| match s {
            RenderSection::Prose { blocks, .. } => Some(blocks),
            _ => None,
        })
        .expect("must have a Prose section");

    let paragraph_runs = match prose_section.first().expect("must have blocks") {
        ProseBlock::Paragraph { runs } => runs,
        other => panic!("expected Paragraph, got {other:?}"),
    };

    // With no doc_links the fast path is taken; the text `[SomeType]`
    // passes through unchanged as a single Text run.
    let full_text: String = paragraph_runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert!(
        full_text.contains("[SomeType]"),
        "without doc_links, bracketed text must pass through unchanged; got: {full_text:?}"
    );
}

// ── Defect-1 regression: multiple code blocks must each be their own section ─

/// Three fenced code blocks separated by prose must produce THREE distinct
/// `RenderSection::CodeBlock`s, each with its own `SectionId`, and each
/// `SectionId` must appear in the `SectionPlan`.
///
/// # Root cause
///
/// Before the fix, the entire event stream for a logical section — including
/// all fenced blocks — was handed to `build_prose_blocks` as one flat slice.
/// `build_prose_blocks` emitted `ProseBlock::Code` for each fence, all
/// embedded inside a single `RenderSection::Prose`.  The GUI renders sections
/// independently and its code-fence path was gated on
/// `RenderSection::CodeBlock`, not `ProseBlock::Code`; so only the first fence
/// (or none) received syntax highlighting, and subsequent fences were lost from
/// the user's view.
///
/// The fix (`split_at_code_blocks` in `parse_markdown`) promotes each fenced
/// block to its own `RenderSection::CodeBlock` with a freshly-assigned
/// `SectionId`.  Surrounding prose becomes separate `RenderSection::Prose`
/// sections.
///
/// # Why this test must go through `walk_doc`
///
/// A test that calls `build_prose_blocks` directly would prove the wrong
/// thing: `build_prose_blocks` still handles `ProseBlock::Code` for indented
/// code blocks.  Only `walk_doc` / `parse_markdown` performs the fenced-block
/// promotion, so this test exercises the real code path.
#[test]
fn walk_doc_three_code_blocks_each_become_own_section() {
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();

    // Documentation with three fenced code blocks separated by prose.
    // pulldown-cmark will emit three Start/Text/End(CodeBlock) triplets,
    // each surrounded by paragraph events.
    let doc = concat!(
        "Before first block.\n\n",
        "```rust\n",
        "fn one() {}\n",
        "```\n\n",
        "Between first and second.\n\n",
        "```python\n",
        "def two(): pass\n",
        "```\n\n",
        "Between second and third.\n\n",
        "```javascript\n",
        "function three() {}\n",
        "```\n\n",
        "After all blocks.",
    );

    let sym = Symbol {
        name: "root".to_owned(),
        visibility: Visibility::Public,
        documentation: doc.to_owned(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        root_id,
        Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let root_entry = pkg.view().entry(root_id).expect("root must exist");
    let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

    // Collect the CodeBlock sections.
    let code_sections: Vec<&RenderSection> = output
        .sections
        .iter()
        .filter(|s| matches!(s, RenderSection::CodeBlock { .. }))
        .collect();

    assert_eq!(
        code_sections.len(),
        3,
        "three fenced blocks must produce three RenderSection::CodeBlock sections; \
             got {} code sections and {} total sections: {:#?}",
        code_sections.len(),
        output.sections.len(),
        output
            .sections
            .iter()
            .map(|s| format!("{:?}", s.section_id()))
            .collect::<Vec<_>>(),
    );

    // All three must have distinct SectionIds.
    let ids: Vec<SectionId> = code_sections.iter().map(|s| s.section_id()).collect();
    assert_eq!(
        ids.len(),
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        "all three CodeBlock sections must have distinct SectionIds; got: {ids:?}",
    );

    // Every CodeBlock section must appear in the plan.
    for id in &ids {
        assert!(
            output.plan.iter().any(|p| p.id == *id),
            "SectionId {id:?} is in sections but not in plan — plan: {:#?}",
            output.plan,
        );
        // And the plan entry must have kind CodeBlock.
        let plan_entry = output.plan.iter().find(|p| p.id == *id).unwrap();
        assert_eq!(
            plan_entry.kind,
            crate::wire::SectionKind::CodeBlock,
            "plan entry for CodeBlock section {id:?} must have kind CodeBlock; \
                 got {:?}",
            plan_entry.kind,
        );
    }

    // The lang strings must be in order: rust, python, javascript.
    let langs: Vec<&str> = code_sections
        .iter()
        .map(|s| match s {
            // `lang.0` is `&SharedStr` (pattern binds a reference to the field);
            // `&*lang.0` dereferences the `&SharedStr` → `SharedStr` → `str`.
            RenderSection::CodeBlock { lang, .. } => &*lang.0,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        langs,
        vec!["rust", "python", "javascript"],
        "code block languages must appear in source order"
    );
}

// ── Defect-2 regression: realistic producer target formats must resolve ────

/// A namespace-tagged target (`"Router::with_state!m"`) must resolve to the
/// correct symbol.  The `!m` suffix is a producer namespace discriminator;
/// stripping it recovers the plain path that the name index knows.
///
/// This is the canonical regression test for the defect described in the
/// task brief: the original `DocLinkTable::build` used `rsplit("::")` to
/// extract the leaf, which returned the whole string `"with_state!m"` for a
/// namespace-tagged target (no `::` before the tag, so the split found
/// nothing to strip).  `by_name.get_exact("with_state!m")` found nothing,
/// so the link was always left unresolved.
#[test]
fn doc_link_table_namespace_tagged_target_resolves() {
    let (pkg, child_id) = build_pkg_with_child("with_state");

    let doc_links = vec![DocLink {
        // The producer emits a namespace tag to disambiguate methods from
        // fields and associated constants that share the same leaf name.
        target: "Router::with_state!m".to_owned(),
        label: Some("Router::with_state".to_owned()),
    }];

    let sym = sym_with_doc_links("Router", "", doc_links);
    let entry = Entry::new(
        sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let table = DocLinkTable::build(&entry, &pkg);

    let expected_key = StableRef::new(lineage(), child_id);

    // The exact original target string (with tag) must resolve.
    assert_eq!(
        table.resolve("Router::with_state!m"),
        Some(&expected_key),
        "namespace-tagged target must resolve after stripping the tag"
    );

    // The clean form (without tag) must also resolve.
    assert_eq!(
        table.resolve("Router::with_state"),
        Some(&expected_key),
        "clean form of the target must also resolve"
    );

    // The bare leaf must resolve.
    assert_eq!(
        table.resolve("with_state"),
        Some(&expected_key),
        "bare leaf must resolve"
    );
}

/// A dot-path producer target (`"axum.routing.Router.with_state"`) must
/// resolve.  Moniker paths use `.` as the separator; the original code only
/// split on `"::"` so a dot-path target returned the whole string as the
/// "leaf", which was never in the name index.
#[test]
fn doc_link_table_dot_path_target_resolves() {
    let (pkg, child_id) = build_pkg_with_child("with_state");

    let doc_links = vec![DocLink {
        target: "axum.routing.Router.with_state".to_owned(),
        label: None,
    }];

    let sym = sym_with_doc_links("Router", "", doc_links);
    let entry = Entry::new(
        sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let table = DocLinkTable::build(&entry, &pkg);
    let expected_key = StableRef::new(lineage(), child_id);

    assert_eq!(
        table.resolve("axum.routing.Router.with_state"),
        Some(&expected_key),
        "dot-path target must resolve via leaf `.` split"
    );
    assert_eq!(
        table.resolve("with_state"),
        Some(&expected_key),
        "bare leaf must also resolve"
    );
}

/// A dot-path namespace-tagged target (`"axum.routing.Router.with_state!m"`)
/// must resolve.  This combines the dot-path and namespace-tag problems.
#[test]
fn doc_link_table_dot_path_namespace_tagged_resolves() {
    let (pkg, child_id) = build_pkg_with_child("with_state");

    let doc_links = vec![DocLink {
        target: "axum.routing.Router.with_state!m".to_owned(),
        label: None,
    }];

    let sym = sym_with_doc_links("Router", "", doc_links);
    let entry = Entry::new(
        sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let table = DocLinkTable::build(&entry, &pkg);
    let expected_key = StableRef::new(lineage(), child_id);

    assert_eq!(
        table.resolve("axum.routing.Router.with_state!m"),
        Some(&expected_key),
        "dot-path + namespace tag must resolve after both stripping steps"
    );
    assert_eq!(
        table.resolve("with_state"),
        Some(&expected_key),
        "bare leaf must also resolve"
    );
}

/// A rustdoc HTML-anchor target (`"#method.with_state"`) must resolve.
/// rustdoc sometimes stores the HTML fragment identifier as the target.
/// The `#method.` prefix carries no semantic information the engine needs.
#[test]
fn doc_link_table_anchor_target_resolves() {
    let (pkg, child_id) = build_pkg_with_child("with_state");

    let doc_links = vec![DocLink {
        target: "#method.with_state".to_owned(),
        label: Some("with_state".to_owned()),
    }];

    let sym = sym_with_doc_links("Router", "", doc_links);
    let entry = Entry::new(
        sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let table = DocLinkTable::build(&entry, &pkg);
    let expected_key = StableRef::new(lineage(), child_id);

    assert_eq!(
        table.resolve("#method.with_state"),
        Some(&expected_key),
        "rustdoc anchor target must resolve after stripping the #discriminator. prefix"
    );
    assert_eq!(
        table.resolve("with_state"),
        Some(&expected_key),
        "bare leaf must also resolve"
    );
}

/// End-to-end test through `walk_doc`: a doc comment `[Router::with_state]`
/// with a namespace-tagged `doc_link` target must produce a Symbol link in
/// the rendered prose.
///
/// This is the full pipeline test for defect 2: the original bug survived
/// because the unit tests used hand-built `DocLinkTable` entries with simple
/// leaf-only targets, while real producer data uses qualified/tagged paths.
#[test]
fn walk_doc_namespace_tagged_target_resolves_to_link_in_prose() {
    let root_id = intro(1);
    let fn_id = intro(2);
    let mut table = PristineIntroTable::new();

    let fn_sym = Symbol {
        name: "with_state".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        fn_id,
        Entry::new(
            fn_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );

    // The root's doc references [Router::with_state] but the producer
    // stored the target with a namespace tag: "Router::with_state!m".
    let root_sym = sym_with_doc_links(
        "root",
        "Call [Router::with_state] to build the app.",
        vec![DocLink {
            target: "Router::with_state!m".to_owned(),
            label: Some("Router::with_state".to_owned()),
        }],
    );
    table.insert_live(
        root_id,
        Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let root_entry = pkg.view().entry(root_id).expect("root must exist");
    let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

    // Locate the prose section.
    let prose_section = output
        .sections
        .iter()
        .find_map(|s| match s {
            RenderSection::Prose { blocks, .. } => Some(blocks),
            _ => None,
        })
        .expect("must have a Prose section");

    let paragraph_runs = match prose_section.first().expect("must have blocks") {
        ProseBlock::Paragraph { runs } => runs,
        other => panic!("expected Paragraph, got {other:?}"),
    };

    let link_run = paragraph_runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .expect(
            "paragraph must contain a Link run; \
                 a namespace-tagged producer target must still resolve to a Symbol link",
        );

    match link_run {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
        } => {
            assert_eq!(&**text, "Router::with_state");
            assert_eq!(key.intro, fn_id);
        }
        other => panic!("expected Link(Symbol), got {other:?}"),
    }
}

/// End-to-end test: `[Self::bar]` (Rust path starting with `Self`) and
/// `[crate::routing::Router]` (crate-relative path) must each resolve to a
/// Symbol link when the underlying symbol is in the same package.
///
/// These two forms exercise the `resolve` fallback in `DocLinkTable::resolve`:
/// the doc text contains the author's path; the table holds the bare leaf.
#[test]
fn walk_doc_self_and_crate_prefix_paths_resolve() {
    let root_id = intro(1);
    let bar_id = intro(2);
    let router_id = intro(3);
    let mut table = PristineIntroTable::new();

    let bar_sym = Symbol {
        name: "bar".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        bar_id,
        Entry::new(
            bar_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );

    let router_sym = Symbol {
        name: "Router".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    table.insert_live(
        router_id,
        Entry::new(
            router_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );

    // The documentation uses both `Self::bar` and `crate::routing::Router`.
    let root_sym = sym_with_doc_links(
        "root",
        "Use [Self::bar] or [crate::routing::Router] here.",
        vec![
            DocLink {
                target: "Self::bar".to_owned(),
                label: Some("Self::bar".to_owned()),
            },
            DocLink {
                target: "crate::routing::Router".to_owned(),
                label: Some("crate::routing::Router".to_owned()),
            },
        ],
    );
    table.insert_live(
        root_id,
        Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let root_entry = pkg.view().entry(root_id).expect("root must exist");
    let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

    let prose_section = output
        .sections
        .iter()
        .find_map(|s| match s {
            RenderSection::Prose { blocks, .. } => Some(blocks),
            _ => None,
        })
        .expect("must have a Prose section");

    let paragraph_runs = match prose_section.first().expect("must have blocks") {
        ProseBlock::Paragraph { runs } => runs,
        other => panic!("expected Paragraph, got {other:?}"),
    };

    // Both links must appear.
    let link_runs: Vec<_> = paragraph_runs
        .iter()
        .filter(|r| {
            matches!(
                r,
                InlineRun::Link {
                    target: LinkTarget::Symbol { .. },
                    ..
                }
            )
        })
        .collect();

    assert_eq!(
        link_runs.len(),
        2,
        "both [Self::bar] and [crate::routing::Router] must produce Symbol links; \
             paragraph runs: {paragraph_runs:#?}",
    );

    let intros: std::collections::HashSet<_> = link_runs
        .iter()
        .map(|r| match r {
            InlineRun::Link {
                target: LinkTarget::Symbol { key },
                ..
            } => key.intro,
            _ => unreachable!(),
        })
        .collect();

    assert!(
        intros.contains(&bar_id),
        "bar_id must be among the linked symbols"
    );
    assert!(
        intros.contains(&router_id),
        "router_id must be among the linked symbols"
    );
}

// ── Defect-3 regression: producer path drops impl-type segment ────────────

/// When two symbols share the same leaf name (e.g. `with_state` on both
/// `Router` and `Handler`) the exact suffix check fails because the
/// producer emits `"axum::routing::with_state"` (module-qualified, no impl
/// type) while `moniker_path` stores `"axum.routing.Router.with_state"`.
///
/// The segment-level fallback uses the **label** (`"Router::with_state"`)
/// to disambiguate: label segments `["router", "with_state"]` are checked
/// against the dot-path segments of each hit's stored path in order.  The
/// entry whose stored path contains all label segments in order wins.
///
/// This test sets up a three-entry package:
///   - root module
///   - `Router.with_state` (child of a `Router` impl-like entry)
///   - `Handler.with_state` (child of a `Handler` impl-like entry)
///
/// Both leaf names are `"with_state"`.  The producer target is
/// `"axum::routing::with_state"` with label `"Router::with_state"`.  The
/// suffix check fails for both (neither moniker path ends with
/// `"axum.routing.with_state"`).  The segment-level fallback must pick the
/// entry under `Router`, not `Handler`.
#[test]
fn doc_link_table_segment_level_fallback_picks_correct_symbol_when_suffix_fails() {
    let root_id = intro(1);
    let router_id = intro(2); // "Router" (the struct/impl type)
    let router_ws_id = intro(3); // "with_state" child of Router
    let handler_id = intro(4); // "Handler"
    let handler_ws_id = intro(5); // "with_state" child of Handler

    let mut table = PristineIntroTable::new();

    // Insert the entries in a hierarchy that produces moniker paths:
    //   root          → "root"
    //   root.Router   → "root.Router"
    //   root.Router.with_state → "root.Router.with_state"
    //   root.Handler  → "root.Handler"
    //   root.Handler.with_state → "root.Handler.with_state"
    //
    // The producer canonical path would be "root::with_state" (module =
    // root, method name = with_state) for BOTH — it drops the type segment.
    // The label distinguishes them: "Router::with_state" vs "Handler::with_state".

    let make_sym = |name: &str| Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    table.insert_live(
        root_id,
        Entry::new(
            make_sym("root"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );
    table.insert_live(
        router_id,
        Entry::new(
            make_sym("Router"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );
    table.insert_live(
        router_ws_id,
        Entry::new(
            make_sym("with_state"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(router_id),
    );
    table.insert_live(
        handler_id,
        Entry::new(
            make_sym("Handler"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );
    table.insert_live(
        handler_ws_id,
        Entry::new(
            make_sym("with_state"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        ),
        Some(handler_id),
    );

    let view = IrView::with_package(lineage(), table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    // The producer target is "root::with_state" (module-qualified, no type
    // segment).  This normalises to "root.with_state".  Neither
    // "root.Router.with_state" nor "root.Handler.with_state" ends with
    // "root.with_state", so the exact suffix check must fail for both.
    //
    // The label "Router::with_state" segments = ["router", "with_state"].
    // "root.Router.with_state" contains both in order → match.
    // "root.Handler.with_state" does not contain "router" → no match.
    let doc_links = vec![DocLink {
        target: "root::with_state".to_owned(),
        label: Some("Router::with_state".to_owned()),
    }];

    let root_sym = sym_with_doc_links("root", "Call [Router::with_state] here.", doc_links);
    let root_entry = Entry::new(
        root_sym,
        Node::build(None::<nudox_ir::index::RawRef>, []),
        Kind::Module(Module),
    );

    let doc_link_table = DocLinkTable::build(&root_entry, &pkg);

    let expected_key = StableRef::new(lineage(), router_ws_id);

    // The label alias "Router::with_state" must resolve to router_ws_id.
    assert_eq!(
        doc_link_table.resolve("Router::with_state"),
        Some(&expected_key),
        "segment-level fallback must pick router_ws_id ('root.Router.with_state') \
             over handler_ws_id ('root.Handler.with_state') when the label is \
             'Router::with_state'; neither stored path ends with 'root.with_state' \
             so the exact suffix check must fail and the fallback must fire"
    );

    // The leaf "with_state" must also resolve to the same entry (stored
    // as alias (c) during the successful segment-level resolution).
    assert_eq!(
        doc_link_table.resolve("with_state"),
        Some(&expected_key),
        "bare leaf must also resolve to router_ws_id after successful \
             segment-level fallback"
    );
}
