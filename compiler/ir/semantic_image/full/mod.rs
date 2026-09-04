//! Private full-image canonical preparation.
//!
//! This is intentionally not a reader or wire grammar. It joins only the
//! common planes whose canonical coordinates are now known: recursive typed
//! pools, terminal entity/docs lists, complete entity pooled references, and
//! graph relations plus occurrence authority. The seven extension planes and
//! their remapped fact pools must join before any full image capability can be
//! exposed.

mod entities;
mod extensions;
mod graph;
mod lists;
mod model;

#[cfg(test)]
mod tests;

use crate::Ir;

use super::typed::TypedDependencyPlan;
use super::full_wire::FullTypedPlan;

pub(super) use model::FullPlanError;

use graph::GraphPlanBuildError;
use lists::CoreTerminalError;
use model::{ExtensionPlans, FullEntityPlan, GraphPlan};
use lists::TerminalPools;

/// Measured private plan for every completed common full-image plane.
pub(super) struct FullSemanticPlan<'image> {
    pub(super) typed: TypedDependencyPlan<'image>,
    /// Explicit canonical generic type/list rows for the future complete wire.
    /// They are prepared with the rest of the transaction so no writer has to
    /// inspect native `TypeExpr` storage after caller output is mutable.
    pub(super) typed_wire: FullTypedPlan,
    pub(super) terminal: TerminalPools,
    pub(super) entities: FullEntityPlan,
    pub(super) graph: GraphPlan,
    pub(super) extensions: ExtensionPlans,
}

impl<'image> FullSemanticPlan<'image> {
    /// Canonicalizes every currently supported full common plane. It does not
    /// expose a reader because language extension pools remain outside this
    /// transaction.
    pub(super) fn build(ir: &'image Ir) -> Result<Self, FullPlanError> {
        let typed = TypedDependencyPlan::build(ir)?;
        let typed_wire = FullTypedPlan::build(&typed)?;
        let terminal = TerminalPools::build(ir, typed.canonical()).map_err(map_terminal)?;
        let entities = FullEntityPlan::build(ir, &typed, &terminal)?;
        let graph = GraphPlan::build(ir, typed.canonical()).map_err(map_graph)?;
        let extensions = ExtensionPlans::build(ir, &typed, &terminal)?;
        Ok(Self { typed, typed_wire, terminal, entities, graph, extensions })
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
