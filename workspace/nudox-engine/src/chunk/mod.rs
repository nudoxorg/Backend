//! The chunker — the single seam between `nudox_ir::Entry` and presentation.
//!
//! # Why this exists as the crown jewel (LR-3)
//!
//! Every byte that `lindsey` renders about a symbol must originate here.
//! Rendering a signature, a doc section, or a kind label anywhere else is a
//! review-blocking violation of LR-3.  This is not a soft convention: the
//! lint script (`scripts/lint-gui-no-block.sh`) greps for `format!` inside
//! `lindsey`'s render bodies and fails CI if it finds any.
//!
//! The reason for such a strict single-seam rule is the zero-jump guarantee
//! (§9.4): `SymbolHead::section_plan` and the actual `RenderSection` stream
//! must agree on section count, section ids, and section order *before* any
//! content arrives.  If two different code paths produced the plan and the
//! sections, they would inevitably diverge in edge cases.  The one-walk
//! guarantee (see below) makes that class of bug impossible by construction.
//!
//! # One-walk guarantee
//!
//! `section_plan` and `sections` are **not** independent computations.  Both
//! call [`walk::walk_doc`], which parses the markdown and enumerates the
//! children in a single pass and returns a [`walk::WalkOutput`] containing
//! both the plan data and the rendered sections.  The caller extracts
//! whichever field it needs.  This means:
//!
//! * The `SectionId` sequence is assigned once, inside `walk_doc`.
//! * The `SizeHint`s are derived from the same markdown that produced the
//!   `ProseBlock`s — they can never disagree.
//! * Adding a new section kind requires changing one place, not two.
//!
//! # LR-4: one signature, many surfaces
//!
//! `signature::tokens` is the single source for every rendered signature.
//! It is called from `head::head` (symbol page header), from `sections`
//! (member rows inside `RenderSection::Members`), and from the search layer
//! (hit row previews).  There is no second code path.

pub mod head;
pub mod plan;
/// Deriving the link-repair count from the rendered runs themselves.
pub mod repair_audit;
pub mod sections;
pub mod signature;

// Private shared walk — not pub; callers go through `plan` or `sections`.
pub(crate) mod walk;

use nudox_ir::{change::IntroId, view::IrView};
use crate::store::package::PackageView;

use crate::wire::{RenderSection, SymbolHead};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Convert one entry into its full head + rendered sections.
///
/// Returns `None` when `intro` is not live in `view`.
///
/// This is the **only** public entry point for IR→presentation for the full
/// page.  Sub-functions (`head`, `sections`, `signature::tokens`) are exposed
/// individually so the search layer can call `signature::tokens` without
/// re-building the full page, but they must not be called by anything outside
/// this crate except through well-defined public APIs.
///
/// # One-walk guarantee
///
/// `SymbolHead::section_plan` and `sections` are produced by the same
/// underlying walk ([`walk::walk_doc`]) so that the skeleton geometry is
/// *derived*, not estimated.  This is what makes the §9.4 zero-jump
/// guarantee provable rather than merely hoped-for.
pub fn chunk(
    intro: IntroId,
    view: &IrView,
    package: &PackageView,
) -> Option<(SymbolHead, Vec<RenderSection>)> {
    let entry = view.entry(intro)?;

    // Produce the plan and sections in one physical walk, then inject the plan
    // into the head. This is both the zero-jump invariant and the cost bound;
    // the previous wrappers each called `walk_doc` and doubled the work.
    let walked = walk::walk_doc(intro, entry, view, package);
    let h = head::head_with_plan(intro, entry, view, package, walked.plan);
    Some((h, walked.sections))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::store::{
        package::{PackageView, Provenance},
        source::fixtures::build_rich_view,
    };

    use crate::chunk::{plan::section_plan, sections::sections};

    /// §9.4 invariant: for every entry in the rich fixture, `section_plan`'s
    /// ids and order exactly match the ids and order of the sections `sections`
    /// actually produces.
    ///
    /// This test is the machine-checked form of "the page never jumps": if the
    /// plan and sections ever disagree, the skeleton geometry would be wrong
    /// and the GUI would shift content on arrival.
    #[test]
    fn section_plan_matches_sections_ids_and_order() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let plan = section_plan(intro, entry, pkg.view(), &pkg);
            let sects = sections(intro, entry, pkg.view(), &pkg);

            assert_eq!(
                plan.len(),
                sects.len(),
                "plan/sections count mismatch for '{}'",
                entry.sym().name
            );

            for (p, s) in plan.iter().zip(sects.iter()) {
                assert_eq!(
                    p.id,
                    s.section_id(),
                    "section id mismatch at position {:?} for '{}'",
                    p.id,
                    entry.sym().name,
                );
            }
        }
    }

    /// Determinism: calling `chunk` twice on the same entry must produce
    /// byte-identical output.
    #[test]
    fn chunk_is_deterministic() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        for (intro, _entry) in pkg.view().entries() {
            let (h1, s1) = super::chunk(intro, pkg.view(), &pkg).expect("chunk must succeed");
            let (h2, s2) = super::chunk(intro, pkg.view(), &pkg).expect("chunk must succeed");
            assert_eq!(
                format!("{h1:?}"),
                format!("{h2:?}"),
                "head is not deterministic for intro {:?}",
                intro
            );
            assert_eq!(
                format!("{s1:?}"),
                format!("{s2:?}"),
                "sections are not deterministic for intro {:?}",
                intro
            );
        }
    }

    /// DocEvent prefix invariant (§9.3 invariant 2): applying any prefix of a
    /// valid stream yields a valid page.  Modelled as a small state machine.
    #[test]
    fn doc_event_prefix_is_valid_page() {
        use crate::wire::{DocEvent, HighlightSpan};
        use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
        use std::sync::Arc;

        // Build a synthetic but realistic event stream.
        let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("test-pkg"));
        let intro = IntroId::from_raw([0x01u8; 32]);
        let _key = StableRef::new(lineage, intro);

        let view_r = build_rich_view();
        let pkg = PackageView::build(view_r, Provenance::TrustedLocal);

        // Grab a real SymbolHead from the rich corpus.
        let (head, sections) = super::chunk(intro, pkg.view(), &pkg)
            // intro(1) is the root module which exists
            .or_else(|| {
                let first_intro = pkg.view().entries().next().map(|(i, _)| i)?;
                super::chunk(first_intro, pkg.view(), &pkg)
            })
            .expect("at least one entry must chunk successfully");

        let mut stream: Vec<DocEvent> = Vec::new();
        stream.push(DocEvent::Head(Box::new(head)));
        for sect in sections {
            let sid = sect.section_id();
            stream.push(DocEvent::Section(sect));
            // Add a highlight event after each section.
            stream.push(DocEvent::Highlight {
                section: sid,
                spans: Arc::from([] as [HighlightSpan; 0]),
            });
        }
        stream.push(DocEvent::Done);

        // State machine: validate every prefix.
        for prefix_len in 0..=stream.len() {
            let prefix = &stream[..prefix_len];
            validate_doc_event_prefix(prefix);
        }
    }

    /// Validate that a prefix of a `DocEvent` stream is structurally sound.
    fn validate_doc_event_prefix(events: &[crate::wire::DocEvent]) {
        use crate::wire::{DocEvent, SectionId};
        use std::collections::HashSet;

        let mut have_head = false;
        let mut terminal = false;
        let mut sent_sections: HashSet<SectionId> = HashSet::new();

        for ev in events {
            assert!(!terminal, "events after terminal are forbidden");
            match ev {
                DocEvent::Head(_) => {
                    assert!(!have_head, "duplicate Head event");
                    have_head = true;
                }
                DocEvent::Section(s) => {
                    assert!(have_head, "Section before Head");
                    sent_sections.insert(s.section_id());
                }
                DocEvent::Highlight { section, .. } => {
                    assert!(have_head, "Highlight before Head");
                    assert!(
                        sent_sections.contains(section),
                        "Highlight references unsent section {section:?}"
                    );
                }
                DocEvent::Refs { .. } | DocEvent::Impls { .. } => {
                    // may interleave freely after Head
                    assert!(have_head, "Refs/Impls before Head");
                }
                DocEvent::Timeline(_) => {
                    // The timeline is a *tab*, not a section: it carries no
                    // `SectionId` and occupies no space in the document
                    // column, which is why `section_plan` never promises
                    // geometry for it.
                    //
                    // It still has a place in the order. It needs `Head` for
                    // the symbol's identity, and it must precede the first
                    // `Section` so the tab is populated by the time a reader
                    // could plausibly switch to it — a tab that fills in after
                    // the body has finished streaming reads as broken.
                    assert!(have_head, "Timeline before Head");
                    assert!(
                        sent_sections.is_empty(),
                        "Timeline must precede the first Section",
                    );
                }
                DocEvent::Done | DocEvent::Failed(_) => {
                    terminal = true;
                }
            }
        }
    }
}
