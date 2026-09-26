//! Kind, name, and adjacency indices for one admitted semantic image.
//!
//! The column structs stay in the parent. This module owns the sort that
//! turns packed links and entity rows into canonical index order.

use super::*;

impl IrIndices {
    /// Builds kind, name, and adjacency indices for one admitted image.
    pub(in crate::ir::semantic) fn build(
        items: &ItemColumns,
        versions: &[EntityVersion],
        atoms: &AtomInterner,
        links: &PackedLinks,
        link_occurrences: &PackedLinkOccurrences,
        externals: &[ExternalTarget],
    ) -> Result<(Self, [u32; 16]), BuildError> {
        let entity_count = items.len();
        let link_count = links.len();
        let occurrence_count = link_occurrences.len();
        let local_links = (0..link_count)
            .filter(|raw| {
                links
                    .get(LinkId::new(*raw as u32))
                    .is_some_and(|link| matches!(link.target, LinkTarget::Local(_)))
            })
            .count();
        let mut plan = SlabPlan::default();
        let instances = plan.column::<EntityId>(entity_count);
        let kind = plan.column::<EntityId>(entity_count);
        let name = plan.column::<EntityId>(entity_count);
        let canonical_links = plan.column::<LinkId>(link_count);
        let outgoing = plan.column::<LinkId>(link_count);
        let outgoing_offsets = plan.column::<u32>(entity_count.saturating_add(1));
        let incoming = plan.column::<LinkId>(local_links);
        let incoming_offsets = plan.column::<u32>(entity_count.saturating_add(1));
        let occurrence_outgoing = plan.column::<LinkOccurrenceId>(occurrence_count);
        let occurrence_outgoing_offsets = plan.column::<u32>(entity_count.saturating_add(1));
        let slab = plan.allocate();
        let mut indices = Self {
            instances: slab.bind(instances),
            kind: slab.bind(kind),
            name: slab.bind(name),
            canonical_links: slab.bind(canonical_links),
            outgoing: slab.bind(outgoing),
            outgoing_offsets: slab.bind(outgoing_offsets),
            incoming: slab.bind(incoming),
            incoming_offsets: slab.bind(incoming_offsets),
            occurrence_outgoing: slab.bind(occurrence_outgoing),
            occurrence_outgoing_offsets: slab.bind(occurrence_outgoing_offsets),
            _slab: slab,
        };
        for raw in 0..entity_count {
            let raw = u32::try_from(raw).map_err(|_| CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: raw,
            })?;
            let id = EntityId::new(raw);
            indices.instances.push(id);
            indices.kind.push(id);
            indices.name.push(id);
        }
        for raw in 0..link_count {
            let raw = u32::try_from(raw).map_err(|_| CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: raw,
            })?;
            let id = LinkId::new(raw);
            indices.canonical_links.push(id);
            indices.outgoing.push(id);
            if links
                .get(id)
                .is_some_and(|link| matches!(link.target, LinkTarget::Local(_)))
            {
                indices.incoming.push(id);
            }
        }
        for raw in 0..occurrence_count {
            let raw = u32::try_from(raw).map_err(|_| CapacityError {
                space: crate::ir::CapacitySpace::Value,
                actual: raw,
            })?;
            indices.occurrence_outgoing.push(LinkOccurrenceId::new(raw));
        }
        for _ in 0..=entity_count {
            indices.outgoing_offsets.push(0);
            indices.incoming_offsets.push(0);
            indices.occurrence_outgoing_offsets.push(0);
        }

        indices
            .instances
            .as_mut_slice()
            .sort_unstable_by_key(|id| versions[id.index()].identity());
        for pair in indices.instances.windows(2) {
            if versions[pair[0].index()].identity() == versions[pair[1].index()].identity() {
                return Err(BuildError::DuplicateDeclarationIdentity {
                    identity: versions[pair[0].index()].identity(),
                });
            }
        }

        indices
            .kind
            .as_mut_slice()
            .sort_unstable_by_key(|id| (items.kinds[id.index()], versions[id.index()].identity()));
        let mut kind_offsets = [0_u32; 16];
        for kind in items.kinds.iter() {
            kind_offsets[*kind as usize + 1] += 1;
        }
        prefix_sum(&mut kind_offsets);

        indices.name.as_mut_slice().sort_unstable_by(|left, right| {
            let left_name = atoms.get(items.names[left.index()]).unwrap_or(&[]);
            let right_name = atoms.get(items.names[right.index()]).unwrap_or(&[]);
            (left_name, versions[left.index()].identity())
                .cmp(&(right_name, versions[right.index()].identity()))
        });
        for id in indices.canonical_links.iter() {
            let link = links
                .get(*id)
                .expect("canonical IDs originate from packed links");
            if let LinkTarget::External(external) = link.target {
                externals
                    .get(external.index())
                    .ok_or(BuildError::Dangling {
                        space: SemanticSpace::External,
                        raw: external.raw,
                    })?;
            }
        }
        indices
            .canonical_links
            .as_mut_slice()
            .sort_unstable_by_key(|id| {
                let link = links
                    .get(*id)
                    .expect("canonical IDs originate from packed links");
                let from = versions[link.from.index()].identity();
                let target = match link.target {
                    LinkTarget::Local(entity) => {
                        DeclarationLinkTarget::Local(versions[entity.index()].identity())
                    }
                    LinkTarget::External(external) => declaration_link_target(
                        *externals
                            .get(external.index())
                            .expect("external coordinate was validated before canonical sorting"),
                    ),
                };
                (from, target, link.kind)
            });
        sort_adjacency(
            links,
            indices.outgoing.as_mut_slice(),
            indices.outgoing_offsets.as_mut_slice(),
            false,
        );
        sort_adjacency(
            links,
            indices.incoming.as_mut_slice(),
            indices.incoming_offsets.as_mut_slice(),
            true,
        );
        sort_occurrence_adjacency(
            links,
            link_occurrences,
            indices.occurrence_outgoing.as_mut_slice(),
            indices.occurrence_outgoing_offsets.as_mut_slice(),
        );
        Ok((indices, kind_offsets))
    }
}
