//! Unit tests for the documentation walk: sections, plan, and link resolution.
// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

use std::path::PathBuf;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{DocLink, Entry, Node, Symbol, Visibility},
    kind::Kind,
    kinds::Module,
    view::IrView,
};
use crate::store::package::{PackageView, Provenance};

use super::doc_link_table::DocLinkTable;
use super::prose::is_symbol_path;
use super::walk_doc;
use crate::wire::{
    InlineRun, LinkOrigin, LinkRepair, LinkRepairKind, LinkTarget, ProseBlock, RenderSection,
    SectionId,
};

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

// ── expand_shortcut_links: DELETED, deliberately ───────────────────────
//
// `expand_shortcut_links` was a string-level shortcut expander that
// `build_prose_blocks` did not call — it was `#[allow(dead_code)]` and
// documented as such — yet it compiled, and it constructed
// `InlineRun::Link` on its own terms: no `DelimiterShape`, no `LinkOrigin`,
// no repair verdict. That made it a second, live way to mint a resolved
// link with no record of whether its spelling was the author's or ours,
// which is exactly the bypass the `shortcut_link` chokepoint exists to
// forbid. It and its eight unit tests were removed together: a test for a
// function nothing calls is not coverage, and keeping them would have
// meant keeping the bypass alive to satisfy them.
//
// The behaviours they pinned are all covered against the real pipeline
// below (bracket stripping, callout leads, backtick shortcuts, strong
// wrapping) by the `walk_doc` integration tests and the L17 adversarial
// suite, which exercise the path the reader actually gets.

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
            origin,
        } => {
            assert_eq!(
                &**text, "child_fn",
                "link text must be the inner shortcut text"
            );
            assert_eq!(
                key.intro, child_id,
                "link must point at the child function's IntroId"
            );
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "`[child_fn]` is rustdoc's own syntax — marking it as repaired \
                 would put a 'we changed this' badge on an ordinary link"
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
            origin,
        } => {
            assert_eq!(
                &**text, "child_fn",
                "link text must be the inner shortcut text (without backticks)"
            );
            assert_eq!(
                key.intro, child_id,
                "link must point at the child function's IntroId"
            );
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "`` [`child_fn`] `` is rustdoc's documented spelling, not a repair"
            );
        }
        other => panic!("expected Link with Symbol target, got {other:?}"),
    }
}

/// L17 regression: an unresolved, path-shaped shortcut on a symbol that
/// *did* declare doc links (just not this one) must still have its
/// brackets stripped — the "declared links present but this shortcut
/// doesn't resolve" side of the `has_declared_links()` gate documented on
/// `is_bracket_open` in `prose.rs`.
///
/// The fixture declares one doc link unrelated to `[SomeType]` (target
/// `"unrelated"`) purely so `has_declared_links()` is `true`; that is what
/// routes `[SomeType]` through `consume_bracket_run` instead of the
/// literal-prose path, and that function's failure-to-resolve branch never
/// re-emits the `[`/`]` delimiters. The zero-declared-links side (where
/// `[SomeType]` would instead survive verbatim) is a *different* behaviour,
/// pinned separately by `undeclared_symbol_preserves_note_index_and_explicit_link_verbatim`
/// below and by `empty_doc_links_preserves_bracketed_prose_verbatim` in
/// `tests/hyperlink_flows.rs`.
#[test]
fn walk_doc_undeclared_bracket_text_strips_brackets_on_no_match() {
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();

    let root_sym = sym_with_doc_links(
        "root",
        "See [SomeType] for info.",
        vec![DocLink {
            // Deliberately unrelated to `[SomeType]` — it exists only to
            // put `has_declared_links()` on the `true` side of the gate.
            target: "unrelated".to_owned(),
            label: Some("unrelated".to_owned()),
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

    let full_text: String = paragraph_runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        full_text, "See SomeType for info.",
        "an undeclared, unresolved shortcut link must have its brackets \
             stripped, not preserved verbatim; got: {full_text:?}"
    );
    assert!(
        !paragraph_runs.iter().any(|r| matches!(
            r,
            InlineRun::Text { text } | InlineRun::Code { text }
                if text.contains('[') || text.contains(']')
        )),
        "no run may contain a literal bracket character; got: {paragraph_runs:#?}"
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
            origin,
        } => {
            assert_eq!(&**text, "Router::with_state");
            assert_eq!(key.intro, fn_id);
            assert_eq!(*origin, LinkOrigin::Authored);
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

// ── L17 adversarial regression suite ───────────────────────────────────────
//
// L17: an unresolved intra-doc link must never leak raw `[`/`]` markdown to
// the reader. These exercise the full `walk_doc` pipeline (not just
// `DocLinkTable`, and not the string-level `expand_shortcut_links` helper
// that used to live in `prose.rs` and has since been deleted — see the note
// above where its tests were)
// because that is exactly where the original bug lived: pulldown-cmark
// tokenises `[Foo]` into separate events, and only the event-stream path in
// `prose.rs` sees what the reader will actually see.

/// Build a single-symbol root package with the given doc comment and
/// `doc_links`, with no children. Sufficient for every adversarial case
/// below, none of which resolve to a local symbol.
fn build_pkg_with_root_doc(doc: &str, doc_links: Vec<DocLink>) -> (PackageView, IntroId) {
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();
    let root_sym = sym_with_doc_links("root", doc, doc_links);
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
    (pkg, root_id)
}

/// Run `walk_doc` and return the first paragraph's `InlineRun`s.
fn first_paragraph_runs(pkg: &PackageView, root_id: IntroId) -> Vec<InlineRun> {
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

    match prose_section.first().expect("must have blocks") {
        ProseBlock::Paragraph { runs } => runs.clone(),
        other => panic!("expected Paragraph, got {other:?}"),
    }
}

/// The one invariant every adversarial case must satisfy: no run's text
/// contains a literal `[` or `]`, in *any* run kind (a leaked bracket could
/// show up as `Text`, `Code`, `Strong`, or `Em`).
fn assert_no_leaked_brackets(runs: &[InlineRun]) {
    for run in runs {
        let text: &str = match run {
            InlineRun::Text { text }
            | InlineRun::Code { text }
            | InlineRun::Strong { text }
            | InlineRun::Em { text } => text,
            InlineRun::Link { text, .. } => text,
        };
        assert!(
            !text.contains('[') && !text.contains(']'),
            "run leaked raw markdown brackets: {run:?} in {runs:#?}"
        );
    }
}

/// A shortcut link whose target isn't in the doc-link table at all (the
/// symbol declared *some* links, just not this one) must render as plain
/// text with the brackets stripped — never as literal `[NoSuchThing]`.
#[test]
fn adversarial_no_target_strips_brackets() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [NoSuchThing] here.",
        vec![DocLink {
            target: "unrelated".to_owned(),
            label: Some("unrelated".to_owned()),
        }],
    );
    let runs = first_paragraph_runs(&pkg, root_id);
    assert_no_leaked_brackets(&runs);

    let text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("NoSuchThing"),
        "the symbol name itself must survive, only the brackets are stripped; got {text:?}"
    );
}

/// Nested brackets (`[[Inner]]`) must not leak any bracket character, even
/// though the outer pair is not a well-formed single shortcut link.
///
/// The symbol declares one unrelated link (target `"unrelated"`) so
/// `has_declared_links()` is `true` and `[[Inner]]` is scanned as a link
/// *attempt* by `consume_bracket_run` rather than passed through as literal
/// prose — the case this test is actually meant to exercise.
#[test]
fn adversarial_nested_brackets_leak_nothing() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [[Inner]] end.",
        vec![DocLink {
            target: "unrelated".to_owned(),
            label: Some("unrelated".to_owned()),
        }],
    );
    let runs = first_paragraph_runs(&pkg, root_id);
    assert_no_leaked_brackets(&runs);
}

/// A stray `]` appearing after a resolved/stripped bracket span (`[a]b]`)
/// must not leak either — both the inner link-shaped span and the leftover
/// closer must be handled without emitting punctuation.
///
/// The symbol declares one unrelated link so `has_declared_links()` is
/// `true` and `[a]` is scanned as a link attempt (see
/// `adversarial_nested_brackets_leak_nothing` above for why this matters).
#[test]
fn adversarial_text_contains_close_bracket_leaks_nothing() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [a]b] end.",
        vec![DocLink {
            target: "unrelated".to_owned(),
            label: Some("unrelated".to_owned()),
        }],
    );
    let runs = first_paragraph_runs(&pkg, root_id);
    assert_no_leaked_brackets(&runs);
}

/// An empty shortcut link `[]` carries no reader-facing content and must
/// produce no bracket characters (and, in particular, no empty `Link` or
/// `Text` run standing in for nothing).
///
/// The symbol declares one unrelated link so `has_declared_links()` is
/// `true` and `[]` is scanned as a link attempt (see
/// `adversarial_nested_brackets_leak_nothing` above for why this matters).
#[test]
fn adversarial_empty_link_leaks_nothing() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [] end.",
        vec![DocLink {
            target: "unrelated".to_owned(),
            label: Some("unrelated".to_owned()),
        }],
    );
    let runs = first_paragraph_runs(&pkg, root_id);
    assert_no_leaked_brackets(&runs);

    let text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert_eq!(
        text.trim(),
        "See  end.".trim(),
        "an empty link must vanish, not leave stray punctuation; got {text:?}"
    );
}

/// A CommonMark reference-style link `[text][ref]` with no reference
/// definition (rustdoc doesn't have these; we simply must not choke on one)
/// must not leak brackets from either bracket pair.
///
/// The symbol declares one unrelated link so `has_declared_links()` is
/// `true` and `[text][ref]` is scanned as a link attempt (see
/// `adversarial_nested_brackets_leak_nothing` above for why this matters).
#[test]
fn adversarial_reference_style_link_leaks_nothing() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [text][ref] end.",
        vec![DocLink {
            target: "unrelated".to_owned(),
            label: Some("unrelated".to_owned()),
        }],
    );
    let runs = first_paragraph_runs(&pkg, root_id);
    assert_no_leaked_brackets(&runs);

    // `ref` is plumbing (like a URL), not reader content — it must not
    // appear in the rendered text any more than a link's URL would.
    let text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(
        !text.contains("ref"),
        "the reference id must be dropped as plumbing, not shown; got {text:?}"
    );
}

/// A link to a symbol in another crate (not in this package's index) must
/// render as plain text without brackets — the exact `L17` screenshot case
/// (`memrchr_iter` aside; see the dedicated regression test below).
#[test]
fn adversarial_cross_crate_link_leaks_nothing() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [std::vec::Vec] end.",
        vec![DocLink {
            target: "std::vec::Vec".to_owned(),
            label: Some("std::vec::Vec".to_owned()),
        }],
    );
    let runs = first_paragraph_runs(&pkg, root_id);
    assert_no_leaked_brackets(&runs);

    let text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("std::vec::Vec"),
        "the path text itself must survive; got {text:?}"
    );
    assert!(
        !runs.iter().any(|r| matches!(r, InlineRun::Link { .. })),
        "a target outside this package's index must not become a Link; got {runs:#?}"
    );
}

/// L17 case study, verbatim: the real `struct Memchr<'h>` doc comment from
/// `result/memchr-2.8.3/src/memchr.rs:282`, backtick/bracket typo and
/// all. Reproduces the screenshot evidence directly rather than a
/// paraphrase of it.
///
/// `memchr_iter` (well-formed `` [`memchr_iter`] ``) and `Memchr::new`
/// (same shape) must resolve to `Link`s, as they always did. `memrchr_iter`
/// — written as `` `[memrchr_iter`] ``, backtick before the bracket — must
/// *also* produce no leaked bracket, whether or not it manages to resolve
/// to a `Link` (see `doc_link_table.rs`'s module docs for why the producer
/// resolves it fine but the old renderer never asked).
#[test]
fn real_memchr_doc_comment_leaks_no_bracket_for_any_link() {
    let root_id = intro(1);
    let memchr_iter_id = intro(2);
    let memrchr_iter_id = intro(3);
    let new_id = intro(4);
    let mut table = PristineIntroTable::new();

    for (id, name) in [
        (memchr_iter_id, "memchr_iter"),
        (memrchr_iter_id, "memrchr_iter"),
        (new_id, "new"),
    ] {
        table.insert_live(
            id,
            Entry::new(
                Symbol {
                    name: name.to_owned(),
                    visibility: Visibility::Public,
                    documentation: String::new(),
                    source: PathBuf::new(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                },
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            Some(root_id),
        );
    }

    // Verbatim from memchr.rs:282-283 (the doc comment on `struct Memchr`).
    // The producer's byte-level `extract_doc_link_targets` finds and
    // resolves all three targets regardless of the backtick/bracket typo
    // (see doc_link_table.rs), so all three are declared here exactly as
    // the real Rust producer would emit them.
    let doc = "This iterator is created by the [`memchr_iter`] or `[memrchr_iter`]\n\
               functions. It can also be created with the [`Memchr::new`] method.";
    let root_sym = sym_with_doc_links(
        "Memchr",
        doc,
        vec![
            DocLink {
                target: "memchr_iter".to_owned(),
                label: Some("memchr_iter".to_owned()),
            },
            DocLink {
                target: "memrchr_iter".to_owned(),
                label: Some("memrchr_iter".to_owned()),
            },
            DocLink {
                target: "Memchr::new".to_owned(),
                label: Some("Memchr::new".to_owned()),
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
    let runs = first_paragraph_runs(&pkg, root_id);

    assert_no_leaked_brackets(&runs);

    let links: Vec<(&str, IntroId, &LinkOrigin)> = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Link {
                text,
                target: LinkTarget::Symbol { key },
                origin,
            } => Some((&**text, key.intro, origin)),
            _ => None,
        })
        .collect();

    assert!(
        links.iter().any(|&(_, id, _)| id == memchr_iter_id),
        "well-formed [`memchr_iter`] must resolve to a Link as before; got {runs:#?}"
    );
    assert!(
        links.iter().any(|&(_, id, _)| id == new_id),
        "well-formed [`Memchr::new`] must resolve to a Link as before; got {runs:#?}"
    );
    assert!(
        links.iter().any(|&(_, id, _)| id == memrchr_iter_id),
        "the malformed `[memrchr_iter`] must ALSO resolve to a Link now that \
             the renderer recognises the swapped backtick/bracket shape as a \
             link attempt; got {runs:#?}"
    );

    // The headline assertion. It is not enough that all three resolve — two of
    // them are rustdoc's own spelling and one is a typo we corrected, and the
    // reader must be able to tell which is which. Before `LinkOrigin` existed
    // the three runs were byte-identical in kind, which is what made the
    // repair silent.
    for &(text, id, origin) in &links {
        let expected_authored = id == memchr_iter_id || id == new_id;
        if expected_authored {
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "{text:?} is spelled the way rustdoc specifies; it must not be \
                 recorded as a repair"
            );
        } else {
            assert_eq!(
                origin.repair_kind(),
                Some(LinkRepairKind::TransposedOpenDelimiter),
                "{text:?} came from the transposed `` `[foo`] `` spelling and \
                 must be recorded as that repair, not passed off as authored"
            );
        }
    }
}

/// The repair must carry the **author's bytes**, the resolved target, and a
/// reader-facing sentence — not merely a flag saying "something happened".
///
/// # Why this test is the point of the whole change
///
/// `real_memchr_doc_comment_leaks_no_bracket_for_any_link` (above) proves the
/// repair *happens*. This one proves it is *recorded* well enough to be shown
/// to a reader and counted in an audit. If the evidence were a reconstruction
/// (`[memrchr_iter]`, the spelling the author *should* have used) it would be
/// worse than useless: it would tell the reader we changed nothing.
///
/// It fails against a tree without `LinkOrigin` in the strongest possible way
/// — the type does not exist.
#[test]
fn repaired_link_records_its_kind_raw_and_resolved() {
    let root_id = intro(1);
    let memchr_iter_id = intro(2);
    let memrchr_iter_id = intro(3);
    let new_id = intro(4);
    let mut table = PristineIntroTable::new();

    for (id, name) in [
        (memchr_iter_id, "memchr_iter"),
        (memrchr_iter_id, "memrchr_iter"),
        (new_id, "new"),
    ] {
        table.insert_live(
            id,
            Entry::new(
                Symbol {
                    name: name.to_owned(),
                    visibility: Visibility::Public,
                    documentation: String::new(),
                    source: PathBuf::new(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                },
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            Some(root_id),
        );
    }

    // Verbatim from memchr-2.8.3/src/memchr.rs:282-283, typo and all.
    let doc = "This iterator is created by the [`memchr_iter`] or `[memrchr_iter`]\n\
               functions. It can also be created with the [`Memchr::new`] method.";
    let root_sym = sym_with_doc_links(
        "Memchr",
        doc,
        vec![
            DocLink {
                target: "memchr_iter".to_owned(),
                label: Some("memchr_iter".to_owned()),
            },
            DocLink {
                target: "memrchr_iter".to_owned(),
                label: Some("memrchr_iter".to_owned()),
            },
            DocLink {
                target: "Memchr::new".to_owned(),
                label: Some("Memchr::new".to_owned()),
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
    let runs = first_paragraph_runs(&pkg, root_id);

    let repaired: Vec<&LinkRepair> = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Link {
                origin: LinkOrigin::Repaired(repair),
                ..
            } => Some(repair),
            _ => None,
        })
        .collect();

    assert_eq!(
        repaired.len(),
        1,
        "exactly one of the three links in this doc comment is malformed; \
         got {runs:#?}"
    );
    let LinkRepair {
        kind,
        raw,
        resolved,
        note,
    } = repaired[0];

    assert_eq!(*kind, LinkRepairKind::TransposedOpenDelimiter);
    assert_eq!(
        &**raw, "`[memrchr_iter`]",
        "the evidence must be the author's bytes, not our reconstruction"
    );
    assert_eq!(&**resolved, "memrchr_iter");
    assert!(
        note.contains("`[memrchr_iter`]"),
        "the reader must be able to see the original spelling; got {note:?}"
    );
    assert!(
        note.contains("memrchr_iter"),
        "the reader must be able to see what we linked instead; got {note:?}"
    );

    // …and the other two are authored, pinned in the same test so neither
    // face can flip without a red test.
    let authored: Vec<&str> = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Link {
                text,
                origin: LinkOrigin::Authored,
                ..
            } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(
        authored.contains(&"memchr_iter"),
        "[`memchr_iter`] must be Authored; got {authored:?}"
    );
    assert!(
        authored.contains(&"Memchr::new"),
        "[`Memchr::new`] must be Authored; got {authored:?}"
    );
}

/// A link we call `Authored` must be a link the author actually spelled: its
/// text has to appear inside a canonical `[…]` / `` [`…`] `` span in the doc
/// comment. A `Repaired` link's `raw` has to appear in the doc comment
/// verbatim.
///
/// # Why this exists on top of the typed chokepoint
///
/// `shortcut_link` makes an unrecorded repair unconstructible, but only for
/// callers that go through it. This is the runtime backstop for a future
/// caller that does not: an `Authored` link whose text is not literally in the
/// source is a lie by definition, and this test says so without needing to
/// know how the lie was told.
#[test]
fn authored_link_text_appears_verbatim_in_the_doc_comment() {
    let root_id = intro(1);
    let memchr_iter_id = intro(2);
    let memrchr_iter_id = intro(3);
    let new_id = intro(4);
    let mut table = PristineIntroTable::new();

    for (id, name) in [
        (memchr_iter_id, "memchr_iter"),
        (memrchr_iter_id, "memrchr_iter"),
        (new_id, "new"),
    ] {
        table.insert_live(
            id,
            Entry::new(
                Symbol {
                    name: name.to_owned(),
                    visibility: Visibility::Public,
                    documentation: String::new(),
                    source: PathBuf::new(),
                    span: 0..0,
                    aliases: Box::new([]),
                    deprecation: None,
                    doc_links: Box::new([]),
                    attrs: Box::new([]),
                    cfg: None,
                },
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            Some(root_id),
        );
    }

    let doc = "This iterator is created by the [`memchr_iter`] or `[memrchr_iter`]\n\
               functions. It can also be created with the [`Memchr::new`] method.";
    let root_sym = sym_with_doc_links(
        "Memchr",
        doc,
        vec![
            DocLink {
                target: "memchr_iter".to_owned(),
                label: Some("memchr_iter".to_owned()),
            },
            DocLink {
                target: "memrchr_iter".to_owned(),
                label: Some("memrchr_iter".to_owned()),
            },
            DocLink {
                target: "Memchr::new".to_owned(),
                label: Some("Memchr::new".to_owned()),
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
    let runs = first_paragraph_runs(&pkg, root_id);

    let mut checked = 0usize;
    for run in &runs {
        let InlineRun::Link {
            text,
            target: LinkTarget::Symbol { .. },
            origin,
        } = run
        else {
            continue;
        };
        checked += 1;
        match origin {
            LinkOrigin::Authored => {
                let bare = format!("[{text}]");
                let ticked = format!("[`{text}`]");
                assert!(
                    doc.contains(&bare) || doc.contains(&ticked),
                    "an Authored link's text must appear in the source inside a \
                     canonical bracket span; {text:?} does not appear as {bare:?} \
                     or {ticked:?} in the doc comment"
                );
            }
            LinkOrigin::Repaired(repair) => {
                assert!(
                    doc.contains(&*repair.raw),
                    "a Repaired link's `raw` must be the author's bytes and so \
                     must appear in the doc comment verbatim; {:?} does not",
                    repair.raw
                );
            }
        }
    }
    assert_eq!(checked, 3, "all three symbol links must be checked");
}

// ── `has_declared_links()` gate — both sides pinned explicitly ─────────────
//
// The gate documented on `is_bracket_open` in `prose.rs` is a heuristic that
// selects between two entire behaviours per symbol (see that doc comment
// for the full contract). The two tests below pin each side directly so a
// future edit to the gate cannot silently flip one without a red test.
//
// `rstest` would be the natural tool for a parameterised version of this
// (doctrine §4), but it is not a dependency of `nudox-engine` today (`grep
// -rn rstest --include=Cargo.toml` finds nothing in this crate or the root
// `[workspace.dependencies]`; the one existing user,
// `tests/generic_signature_shapes.rs`, made the same finding and chose a
// hand-rolled table for the same reason). Adding it would mean editing
// `Cargo.toml`, outside this task's scope (only `prose.rs`, `tests.rs`,
// `tests/hyperlink_flows.rs`, `docs/LIMITATIONS.md`). It would also buy little
// here regardless: the two tests below pin two *qualitatively different*
// scenarios (zero declared links vs. one declared link with a resolving and
// a non-resolving shortcut side by side), not the same assertion repeated
// over a varying input — the case `rstest`'s `#[case]` is built for.

/// Zero declared links: every kind of bracket-shaped prose must survive
/// untouched — `[NOTE]` (a callout/citation marker), `arr[0]` (an indexing
/// expression, not markdown) — and a genuine, well-formed CommonMark link
/// `[text](url)` must still work as a real `Url` link.
///
/// The third case is not actually gated by `has_declared_links()` at all:
/// pulldown-cmark recognises well-formed inline link syntax as `Tag::Link`
/// during its own CommonMark parse, before any event reaches
/// `is_bracket_open` — that predicate only ever sees the *left-over* bracket
/// events for text pulldown-cmark could **not** resolve into a real link
/// (shortcuts, unmatched brackets). So this case proves the gate's
/// "zero declared links" side does not accidentally disable real markdown
/// links too, only the ambiguous shortcut case it exists to police.
#[test]
fn undeclared_symbol_preserves_note_index_and_explicit_link_verbatim() {
    let (pkg, root_id) = build_pkg_with_root_doc(
        "See [NOTE], arr[0], and [text](https://example.com/) for more.",
        vec![], // zero declared doc links
    );
    let runs = first_paragraph_runs(&pkg, root_id);

    let text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("[NOTE]"),
        "a symbol with zero declared links must preserve [NOTE] verbatim, \
             brackets included; got {text:?}"
    );
    assert!(
        text.contains("arr[0]"),
        "a symbol with zero declared links must preserve arr[0] verbatim, \
             brackets included; got {text:?}"
    );

    let url_link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Url { .. },
                ..
            }
        )
    });
    match url_link.expect(
        "a well-formed [text](url) link must still produce a Url link even \
             when the symbol declared zero doc links",
    ) {
        InlineRun::Link {
            text,
            target: LinkTarget::Url { url },
            origin,
        } => {
            assert_eq!(&**text, "text", "link text must be 'text'");
            assert!(
                url.contains("example.com"),
                "URL must be preserved; got {url:?}"
            );
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "a well-formed CommonMark [text](url) link is authored by \
                 definition — pulldown-cmark only emits Tag::Link for spellings \
                 the spec accepts"
            );
        }
        other => panic!("expected Link(Url), got {other:?}"),
    }

    assert!(
        !runs.iter().any(|r| matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )),
        "zero declared links must never fabricate a Symbol link; got {runs:#?}"
    );
}

/// One declared link: a paragraph containing both a shortcut that resolves
/// and one that does not must produce an `InlineRun::Link` for the first
/// and a bracket-free `InlineRun::Text` for the second — the two faces of
/// the gate, pinned together so neither can silently flip without failing
/// this one test.
#[test]
fn declared_symbol_resolves_matching_shortcut_and_strips_nonmatching_one() {
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

    let root_sym = sym_with_doc_links(
        "root",
        "See [child_fn] and [NoSuchThing] here.",
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
    let runs = first_paragraph_runs(&pkg, root_id);

    assert_no_leaked_brackets(&runs);

    let link = runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .expect("the resolving shortcut [child_fn] must produce a Link run");
    match link {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
            origin,
        } => {
            assert_eq!(&**text, "child_fn", "link text must be 'child_fn'");
            assert_eq!(
                key.intro, child_id,
                "link must point at child_fn's IntroId"
            );
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "`[child_fn]` is the canonical spelling; only the transposed \
                 `` `[foo`] `` shape is a repair"
            );
        }
        other => panic!("expected Link(Symbol), got {other:?}"),
    }

    let text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("NoSuchThing"),
        "the non-resolving shortcut's inner text must survive; got {text:?}"
    );
    assert!(
        !text.contains('[') && !text.contains(']'),
        "the non-resolving shortcut must have its brackets stripped (this \
             symbol DID declare a doc link, just not for NoSuchThing); got {text:?}"
    );
}
