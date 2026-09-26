//! Compressed sparse-row layout for semantic links.
//!
//! Canonical relations and authority-observed occurrences sort into forward
//! and reverse adjacency. Distinct observation sites stay distinct.

use super::super::columns::{PackedLinkOccurrences, PackedLinks};
use super::super::ids::{LinkId, LinkOccurrenceId};
use super::super::relations::{DeclarationLinkTarget, ExternalTarget, LinkTarget, SourceSpan};

pub(in crate::ir::semantic) fn declaration_link_target(
    target: ExternalTarget,
) -> DeclarationLinkTarget {
    match target {
        ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(target),
        ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
        ExternalTarget::FragmentEntity { target, .. } => {
            DeclarationLinkTarget::FragmentEntity(target)
        }
    }
}

pub(in crate::ir::semantic) fn prefix_sum(values: &mut [u32]) {
    for index in 1..values.len() {
        values[index] += values[index - 1];
    }
}

pub(in crate::ir::semantic) fn sort_adjacency(
    links: &PackedLinks,
    order: &mut [LinkId],
    offsets: &mut [u32],
    reverse: bool,
) {
    order.sort_unstable_by_key(|link_id| {
        let link = links
            .get(*link_id)
            .expect("adjacency IDs originate from packed links");
        if reverse {
            let target = match link.target {
                LinkTarget::Local(target) => target.raw,
                LinkTarget::External(_) => u32::MAX,
            };
            (target, 0, link.from.raw, link.kind, link_id.raw)
        } else {
            let (external, target) = match link.target {
                LinkTarget::Local(target) => (0, target.raw),
                LinkTarget::External(target) => (1, target.raw),
            };
            (link.from.raw, external, target, link.kind, link_id.raw)
        }
    });
    for link_id in order.iter().copied() {
        let link = links
            .get(link_id)
            .expect("adjacency IDs originate from packed links");
        let raw = if reverse {
            match link.target {
                LinkTarget::Local(target) => target.raw,
                LinkTarget::External(_) => continue,
            }
        } else {
            link.from.raw
        };
        offsets[raw as usize + 1] += 1;
    }
    prefix_sum(offsets);
}

/// Sorts every authority-observed occurrence by owner and canonical relation
/// without collapsing sites. Fully equal sites are intentionally an unordered
/// multiset: no emission ordinal enters a canonical storage key.
pub(in crate::ir::semantic) fn sort_occurrence_adjacency(
    links: &PackedLinks,
    occurrences: &PackedLinkOccurrences,
    order: &mut [LinkOccurrenceId],
    offsets: &mut [u32],
) {
    order.sort_unstable_by_key(|occurrence_id| {
        let occurrence = occurrences
            .get(*occurrence_id)
            .expect("occurrence IDs originate from packed occurrence rows");
        let relation = links
            .get(occurrence.link)
            .expect("occurrence relations were validated before indexing");
        let (external, target) = match relation.target {
            LinkTarget::Local(target) => (0, target.raw),
            LinkTarget::External(target) => (1, target.raw),
        };
        (
            relation.from.raw,
            external,
            target,
            relation.kind,
            occurrence.source.map_or(u32::MAX, |span| span.file().raw),
            occurrence.source.map_or(u32::MAX, SourceSpan::start),
            occurrence.source.map_or(u32::MAX, SourceSpan::end),
        )
    });
    for occurrence_id in order.iter().copied() {
        let occurrence = occurrences
            .get(occurrence_id)
            .expect("occurrence IDs originate from packed occurrence rows");
        let relation = links
            .get(occurrence.link)
            .expect("occurrence relations were validated before indexing");
        offsets[relation.from.index() + 1] += 1;
    }
    prefix_sum(offsets);
}
