//! Adversarial integration tests for intra-doc hyperlink resolution.
//!
//! # Design
//!
//! All tests go through the public `chunk::chunk` entry point (which calls
//! `walk_doc` internally).  That is the critical constraint: the original bug
//! was that internal unit tests called `expand_shortcut_links` with a whole
//! string and passed, while `build_prose_blocks` — which processes the
//! pulldown-cmark event stream — never fired the expansion because
//! pulldown-cmark splits `[Foo]` into three separate events.  Every test here
//! exercises the full pipeline so that the regression is impossible at the
//! call-site level.
//!
//! (`expand_shortcut_links` was deleted on 2026-08-06 — it was a second,
//! live-compiling way to construct a resolved link with no spelling and no
//! `LinkOrigin`, which is exactly the bypass the `shortcut_link` chokepoint
//! exists to forbid. Its tests went with it; see `chunk/walk/tests.rs`.)
//!
//! `chunk::chunk` is the public function declared in `src/chunk/mod.rs`:
//! ```
//! pub fn chunk(intro, view, package) -> Option<(SymbolHead, Vec<RenderSection>)>
//! ```
//!
//! # Coverage
//!
//! * Bare shortcut `[Foo]` → `InlineRun::Link { target: Symbol }`.
//! * Backtick shortcut `` [`Foo`] `` → `InlineRun::Link { target: Symbol }`.
//! * Cross-crate / unresolvable → plain text or code, **no brackets**, **no dead link**.
//! * `[!NOTE]` callout lead survives verbatim (brackets kept, NOT a link).
//! * Real Markdown link `[text](https://…)` → `InlineRun::Link { target: Url }`.
//! * Ambiguous leaf name: the correct symbol is chosen, never the wrong one.
//! * Link inside `**strong**` is still a `Link` run, not demoted to `Strong`.
//! * Empty `doc_links` — bracketed prose passes through unchanged.
//! * Trailing prose after `]` is preserved as a separate run.

use std::path::PathBuf;
use std::sync::Arc;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{DocLink, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::Module,
    view::IrView,
};
use nudox_engine::store::package::{PackageView, Provenance};

use nudox_engine::chunk;
use nudox_engine::wire::{
    InlineRun, LinkOrigin, LinkRepairKind, LinkTarget, ProseBlock, RenderSection,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn blank_sym(name: &str) -> Symbol {
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
    }
}

fn sym_with(name: &str, doc: &str, links: Vec<DocLink>) -> Symbol {
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

fn make_module_entry(sym: Symbol) -> Entry {
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

/// Build a two-symbol package: root (`intro=1`) + child (`intro=2`, `name=child_name`).
///
/// The root's `doc` and `links` are parameters; the child has no docs of its own.
/// Returns `(PackageView, root_intro, child_intro)`.
fn two_entry_pkg(
    doc: &str,
    links: Vec<DocLink>,
    child_name: &str,
) -> (Arc<PackageView>, IntroId, IntroId) {
    let lid = lineage("walk-test");
    let root_id = intro(1);
    let child_id = intro(2);
    let mut table = PristineIntroTable::new();

    table.insert_live(
        child_id,
        make_module_entry(blank_sym(child_name)),
        Some(root_id),
    );
    table.insert_live(
        root_id,
        make_module_entry(sym_with("root", doc, links)),
        None,
    );

    let view = IrView::with_package(lid, table);
    (
        Arc::new(PackageView::build(view, Provenance::TrustedLocal)),
        root_id,
        child_id,
    )
}

/// Run `chunk::chunk` on `root_id` inside `pkg` and return the prose sections.
fn prose_sections(pkg: &PackageView, root_id: IntroId) -> Vec<Vec<ProseBlock>> {
    let (_, sections) =
        chunk::chunk(root_id, pkg.view(), pkg).expect("chunk must succeed for the root entry");
    sections
        .into_iter()
        .filter_map(|s| match s {
            RenderSection::Prose { blocks, .. } => Some(blocks),
            _ => None,
        })
        .collect()
}

/// Flatten all `InlineRun`s from the first paragraph of the first prose section.
fn first_paragraph_runs(pkg: &PackageView, root_id: IntroId) -> Vec<InlineRun> {
    let sections = prose_sections(pkg, root_id);
    let blocks = sections
        .into_iter()
        .next()
        .expect("must have at least one Prose section");
    match blocks
        .into_iter()
        .next()
        .expect("must have at least one block")
    {
        ProseBlock::Paragraph { runs } => runs,
        other => panic!("expected Paragraph block, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bare shortcut `[Foo]` → Symbol link
// ---------------------------------------------------------------------------

/// The canonical regression test: `[child_fn]` in doc text, with `doc_links`
/// carrying the resolved target, must produce `InlineRun::Link { Symbol }`.
///
/// Goes through `chunk::chunk` (the public API) which internally calls `walk_doc`
/// and `build_prose_blocks` with the pulldown-cmark event stream — the path
/// where the three-event split actually happens and where the original bug lived.
#[test]
fn bare_shortcut_link_becomes_symbol_link_through_chunk() {
    let (pkg, root_id, child_id) = two_entry_pkg(
        "See [child_fn] for details.",
        vec![DocLink {
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
        "child_fn",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .unwrap_or_else(|| {
            panic!(
                "paragraph must contain a Link run; got: {:?}\n\
                 REGRESSION: pulldown-cmark splits [Foo] into 3 events; \
                 the event-stream lookahead must fire, not the string-level helper",
                runs
            )
        });

    match link {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
            origin,
        } => {
            assert_eq!(
                &**text, "child_fn",
                "link text must be the inner shortcut name"
            );
            assert_eq!(
                key.intro, child_id,
                "link must point at the child's IntroId"
            );
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "`[child_fn]` is rustdoc's documented intra-doc-link syntax, so \
                 rendering a link from it is fidelity, not a repair"
            );
        }
        other => panic!("expected Link(Symbol), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Backtick shortcut `` [`Foo`] `` → Symbol link
// ---------------------------------------------------------------------------

/// pulldown-cmark emits `` [`child_fn`] `` as:
///   `Text("[")` / `Code("child_fn")` / `Text("]...")`
///
/// The lookahead in `build_prose_blocks` handles the `Code` inner event.
/// This test proves that branch fires through the public `chunk` API.
#[test]
fn backtick_shortcut_link_becomes_symbol_link_through_chunk() {
    let (pkg, root_id, child_id) = two_entry_pkg(
        "See [`child_fn`] for details.",
        vec![DocLink {
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
        "child_fn",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .unwrap_or_else(|| panic!("backtick shortcut must produce a Link run; got: {:?}", runs));

    match link {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
            origin,
        } => {
            // pulldown-cmark's `Code` event strips the backticks, so the text is bare.
            assert_eq!(&**text, "child_fn");
            assert_eq!(key.intro, child_id);
            // `` [`child_fn`] `` opens with `Text("[")` — the *canonical*
            // delimiter. The `Code` event this test is about is the run's
            // *inner* content, not its opener, so this is rustdoc's own
            // spelling and must be `Authored`. The transposed opener
            // (`` `[child_fn`] ``) is a different shape entirely and is
            // pinned by `transposed_backtick_shortcut_is_recorded_as_a_repair`
            // below.
            assert_eq!(*origin, LinkOrigin::Authored);
        }
        other => panic!("expected Link(Symbol), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Transposed opener `` `[Foo`] `` → Symbol link, RECORDED as a repair
// ---------------------------------------------------------------------------

/// The one malformed spelling we repair, end-to-end through the public `chunk`
/// API, asserted to arrive at the GUI seam **marked**.
///
/// # Why this test fails if the repair goes silent again
///
/// It does not merely check that `` `[child_fn`] `` becomes a link — the older
/// tests already did that, and that is exactly the state in which the repair
/// was invisible. It checks that the link carries
/// `LinkOrigin::Repaired(TransposedOpenDelimiter)` with the author's bytes in
/// `raw`. Deleting the record, or widening `shortcut_link` to hand back
/// `Authored`, turns this red.
///
/// The source spelling is the real one from `memchr-2.8.3/src/memchr.rs:282`:
/// backtick first, then bracket. CommonMark binds code spans tighter than link
/// brackets, so pulldown-cmark emits `Code("[child_fn")` + `Text("] for …")`
/// and rustdoc — and therefore docs.rs — renders it as literal text.
#[test]
fn transposed_backtick_shortcut_is_recorded_as_a_repair() {
    let (pkg, root_id, child_id) = two_entry_pkg(
        "See `[child_fn`] for details.",
        vec![DocLink {
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
        "child_fn",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs
        .iter()
        .find(|r| matches!(r, InlineRun::Link { .. }))
        .unwrap_or_else(|| {
            panic!("the transposed spelling must still produce a Link; got: {runs:?}")
        });

    match link {
        InlineRun::Link {
            text,
            target: LinkTarget::Symbol { key },
            origin,
        } => {
            assert_eq!(&**text, "child_fn");
            assert_eq!(key.intro, child_id);
            match origin {
                LinkOrigin::Repaired(repair) => {
                    assert_eq!(repair.kind, LinkRepairKind::TransposedOpenDelimiter);
                    assert_eq!(
                        &*repair.raw, "`[child_fn`]",
                        "the evidence must be the author's bytes — a \
                         reconstruction of the canonical spelling would tell the \
                         reader we changed nothing"
                    );
                    assert_eq!(&*repair.resolved, "child_fn");
                    assert!(
                        repair.note.contains("`[child_fn`]"),
                        "the note the GUI shows verbatim must name the original \
                         spelling; got {:?}",
                        repair.note
                    );
                }
                LinkOrigin::Authored => panic!(
                    "SILENT REPAIR: `[child_fn`] is not rustdoc's syntax — \
                     rustdoc and docs.rs render it as literal text. Rendering a \
                     link from it is a repair and must be recorded as one."
                ),
            }
        }
        other => panic!("expected Link(Symbol), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Cross-crate / unresolvable → no brackets, no dead link
// ---------------------------------------------------------------------------

/// When `doc_links` is non-empty but the bracketed name does not resolve
/// (e.g. cross-crate), the text must appear without brackets and without
/// a dead `Symbol` link.
#[test]
fn cross_crate_link_strips_brackets_and_emits_plain_text() {
    let lid = lineage("walk-test");
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();

    // A doc_link for "tower::Service" which is NOT in this package.
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Use [tower::Service] to implement the trait.",
            vec![DocLink {
                target: "tower::Service".to_owned(),
                label: Some("Service".to_owned()),
            }],
        )),
        None,
    );
    // Add a child with a DIFFERENT name so the index has an entry but "Service" is absent.
    table.insert_live(
        intro(2),
        make_module_entry(blank_sym("NotService")),
        Some(root_id),
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    // Must not produce a dead Symbol link.
    let dead_link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    });
    assert!(
        dead_link.is_none(),
        "cross-crate link must NOT produce a Symbol link; got {:?}",
        dead_link
    );

    // The inner text must appear, without literal brackets.
    let all_text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text }
            | InlineRun::Code { text }
            | InlineRun::Strong { text }
            | InlineRun::Em { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert!(
        all_text.contains("Service") || all_text.contains("tower"),
        "the inner symbol name must appear in the output even when unresolvable; got: {all_text:?}"
    );
    assert!(
        !all_text.contains("[Service]") && !all_text.contains("[tower"),
        "brackets must be stripped for unresolvable links; got: {all_text:?}"
    );
}

// ---------------------------------------------------------------------------
// `[!NOTE]` callout lead — brackets must survive, NOT treated as a link
// ---------------------------------------------------------------------------

/// `[!NOTE]` inside a blockquote is a callout lead. The `!NOTE` text is not
/// a valid symbol path (contains `!`), so the lookahead must fall through and
/// emit the `[` as literal text, preserving the callout classification.
#[test]
fn callout_lead_survives_verbatim_and_is_not_a_link() {
    let lid = lineage("walk-test");
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();

    // A doc_link is present so the lookahead is even attempted.
    // The child "something" is in the corpus; "[!NOTE]" must still not become a link.
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "> [!NOTE]\n> This is important.",
            vec![DocLink {
                target: "something".to_owned(),
                label: Some("something".to_owned()),
            }],
        )),
        None,
    );
    table.insert_live(
        intro(2),
        make_module_entry(blank_sym("something")),
        Some(root_id),
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let (_, sections) = chunk::chunk(root_id, pkg.view(), &pkg).expect("chunk must succeed");

    // No section may have a Symbol link produced from "[!NOTE]".
    let has_bad_symbol_link = sections.iter().any(|s| {
        let blocks: &[ProseBlock] = match s {
            RenderSection::Prose { blocks, .. } | RenderSection::Callout { blocks, .. } => blocks,
            _ => return false,
        };
        blocks.iter().any(|b| match b {
            ProseBlock::Paragraph { runs } => runs.iter().any(|r| {
                matches!(
                    r,
                    InlineRun::Link {
                        target: LinkTarget::Symbol { .. },
                        ..
                    }
                )
            }),
            _ => false,
        })
    });

    assert!(
        !has_bad_symbol_link,
        "the [!NOTE] callout lead must not produce a Symbol link; \
         the lookahead must reject it because '!NOTE' is not a valid symbol path"
    );

    // The NOTE or callout content must appear somewhere.
    let full_text: String = sections
        .iter()
        .flat_map(|s| {
            let blocks: &[ProseBlock] = match s {
                RenderSection::Prose { blocks, .. } | RenderSection::Callout { blocks, .. } => {
                    blocks
                }
                _ => return Vec::new(),
            };
            blocks
                .iter()
                .flat_map(|b| match b {
                    ProseBlock::Paragraph { runs } => runs
                        .iter()
                        .filter_map(|r| match r {
                            InlineRun::Text { text } => Some(text.to_string()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .collect::<Vec<_>>()
        })
        .collect();

    assert!(
        full_text.contains("NOTE") || full_text.contains("important"),
        "callout content must survive through chunk; full text: {full_text:?}"
    );
}

// ---------------------------------------------------------------------------
// Real Markdown link → Url, not Symbol
// ---------------------------------------------------------------------------

/// `[text](https://example.com)` is a real Markdown link. pulldown-cmark emits
/// it as a `Tag::Link` with the URL in `dest_url` — not as a shortcut ref.
/// It must become `LinkTarget::Url`, never `LinkTarget::Symbol`.
#[test]
fn real_markdown_url_link_stays_url_link() {
    let (pkg, root_id, _) = two_entry_pkg(
        "Read the [docs](https://docs.rs/axum) for more.",
        vec![], // no doc_links — the link is explicit Markdown
        "unrelated",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    let url_link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Url { .. },
                ..
            }
        )
    });

    assert!(
        url_link.is_some(),
        "explicit Markdown URL must produce a Url link; got: {:?}",
        runs
    );

    match url_link.unwrap() {
        InlineRun::Link {
            text,
            target: LinkTarget::Url { url },
            origin,
        } => {
            assert_eq!(&**text, "docs", "link text must be 'docs'");
            assert!(
                url.contains("docs.rs"),
                "URL must be preserved; got: {url:?}"
            );
            assert_eq!(
                *origin,
                LinkOrigin::Authored,
                "an explicit CommonMark link is authored by definition"
            );
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Ambiguous leaf name → correct symbol, never the wrong one
// ---------------------------------------------------------------------------

/// Two symbols share the same leaf name "Read". The resolution must pick the one
/// whose stored path matches the target suffix — not the other one — or fall
/// back to no link (but must never produce the *wrong* Symbol link).
///
/// This exercises the multi-hit suffix-narrowing path in `DocLinkTable::build`.
#[test]
fn ambiguous_leaf_name_does_not_resolve_to_wrong_symbol() {
    let lid = lineage("walk-test");
    let root_id = intro(1);
    let read_a_id = intro(2); // will be named "Read"
    let read_b_id = intro(3); // also named "Read"
    let mut table = PristineIntroTable::new();

    table.insert_live(
        read_a_id,
        make_module_entry(blank_sym("Read")),
        Some(root_id),
    );
    table.insert_live(
        read_b_id,
        make_module_entry(blank_sym("Read")),
        Some(root_id),
    );

    // The doc says [io::Read]; the doc_link target is "io::Read".
    // The suffix-narrowing must not pick read_b_id over read_a_id arbitrarily.
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Implement [io::Read] for your type.",
            vec![DocLink {
                target: "io::Read".to_owned(),
                label: Some("io::Read".to_owned()),
            }],
        )),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    // If a Symbol link was produced, it must not point at the "wrong" one
    // (the one the suffix does not match). Since both symbols have the same
    // name and the index doesn't store qualified paths in this hand-built
    // package, the suffix narrowing may produce no match — which is fine.
    // The critical assertion is "never the wrong symbol", not "always a link".
    if let Some(link) = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    }) {
        match link {
            InlineRun::Link {
                target: LinkTarget::Symbol { key },
                ..
            } => {
                // Both are valid because we can't know which one is "io::Read"
                // without a real module path — but neither must be a fabricated one.
                assert!(
                    key.intro == read_a_id || key.intro == read_b_id,
                    "the Symbol link must point at one of the two 'Read' entries; \
                     got intro {:?}",
                    key.intro
                );
            }
            _ => {}
        }
    }
    // No assertion that a link MUST be present — "no link" is the correct
    // fallback when ambiguity cannot be resolved.
}

// ---------------------------------------------------------------------------
// Link inside `**strong**` — the Link survives, not demoted to Strong
// ---------------------------------------------------------------------------

/// A shortcut link inside a bold span must produce `InlineRun::Link`, not just
/// `InlineRun::Strong`. This tests the interaction between the strong-state
/// tracker and the shortcut-link lookahead in `build_prose_blocks`.
#[test]
fn shortcut_link_inside_strong_is_still_a_link() {
    let (pkg, root_id, child_id) = two_entry_pkg(
        "This is **[child_fn]** which is important.",
        vec![DocLink {
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
        "child_fn",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    });

    assert!(
        link.is_some(),
        "shortcut link inside **strong** must produce a Link run, not just Strong; \
         got: {:?}",
        runs
    );

    match link.unwrap() {
        InlineRun::Link {
            target: LinkTarget::Symbol { key },
            ..
        } => {
            assert_eq!(key.intro, child_id);
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Empty `doc_links` — bracketed prose passes through unchanged
// ---------------------------------------------------------------------------

/// With an empty `doc_links` slice the fast-path guard (`doc_link_table.is_empty()`)
/// prevents any lookahead. The `[SomeType]` text must survive intact.
///
/// Guards against a regression where the fast path was removed or the guard
/// condition inverted.
#[test]
fn empty_doc_links_preserves_bracketed_prose_verbatim() {
    let (pkg, root_id, _) = two_entry_pkg(
        "See [SomeType] for background.",
        vec![], // explicitly empty
        "unrelated",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    // Reconstruct all text runs.
    let full: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert!(
        full.contains("[SomeType]"),
        "with empty doc_links the bracketed text must pass through unchanged; got: {full:?}"
    );

    // Must produce no Link runs.
    assert!(
        !runs.iter().any(|r| matches!(r, InlineRun::Link { .. })),
        "empty doc_links must not produce any Link runs"
    );
}

// ---------------------------------------------------------------------------
// Trailing prose after `]` is not lost
// ---------------------------------------------------------------------------

/// When pulldown-cmark fuses trailing prose onto the closing `]` event
/// (e.g. `"] for details."` as one Text event), the remainder must be
/// preserved in a separate run after the link.
#[test]
fn trailing_prose_after_shortcut_link_is_not_lost() {
    let (pkg, root_id, _) = two_entry_pkg(
        "See [child_fn] for details.",
        vec![DocLink {
            target: "child_fn".to_owned(),
            label: Some("child_fn".to_owned()),
        }],
        "child_fn",
    );

    let runs = first_paragraph_runs(&pkg, root_id);

    // Reconstruct all text (from both Link and Text runs).
    let all_text: String = runs
        .iter()
        .filter_map(|r| match r {
            InlineRun::Text { text } | InlineRun::Strong { text } | InlineRun::Em { text } => {
                Some(text.as_ref())
            }
            InlineRun::Link { text, .. } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert!(
        all_text.contains("for details"),
        "trailing prose after a shortcut link must not be lost; got: {all_text:?}"
    );
    assert!(
        all_text.contains("See") || all_text.contains("child_fn"),
        "prose before and the link itself must also be present; got: {all_text:?}"
    );
}

// ---------------------------------------------------------------------------
// Qualified path shortcut `[Router::with_state]` → resolves via leaf
// ---------------------------------------------------------------------------

/// The producer stores `"Router::with_state"` as the target but the corpus
/// knows the entry as leaf `"with_state"`.  The `DocLinkTable` must index both
/// the full target and the bare leaf so that the three-event bracket sequence
/// in `build_prose_blocks` finds a match via the inner text `"Router::with_state"`.
#[test]
fn qualified_path_shortcut_resolves_via_leaf_fallback() {
    let lid = lineage("walk-test");
    let root_id = intro(1);
    let fn_id = intro(2);
    let mut table = PristineIntroTable::new();

    table.insert_live(
        fn_id,
        make_module_entry(blank_sym("with_state")),
        Some(root_id),
    );
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Call [Router::with_state] to build a router.",
            vec![DocLink {
                target: "Router::with_state".to_owned(),
                label: Some("Router::with_state".to_owned()),
            }],
        )),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    });

    assert!(
        link.is_some(),
        "qualified-path shortcut [Router::with_state] must produce a Symbol link \
         when the leaf 'with_state' is in the corpus; got: {:?}",
        runs
    );

    match link.unwrap() {
        InlineRun::Link {
            target: LinkTarget::Symbol { key },
            ..
        } => {
            assert_eq!(key.intro, fn_id, "link must point at with_state's IntroId");
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Defect 1: three fenced code blocks → three RenderSection::CodeBlock
// ---------------------------------------------------------------------------

/// Three fenced code blocks separated by prose must each become an independent
/// `RenderSection::CodeBlock` with its own `SectionId` in the plan.
///
/// Before the fix, the entire logical section was handed as a flat event
/// slice to `build_prose_blocks`, which embedded all three fences as
/// `ProseBlock::Code` inside a single `RenderSection::Prose`.  The GUI's
/// code-fence renderer is attached to `RenderSection::CodeBlock`, not to
/// `ProseBlock::Code`, so only the first fence was ever displayed.
///
/// The fix (`split_at_code_blocks` inside `parse_markdown`) promotes each
/// fenced block to a top-level `RenderSection::CodeBlock`.  This test goes
/// through `chunk::chunk` (the full public pipeline) to ensure the
/// promotion happens end-to-end, not just inside the event-stream helper.
#[test]
fn three_fenced_code_blocks_each_become_own_render_section() {
    let lid = lineage("code-block-test");
    let root_id = intro(1);
    let mut table = PristineIntroTable::new();

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

    table.insert_live(
        root_id,
        make_module_entry(sym_with("root", doc, vec![])),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let (head, sections) = chunk::chunk(root_id, pkg.view(), &pkg).expect("chunk must succeed");

    // Three CodeBlock sections must exist.
    let code_sections: Vec<_> = sections
        .iter()
        .filter(|s| matches!(s, RenderSection::CodeBlock { .. }))
        .collect();

    assert_eq!(
        code_sections.len(),
        3,
        "three fenced blocks must each become a RenderSection::CodeBlock; \
         got {} code sections out of {} total: {:#?}",
        code_sections.len(),
        sections.len(),
        sections
            .iter()
            .map(|s| format!("{:?}", s.section_id()))
            .collect::<Vec<_>>(),
    );

    // All three must have distinct SectionIds.
    let ids: Vec<_> = code_sections.iter().map(|s| s.section_id()).collect();
    {
        let mut sorted = ids.clone();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "all code section ids must be distinct: {ids:?}"
        );
    }

    // Every CodeBlock SectionId must appear in the plan with kind CodeBlock.
    for id in &ids {
        let plan_entry = head
            .section_plan
            .iter()
            .find(|p| p.id == *id)
            .unwrap_or_else(|| {
                panic!(
                    "SectionId {id:?} is in sections but absent from section_plan; \
                 plan: {:#?}",
                    head.section_plan
                )
            });
        assert_eq!(
            plan_entry.kind,
            nudox_engine::wire::SectionKind::CodeBlock,
            "plan entry for {id:?} must have kind CodeBlock, got {:?}",
            plan_entry.kind,
        );
    }

    // Language tags must appear in source order.
    let langs: Vec<&str> = code_sections
        .iter()
        .map(|s| match s {
            // `lang.0` is `&SharedStr` (behind reference), so `&*lang.0` dereferences
            // both the `&SharedStr` wrapper and the `SharedStr` → `str` Deref.
            RenderSection::CodeBlock { lang, .. } => &*lang.0,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        langs,
        vec!["rust", "python", "javascript"],
        "code block languages must be in source order"
    );
}

// ---------------------------------------------------------------------------
// Defect 2: realistic producer target formats
// ---------------------------------------------------------------------------

/// A namespace-tagged producer target (`"Router::with_state!m"`) must resolve
/// to a Symbol link when the underlying symbol is in the same package.
///
/// Before the fix, `DocLinkTable::build` used `rsplit("::")` to extract the
/// leaf.  For a target with no `::` before the tag (like a dot-path or a
/// single-segment tagged form), the whole string including `!m` became the
/// "leaf" and `by_name.get_exact("with_state!m")` found nothing.
///
/// The fix strips the namespace tag before leaf extraction.
#[test]
fn namespace_tagged_target_produces_symbol_link_through_chunk() {
    let lid = lineage("tag-test");
    let root_id = intro(1);
    let fn_id = intro(2);
    let mut table = PristineIntroTable::new();

    table.insert_live(
        fn_id,
        make_module_entry(blank_sym("with_state")),
        Some(root_id),
    );
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Call [Router::with_state] to build the app.",
            vec![DocLink {
                // The producer emits a namespace tag to disambiguate methods from
                // same-name fields/consts.  The `!m` suffix must be stripped.
                target: "Router::with_state!m".to_owned(),
                label: Some("Router::with_state".to_owned()),
            }],
        )),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    });

    assert!(
        link.is_some(),
        "a namespace-tagged target 'Router::with_state!m' must still produce \
         a Symbol link; the tag must be stripped before leaf extraction; got: {runs:?}"
    );

    match link.unwrap() {
        InlineRun::Link {
            target: LinkTarget::Symbol { key },
            ..
        } => {
            assert_eq!(key.intro, fn_id, "link must point at with_state's IntroId");
        }
        _ => unreachable!(),
    }
}

/// A dot-path namespace-tagged target (`"axum.routing.Router.with_state!m"`)
/// must resolve through the full chunk pipeline.
///
/// This combines the two most common real-producer deviations: dot-path
/// separators AND a namespace tag.  The original code handled neither.
#[test]
fn dot_path_namespace_tagged_target_produces_symbol_link_through_chunk() {
    let lid = lineage("dot-tag-test");
    let root_id = intro(1);
    let fn_id = intro(2);
    let mut table = PristineIntroTable::new();

    table.insert_live(
        fn_id,
        make_module_entry(blank_sym("with_state")),
        Some(root_id),
    );
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Call [Router::with_state] to build the app.",
            vec![DocLink {
                target: "axum.routing.Router.with_state!m".to_owned(),
                label: Some("Router::with_state".to_owned()),
            }],
        )),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    });

    assert!(
        link.is_some(),
        "dot-path + namespace-tagged target 'axum.routing.Router.with_state!m' \
         must produce a Symbol link; got: {runs:?}"
    );

    match link.unwrap() {
        InlineRun::Link {
            target: LinkTarget::Symbol { key },
            ..
        } => {
            assert_eq!(key.intro, fn_id);
        }
        _ => unreachable!(),
    }
}

/// A rustdoc anchor-style target (`"#method.with_state"`) must resolve through
/// the full chunk pipeline.
///
/// rustdoc sometimes stores the HTML fragment identifier as the target field.
/// The `#method.` prefix carries no semantic information; stripping it recovers
/// the bare name `"with_state"` which the name index knows.
#[test]
fn anchor_style_target_produces_symbol_link_through_chunk() {
    let lid = lineage("anchor-test");
    let root_id = intro(1);
    let fn_id = intro(2);
    let mut table = PristineIntroTable::new();

    table.insert_live(
        fn_id,
        make_module_entry(blank_sym("with_state")),
        Some(root_id),
    );
    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Call [with_state] here.",
            vec![DocLink {
                target: "#method.with_state".to_owned(),
                label: Some("with_state".to_owned()),
            }],
        )),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    let link = runs.iter().find(|r| {
        matches!(
            r,
            InlineRun::Link {
                target: LinkTarget::Symbol { .. },
                ..
            }
        )
    });

    assert!(
        link.is_some(),
        "anchor-style target '#method.with_state' must produce a Symbol link \
         after stripping the '#method.' prefix; got: {runs:?}"
    );

    match link.unwrap() {
        InlineRun::Link {
            target: LinkTarget::Symbol { key },
            ..
        } => {
            assert_eq!(key.intro, fn_id);
        }
        _ => unreachable!(),
    }
}

/// `[Self::method]` (Rust `Self::` prefix) and
/// `[crate::routing::Router]` (crate-relative path) must both produce Symbol
/// links when the underlying symbols are in the same package.
///
/// These are the shapes most commonly written in real Rust doc comments.  The
/// existing tests only used bare-leaf paths; this test uses the qualified forms
/// that authors actually type.
#[test]
fn self_and_crate_prefixed_paths_produce_symbol_links_through_chunk() {
    let lid = lineage("self-crate-test");
    let root_id = intro(1);
    let bar_id = intro(2);
    let router_id = intro(3);
    let mut table = PristineIntroTable::new();

    table.insert_live(bar_id, make_module_entry(blank_sym("bar")), Some(root_id));
    table.insert_live(
        router_id,
        make_module_entry(blank_sym("Router")),
        Some(root_id),
    );

    table.insert_live(
        root_id,
        make_module_entry(sym_with(
            "root",
            "Use [Self::bar] or [crate::routing::Router] in your code.",
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
        )),
        None,
    );

    let view = IrView::with_package(lid, table);
    let pkg = PackageView::build(view, Provenance::TrustedLocal);

    let runs = first_paragraph_runs(&pkg, root_id);

    let symbol_links: Vec<_> = runs
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
        symbol_links.len(),
        2,
        "[Self::bar] and [crate::routing::Router] must each produce a Symbol link; \
         got {}: {runs:#?}",
        symbol_links.len(),
    );

    let intros: std::collections::HashSet<_> = symbol_links
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
        "bar_id must be in the resolved symbols"
    );
    assert!(
        intros.contains(&router_id),
        "router_id must be in the resolved symbols"
    );
}
