//! Private full-image canonical preparation.
//!
//! This is intentionally not a reader or wire grammar. It prepares the
//! coordinate-free inputs for every full-image plane: recursive typed pools,
//! terminal entity/docs lists, complete entity pooled references, graph
//! evidence and occurrence authority, and the seven sparse extension planes.

mod entities;
mod extensions;
mod graph;
mod lists;
mod model;

#[cfg(test)]
mod tests;

use crate::Ir;

use super::full_wire::FullTypedPlan;
use super::typed::TypedDependencyPlan;

pub(crate) use model::{
    ExtensionBinding, ExtensionPlanePlan, ExtensionPlans, FullEntityPlan, FullEntityRow, GraphPlan,
};
pub use model::{
    ExtensionPlanFault, FullEntityFault, FullPlanError, GraphPlanFault, TerminalPoolDomain,
    TerminalPoolFault,
};

use graph::GraphPlanBuildError;
use lists::CoreTerminalError;
use lists::TerminalPools;

/// Measured private plan for every completed common full-image plane.
pub(crate) struct FullSemanticPlan<'image> {
    pub(crate) typed: TypedDependencyPlan<'image>,
    /// Explicit canonical generic type/list rows for the complete wire.
    /// They are prepared with the rest of the transaction so no writer has to
    /// inspect native `TypeExpr` storage after caller output is mutable.
    pub(crate) typed_wire: FullTypedPlan,
    pub(crate) terminal: TerminalPools,
    pub(crate) entities: FullEntityPlan,
    pub(crate) graph: GraphPlan,
    pub(crate) extensions: ExtensionPlans,
}

impl<'image> FullSemanticPlan<'image> {
    /// Canonicalizes every full-image source plane before the writer borrows
    /// caller output mutably.
    pub(crate) fn build(ir: &'image Ir) -> Result<Self, FullPlanError> {
        let typed = TypedDependencyPlan::build(ir)?;
        let typed_wire = FullTypedPlan::build(&typed)?;
        let terminal = TerminalPools::build(ir, typed.canonical()).map_err(map_terminal)?;
        let entities = FullEntityPlan::build(ir, &typed, &terminal)?;
        let graph = GraphPlan::build(ir, typed.canonical()).map_err(map_graph)?;
        let extensions = ExtensionPlans::build(ir, &typed, &terminal)?;
        Ok(Self {
            typed,
            typed_wire,
            terminal,
            entities,
            graph,
            extensions,
        })
    }
}

fn map_terminal(cause: CoreTerminalError) -> FullPlanError {
    match cause {
        CoreTerminalError::Core(cause) => cause.into(),
        CoreTerminalError::Terminal(cause) => cause.into(),
    }
}

fn map_graph(cause: GraphPlanBuildError) -> FullPlanError {
    match cause {
        GraphPlanBuildError::Core(cause) => cause.into(),
        GraphPlanBuildError::Graph(cause) => cause.into(),
    }
}
