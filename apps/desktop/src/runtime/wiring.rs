//! Application wiring for the GPUI shell boundary.

use super::actor::{ActorStartError, EngineClient};
use super::coordinator::DesktopRuntime;
use super::ui_graph::UiEntityGraph;
use crate::model::AppSnapshot;
use gpui::App;

/// Installs the UI entity graph and its dedicated engine actor on one GPUI
/// application. The client is moved to the worker before the first frame.
pub fn install_shell<C: EngineClient>(
    cx: &mut App,
    snapshot: AppSnapshot,
    client: C,
    mailbox_capacity: usize,
) -> Result<UiEntityGraph, ActorStartError> {
    let actor = super::actor::EngineActor::start(client, mailbox_capacity)?;
    Ok(UiEntityGraph::install(
        cx,
        DesktopRuntime::new(snapshot, actor),
        None,
    ))
}
