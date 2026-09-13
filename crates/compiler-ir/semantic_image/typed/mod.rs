//! Canonical full-image typed-pool planning.
//!
//! The façade keeps the planner internal until all full-image directories can
//! encode and validate its remapped coordinates.  Model, graph traversal, and
//! collision ordering own separate invariants so no future wire encoder has to
//! recover semantics from a monolithic helper.

use super::{canonical::CanonicalFullPlan, fault::CoreSemanticImageFault};

mod graph;
mod model;
mod order;
mod traverse;

#[cfg(test)]
mod tests;

pub(crate) use model::*;

pub(crate) use graph::for_each_edge;
pub use model::{TypedPlanFault, TypedPlanNode, TypedPlanTerminal};
pub use traverse::TypedPlanError;
pub(crate) use traverse::{TypedDependencyPlan, compare_role, target_tag};

// The full-image planner consumes only canonical coordinates from the typed
// dependency subgraph.  It never reaches into the graph scratch or converts a
// raw pool ordinal itself, so type-owned and terminal-only pool invariants
// remain separate.
impl<'image> TypedDependencyPlan<'image> {
    pub(crate) fn canonical_type(&self, id: crate::TypeId) -> Result<u32, TypedPlanError> {
        self.canonical_node(model::TypedPlanNode::Type(id))
    }

    pub(crate) fn canonical_atom_list(&self, id: crate::AtomListId) -> Result<u32, TypedPlanError> {
        self.canonical_node(model::TypedPlanNode::AtomList(id))
    }

    pub(crate) fn canonical_type_list(&self, id: crate::TypeListId) -> Result<u32, TypedPlanError> {
        self.canonical_node(model::TypedPlanNode::TypeList(id))
    }

    pub(crate) fn canonical_type_parameters(
        &self,
        id: crate::TypeParameterListId,
    ) -> Result<u32, TypedPlanError> {
        self.canonical_node(model::TypedPlanNode::TypeParameters(id))
    }

    pub(crate) fn canonical(&self) -> &CanonicalFullPlan<'image> {
        &self.canonical
    }

    fn canonical_node(&self, node: model::TypedPlanNode) -> Result<u32, TypedPlanError> {
        let slot = self.slot(node)?;
        self.scratch
            .canonical_slots
            .get(slot)
            .copied()
            .ok_or(model::TypedPlanFault::MissingNode {
                node,
                count: self.counts.at(node.domain()),
            })
            .map_err(Into::into)
    }
}
