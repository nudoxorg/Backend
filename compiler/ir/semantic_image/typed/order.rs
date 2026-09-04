//! Cold exact collision comparison and canonical typed-pool ordering.

use alloc::vec::Vec;
use core::cmp::Ordering;

use super::*;

impl<'image> TypedDependencyPlan<'image> {
    pub(crate) fn order_canonical_nodes(&mut self) -> Result<(), TypedPlanError> {
        let mut ordered = Vec::with_capacity(self.scratch.nodes.len());
        for node in self.scratch.nodes.iter().copied() {
            ordered.push((node, self.scratch.fingerprints[self.slot(node)?]));
        }
        ordered.sort_unstable_by(|(left_node, left_fingerprint), (right_node, right_fingerprint)| {
            left_node
                .domain()
                .cmp(&right_node.domain())
                .then_with(|| left_fingerprint.cmp(right_fingerprint))
        });
        self.scratch
            .canonical_nodes
            .extend(ordered.into_iter().map(|(node, _)| node));
        let mut start = 0_usize;
        while start < self.scratch.canonical_nodes.len() {
            let node = self.scratch.canonical_nodes[start];
            let fingerprint = self.scratch.fingerprints[self.slot(node)?];
            let mut end = start.checked_add(1).ok_or(TypedPlanFault::GeometryOverflow {
                nodes: self.scratch.canonical_nodes.len(),
            })?;
            while end < self.scratch.canonical_nodes.len() {
                let other = self.scratch.canonical_nodes[end];
                if other.domain() != node.domain()
                    || self.scratch.fingerprints[self.slot(other)?] != fingerprint
                {
                    break;
                }
                end += 1;
            }
            self.order_equal_fingerprint_group(start, end)?;
            start = end;
        }

        let mut next = [0_u32; 8];
        for node in self.scratch.canonical_nodes.iter().copied() {
            let slot = self.slot(node)?;
            let domain = node.domain().index();
            self.scratch.canonical_slots[slot] = next[domain];
            next[domain] = next[domain].checked_add(1).ok_or(
                TypedPlanFault::GeometrySumOverflow {
                    accumulated: next[domain],
                    additional: 1,
                },
            )?;
        }
        Ok(())
    }

    /// `sort_unstable_by` cannot surface a typed comparator error. Equal
    /// fingerprint runs are therefore ordered by this small fallible
    /// insertion pass. Ordinary planning only pays the O(nodes log nodes)
    /// digest sort; this exact graph walk is collision-only.
    fn order_equal_fingerprint_group(
        &mut self,
        start: usize,
        end: usize,
    ) -> Result<(), TypedPlanError> {
        let first_unsorted = start.checked_add(1).ok_or(TypedPlanFault::GeometryOverflow {
            nodes: self.scratch.canonical_nodes.len(),
        })?;
        for cursor in first_unsorted..end {
            let value = self.scratch.canonical_nodes[cursor];
            let mut insertion = cursor;
            while insertion > start {
                let previous = self.scratch.canonical_nodes[insertion - 1];
                match self.compare_structure(previous, value)? {
                    Ordering::Less => break,
                    Ordering::Equal => {
                        return Err(TypedPlanFault::DuplicateCanonicalKey {
                            first: previous,
                            second: value,
                        }
                        .into());
                    }
                    Ordering::Greater => {
                        self.scratch.canonical_nodes[insertion] = previous;
                        insertion -= 1;
                    }
                }
            }
            self.scratch.canonical_nodes[insertion] = value;
        }
        Ok(())
    }

    fn compare_structure(
        &self,
        left: TypedPlanNode,
        right: TypedPlanNode,
    ) -> Result<Ordering, TypedPlanError> {
        let mut stack = Vec::new();
        stack.push((left, right));
        while let Some((left, right)) = stack.pop() {
            let ordering = left.domain().cmp(&right.domain());
            if ordering != Ordering::Equal {
                return Ok(ordering);
            }
            let (left_start, left_end) = self.edge_range(left)?;
            let (right_start, right_end) = self.edge_range(right)?;
            let left_edges = &self.scratch.edges[left_start..left_end];
            let right_edges = &self.scratch.edges[right_start..right_end];
            let ordering = left_edges.len().cmp(&right_edges.len());
            if ordering != Ordering::Equal {
                return Ok(ordering);
            }
            for (left_edge, right_edge) in left_edges.iter().zip(right_edges).rev() {
                let ordering = compare_role(left_edge.role, right_edge.role);
                if ordering != Ordering::Equal {
                    return Ok(ordering);
                }
                match (left_edge.target, right_edge.target) {
                    (TypedPlanTarget::Node(left), TypedPlanTarget::Node(right)) => {
                        stack.push((left, right));
                    }
                    (left, right) => {
                        let ordering = self.compare_terminal(left, right)?;
                        if ordering != Ordering::Equal {
                            return Ok(ordering);
                        }
                    }
                }
            }
        }
        Ok(Ordering::Equal)
    }

    fn compare_terminal(
        &self,
        left: TypedPlanTarget,
        right: TypedPlanTarget,
    ) -> Result<Ordering, TypedPlanError> {
        let tag = target_tag(left).cmp(&target_tag(right));
        if tag != Ordering::Equal {
            return Ok(tag);
        }
        match (left, right) {
            (TypedPlanTarget::Atom(left), TypedPlanTarget::Atom(right)) => {
                Ok(self.canonical.atom_bytes(left)?.cmp(self.canonical.atom_bytes(right)?))
            }
            (TypedPlanTarget::Entity(left), TypedPlanTarget::Entity(right)) => Ok(
                self.canonical
                    .entity_identity(left)?
                    .cmp(&self.canonical.entity_identity(right)?),
            ),
            (TypedPlanTarget::External(left), TypedPlanTarget::External(right)) => Ok(
                self.canonical.compare_external(left, right)?,
            ),
            (TypedPlanTarget::Scalar(left), TypedPlanTarget::Scalar(right)) => Ok(left.cmp(&right)),
            (TypedPlanTarget::Node(_), TypedPlanTarget::Node(_)) => Ok(Ordering::Equal),
            _ => Ok(Ordering::Equal),
        }
    }
}
