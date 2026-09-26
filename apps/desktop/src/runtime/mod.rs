//! Background runtime, mapping, animation, and GPUI entity wiring.

pub mod actor;
pub mod client;
pub mod coordinator;
pub mod debug_page;
pub mod mailbox;
pub mod mapping;
pub mod page_mapping;
pub mod reads;
pub mod store;
pub mod ui_graph;
pub mod wake;
pub mod wiring;

#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod tests;

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
