//! Background runtime, mapping, animation, and GPUI entity wiring.

pub(crate) mod acquire;
pub mod actor;
pub mod browse_reads;
pub(crate) mod browse_views;
pub mod client;
pub mod coordinator;
pub mod debug_page;
pub(crate) mod fixture_releases;
pub(crate) mod fixture_world;
pub(crate) mod graph_focus;
pub mod mailbox;
pub mod mapping;
pub(crate) mod offload;
pub(crate) mod owner;
pub mod page_mapping;
pub mod reads;
pub(crate) mod snapshot;
pub mod store;
pub mod trace;
pub(crate) mod traffic;
pub mod ui_graph;
pub mod wake;
pub mod wiring;
pub(crate) mod workspace_lines;

#[cfg(test)]
mod acquire_tests;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod wait;

pub use actor::{
    ActorStartError, CancellationToken, EngineActor, EngineClient, EngineDto, EngineEvent,
    EngineFault, EngineRequest, LocalRead, ProjectDto,
};
pub use client::LocalEngineClient;
pub use coordinator::{DesktopRuntime, RuntimeEvent};
pub use mailbox::{CoalesceKey, Coalescible, CoalescingMailbox, PushResult};
pub use mapping::{MappingError, map_event};
pub use ui_graph::{UiEntityGraph, UiRootEntity};
pub use wiring::install_shell;
