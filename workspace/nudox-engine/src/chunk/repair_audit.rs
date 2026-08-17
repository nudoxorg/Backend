//! Counting link repairs — derived from the rendered runs, never accumulated
//! alongside them.
//!
//! # Why a derivation and not a counter
//!
//! The obvious implementation is a `u32` incremented inside
//! `consume_bracket_run`. That is exactly the shape doctrine §8 warns about: a
//! tally maintained in parallel with the thing it counts *can* drift from it,
//! and when it does, the number is wrong in a way nothing detects. A tally
//! computed *from* the runs cannot — if the count says three, three repaired
//! runs exist in the sections you handed it.
//!
//! So there is no counter field anywhere in the engine. [`RepairTally`] walks
//! rendered sections and reads the [`LinkOrigin`] that every
//! [`InlineRun::Link`] is required to carry.

use crate::wire::{InlineRun, LinkOrigin, LinkRepairKind, ProseBlock, RenderSection};

/// A count of link repairs, by kind, derived from rendered sections.
///
/// Derived — never accumulated in parallel with the thing it counts. See the
/// module docs for why that distinction is the whole point of the type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepairTally {
    counts: [u32; LinkRepairKind::ALL.len()],
}

impl RepairTally {
    /// Walk every [`InlineRun::Link`] in `sections` and count the repaired
    /// ones.
    ///
    /// Descends `Prose`, `Examples` and `Callout` blocks — `Paragraph`,
    /// `Heading`, and `List` items are the three block kinds that carry inline
    /// runs; `Code` and `Rule` carry none. `Members`, `Fields` and `Unknown`
    /// sections carry no prose at all.
    pub fn of_sections(sections: &[RenderSection]) -> Self {
        let mut tally = Self::default();
        for section in sections {
            let blocks = match section {
                RenderSection::Prose { blocks, .. }
                | RenderSection::Examples { blocks, .. }
                | RenderSection::Callout { blocks, .. } => blocks.as_slice(),
                // No inline runs live in these; nothing to count.
                //
                // Deliberately no wildcard arm: `RenderSection` is
                // `#[non_exhaustive]` only for *other* crates, so in here a
                // new section kind breaks this match — which is the point. A
                // future run-bearing section that this function silently
                // skipped would undercount repairs, and an undercounted
                // repair is a silent repair again.
                RenderSection::CodeBlock { .. }
                | RenderSection::Members { .. }
                | RenderSection::Fields { .. }
                | RenderSection::Unknown { .. } => &[],
            };
            for block in blocks {
                tally.add_block(block);
            }
        }
        tally
    }

    fn add_block(&mut self, block: &ProseBlock) {
        match block {
            ProseBlock::Paragraph { runs } | ProseBlock::Heading { runs, .. } => {
                self.add_runs(runs);
            }
            ProseBlock::List { items, .. } => {
                for item in items {
                    self.add_runs(item);
                }
            }
            // No wildcard, same reasoning as `of_sections`: a new
            // run-bearing block kind must break this match rather than be
            // quietly skipped.
            ProseBlock::Code { .. } | ProseBlock::Rule => {}
        }
    }

    fn add_runs(&mut self, runs: &[InlineRun]) {
        for run in runs {
            if let InlineRun::Link {
                origin: LinkOrigin::Repaired(repair),
                ..
            } = run
            {
                self.counts[repair.kind.index()] += 1;
            }
        }
    }

    /// Fold another tally into this one, for aggregating across entries or
    /// packages.
    pub fn merge(&mut self, other: &Self) {
        for (slot, add) in self.counts.iter_mut().zip(other.counts.iter()) {
            *slot += *add;
        }
    }

    /// How many repairs of `kind` were seen.
    pub fn get(&self, kind: LinkRepairKind) -> u32 {
        self.counts[kind.index()]
    }

    /// Every repair, of every kind.
    pub fn total(&self) -> u32 {
        self.counts.iter().sum()
    }

    /// Each kind paired with its count, in `LinkRepairKind::ALL` order —
    /// including the zeroes, so a reader of the audit can see *which* repairs
    /// did not fire, not merely which did.
    pub fn iter(&self) -> impl Iterator<Item = (LinkRepairKind, u32)> + '_ {
        LinkRepairKind::ALL
            .iter()
            .map(move |&kind| (kind, self.counts[kind.index()]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{LinkRepair, LinkTarget, SectionId, SharedStr, SymbolKey};
    use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};

    fn key() -> SymbolKey {
        SymbolKey::new(
            PackageLineageId::new(EcosystemId::new("test"), PackageName::new("tally")),
            IntroId::from_raw([7; 32]),
        )
    }

    fn link(origin: LinkOrigin) -> InlineRun {
        InlineRun::Link {
            text: SharedStr::from("memrchr_iter"),
            target: LinkTarget::Symbol { key: key() },
            origin,
        }
    }

    fn repaired() -> LinkOrigin {
        LinkOrigin::Repaired(LinkRepair {
            kind: LinkRepairKind::TransposedOpenDelimiter,
            raw: SharedStr::from("`[memrchr_iter`]"),
            resolved: SharedStr::from("memrchr_iter"),
            note: SharedStr::from("Repaired link."),
        })
    }

    /// Authored links must not be counted. If they were, every page in the
    /// corpus would report a repair and the number would mean nothing.
    #[test]
    fn authored_links_are_not_counted_as_repairs() {
        let sections = vec![RenderSection::Prose {
            id: SectionId(0),
            blocks: vec![ProseBlock::Paragraph {
                runs: vec![link(LinkOrigin::Authored), link(LinkOrigin::Authored)],
            }],
        }];
        let tally = RepairTally::of_sections(&sections);
        assert_eq!(tally.total(), 0);
    }

    /// Repairs must be found in headings and list items too, not only in
    /// paragraphs — a repair hiding in a bullet is still a repair.
    #[test]
    fn repairs_are_counted_in_every_run_bearing_block() {
        let sections = vec![
            RenderSection::Prose {
                id: SectionId(0),
                blocks: vec![
                    ProseBlock::Paragraph {
                        runs: vec![link(repaired()), link(LinkOrigin::Authored)],
                    },
                    ProseBlock::Heading {
                        level: 2,
                        runs: vec![link(repaired())],
                    },
                    ProseBlock::List {
                        ordered: false,
                        items: vec![vec![link(repaired())], vec![link(LinkOrigin::Authored)]],
                    },
                    ProseBlock::Rule,
                ],
            },
            RenderSection::Examples {
                id: SectionId(1),
                blocks: vec![ProseBlock::Paragraph {
                    runs: vec![link(repaired())],
                }],
            },
        ];
        let tally = RepairTally::of_sections(&sections);
        assert_eq!(tally.total(), 4);
        assert_eq!(tally.get(LinkRepairKind::TransposedOpenDelimiter), 4);
    }

    /// `iter()` must yield every kind including the zeroes — an audit that
    /// only prints non-zero rows cannot distinguish "this repair never fired"
    /// from "this repair does not exist".
    #[test]
    fn iter_yields_every_kind_including_zeroes() {
        let tally = RepairTally::default();
        let rows: Vec<_> = tally.iter().collect();
        assert_eq!(rows.len(), LinkRepairKind::ALL.len());
        assert!(rows.iter().all(|&(_, n)| n == 0));
    }

    /// Merging is what makes a corpus-wide number possible; it must add, not
    /// replace.
    #[test]
    fn merge_sums_counts() {
        let sections = vec![RenderSection::Prose {
            id: SectionId(0),
            blocks: vec![ProseBlock::Paragraph {
                runs: vec![link(repaired())],
            }],
        }];
        let mut a = RepairTally::of_sections(&sections);
        let b = RepairTally::of_sections(&sections);
        a.merge(&b);
        assert_eq!(a.total(), 2);
    }
}
