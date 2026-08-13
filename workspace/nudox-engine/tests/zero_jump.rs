//! Adversarial tests for the chunker's zero-jump guarantee (GUI-PLAN §9.4).
//!
//! The in-module tests check that `section_plan()` and `sections()` agree on
//! *ids and order*. That is necessary but nowhere near sufficient: the whole
//! point of the plan is that the GUI can pre-lay a skeleton of the right
//! **size** before any content arrives, so that arrival swaps content in
//! without moving anything below it. A plan with perfect ids and wrong sizes
//! still makes the page jump under the reader's eyes — the exact failure §9.4
//! exists to forbid.
//!
//! So these tests measure the plan against what the sections actually render,
//! and they do it for every entry in the rich fixture rather than a chosen one.

use std::collections::HashMap;

use nudox_engine::chunk;
use nudox_engine::wire::{ProseBlock, RenderSection, SectionId, SizeHint};
use nudox_store::package::{PackageView, Provenance};
use nudox_store::source::fixtures::build_rich_view;

/// How many display lines a section will actually occupy.
///
/// This mirrors what the GUI's `list()` will measure, per §9.4's size classes:
/// prose counts its blocks' lines, code counts its `line_count`, and member and
/// field tables count rows.
fn actual_size(section: &RenderSection) -> SizeHint {
    fn prose_lines(blocks: &[ProseBlock]) -> u32 {
        blocks
            .iter()
            .map(|b| match b {
                ProseBlock::Paragraph { .. } => 1,
                ProseBlock::Heading { .. } => 1,
                ProseBlock::List { items, .. } => items.len().max(1) as u32,
                ProseBlock::Rule => 1,
                ProseBlock::Code { line_count, .. } => *line_count,
                _ => 1,
            })
            .sum::<u32>()
            .max(1)
    }

    match section {
        RenderSection::Prose { blocks, .. } | RenderSection::Examples { blocks, .. } => {
            SizeHint::Lines(prose_lines(blocks))
        }
        RenderSection::Callout { blocks, .. } => SizeHint::Lines(prose_lines(blocks)),
        RenderSection::CodeBlock { line_count, .. } => SizeHint::Lines(*line_count),
        RenderSection::Members { entries, .. } => SizeHint::Rows(entries.len() as u32),
        RenderSection::Fields { entries, .. } => SizeHint::Rows(entries.len() as u32),
        RenderSection::Unknown { .. } => SizeHint::Unknown,
        _ => SizeHint::Unknown,
    }
}

/// Both hints must be the same *class* — a skeleton laid out as rows cannot be
/// replaced by content measured in lines without the geometry changing.
fn same_class(a: &SizeHint, b: &SizeHint) -> bool {
    matches!(
        (a, b),
        (SizeHint::Lines(_), SizeHint::Lines(_))
            | (SizeHint::Rows(_), SizeHint::Rows(_))
            | (SizeHint::Unknown, SizeHint::Unknown)
    )
}

fn magnitude(h: &SizeHint) -> Option<u32> {
    match h {
        SizeHint::Lines(n) | SizeHint::Rows(n) => Some(*n),
        SizeHint::Unknown => None,
        _ => None,
    }
}

/// For every entry in the fixture, every planned section's `SizeHint` must
/// match the size the section actually renders at.
///
/// `Rows` must be **exact** — the chunker counts children during the same walk
/// that emits them, so there is no excuse for an estimate. `Lines` is allowed a
/// small tolerance because prose line counts depend on wrapping, but a hint
/// that is out by more than a couple of lines will visibly shift the content
/// below it when the real section lands.
#[test]
fn plan_size_hints_match_rendered_sections() {
    let view = build_rich_view();
    let package = PackageView::build(view, Provenance::TrustedLocal);

    let mut checked_sections = 0usize;
    let mut checked_entries = 0usize;

    for (intro, _entry) in package.view().entries() {
        let Some((head, sections)) = chunk::chunk(intro, package.view(), &package) else {
            continue;
        };
        checked_entries += 1;

        let actual: HashMap<SectionId, SizeHint> = sections
            .iter()
            .map(|s| (s.section_id(), actual_size(s)))
            .collect();

        for planned in &head.section_plan {
            let Some(real) = actual.get(&planned.id) else {
                panic!(
                    "planned section {:?} for {} was never emitted — \
                     the skeleton would be left showing an empty slot forever",
                    planned.id, head.key
                );
            };

            assert!(
                same_class(&planned.size_hint, real),
                "section {:?} of {} planned as {:?} but rendered as {:?}: \
                 a skeleton of one geometry class cannot be replaced by content \
                 of another without the page shifting",
                planned.id,
                head.key,
                planned.size_hint,
                real
            );

            if let (Some(hinted), Some(actual_n)) = (magnitude(&planned.size_hint), magnitude(real))
            {
                let tolerance = match real {
                    // Rows are counted, not estimated — demand exactness.
                    SizeHint::Rows(_) => 0,
                    // Prose wraps; allow a couple of lines of slack.
                    _ => 2,
                };
                assert!(
                    hinted.abs_diff(actual_n) <= tolerance,
                    "section {:?} of {} hinted {hinted} but rendered {actual_n} \
                     (tolerance {tolerance}) — content below it would jump by \
                     {} units on arrival",
                    planned.id,
                    head.key,
                    hinted.abs_diff(actual_n)
                );
            }
            checked_sections += 1;
        }
    }

    assert!(
        checked_entries > 20 && checked_sections > 20,
        "only checked {checked_entries} entries / {checked_sections} sections — \
         the fixture is too thin for this guarantee to mean anything"
    );
}

/// Every section that is emitted must have been planned.
///
/// The converse of the test above. An unplanned section has no skeleton slot,
/// so it appears by *inserting* itself into the layout — pushing everything
/// below it down mid-read, which is precisely the jump §9.4 forbids.
#[test]
fn no_section_arrives_unplanned() {
    let view = build_rich_view();
    let package = PackageView::build(view, Provenance::TrustedLocal);

    for (intro, _entry) in package.view().entries() {
        let Some((head, sections)) = chunk::chunk(intro, package.view(), &package) else {
            continue;
        };
        let planned: Vec<SectionId> = head.section_plan.iter().map(|p| p.id).collect();
        for section in &sections {
            assert!(
                planned.contains(&section.section_id()),
                "{} emitted section {:?} that appears in no plan",
                head.key,
                section.section_id()
            );
        }
    }
}

/// The same fixture pass emits a machine-readable cost line so the test suite
/// doubles as a regression benchmark without changing the production path.
#[test]
fn measured_fixture_chunking_reports_cost() {
    let directory =
        std::env::temp_dir().join(format!("nudox-engine-zero-jump-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("create measurement directory");
    let (checked, cost) =
        nudox_test_support::measured("engine.zero_jump.fixture_chunking", &directory, || {
            let view = build_rich_view();
            let package = PackageView::build(view, Provenance::TrustedLocal);
            package
                .view()
                .entries()
                .filter_map(|(intro, _)| chunk::chunk(intro, package.view(), &package))
                .count()
        });
    assert!(
        checked > 20,
        "fixture pass measured too few entries: {checked}"
    );
    assert!(cost.wall > std::time::Duration::ZERO);
    let _ = std::fs::remove_dir_all(directory);
}

/// Every `SigToken::Ty` that claims a target must actually resolve.
///
/// A `Ty` token with `target: Some(key)` renders as a clickable link. If the
/// key does not resolve, the user gets a link that opens nothing — worse than
/// plain text, because it advertises a destination that is not there.
#[test]
fn signature_type_targets_all_resolve() {
    use nudox_engine::wire::SigToken;

    let view = build_rich_view();
    let lineage = view.package().clone();
    let package = PackageView::build(view, Provenance::TrustedLocal);

    let mut linked = 0usize;
    for (intro, _entry) in package.view().entries() {
        let Some((head, _)) = chunk::chunk(intro, package.view(), &package) else {
            continue;
        };
        for token in &head.signature {
            if let SigToken::Ty {
                target: Some(key), ..
            } = token
            {
                // Same-package targets must be present in this view; foreign
                // ones are legitimately unresolvable until that package loads.
                if key.package == lineage {
                    assert!(
                        package.view().entry(key.intro).is_some(),
                        "{} has a signature link to {key}, which does not exist \
                         in its own package",
                        head.key
                    );
                    linked += 1;
                }
            }
        }
    }

    assert!(
        linked > 0,
        "no same-package signature links were produced — either the fixture has \
         no nominal types or target resolution silently returns None, and this \
         test proves nothing either way"
    );
}

/// Chunking must not depend on how many other packages happen to be loaded.
///
/// The chunker takes a `PackageView`, so this should hold structurally; the
/// test exists because the moment it stops holding, symbol pages start
/// rendering differently depending on load order, which is close to
/// undebuggable from a screenshot.
#[test]
fn chunk_output_is_independent_of_corpus_state() {
    let package_a = PackageView::build(build_rich_view(), Provenance::TrustedLocal);
    let package_b = PackageView::build(build_rich_view(), Provenance::TrustedLocal);

    for (intro, _) in package_a.view().entries() {
        let first = chunk::chunk(intro, package_a.view(), &package_a);
        let second = chunk::chunk(intro, package_b.view(), &package_b);
        match (first, second) {
            (Some((h1, s1)), Some((h2, s2))) => {
                assert_eq!(
                    h1.signature, h2.signature,
                    "signature diverged for {}",
                    h1.key
                );
                assert_eq!(
                    h1.section_plan, h2.section_plan,
                    "section plan diverged for {}",
                    h1.key
                );
                assert_eq!(s1.len(), s2.len(), "section count diverged for {}", h1.key);
            }
            (None, None) => {}
            _ => panic!("chunk() disagreed about whether {intro:?} is chunkable"),
        }
    }
}
