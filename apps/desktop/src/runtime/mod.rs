//! Background runtime, mapping, animation, and GPUI entity wiring.

pub mod actor;
pub mod animation;
pub mod client;
pub mod coordinator;
pub mod mailbox;
pub mod mapping;
pub mod ui_graph;
pub mod wiring;

#[cfg(test)]
mod tests;

pub use actor::{
    ActorStartError, CancellationToken, EngineActor, EngineClient, EngineDto, EngineEvent,
    EngineFault, EngineRequest, ProjectDto,
};
pub use animation::{
    AnimationChannel, AnimationId, AnimationTimeline, Beat, CaptureFrameClock, Easing, FrameClock,
    LiveFrameClock, Motion, TimelineVersion, TrackSnapshot,
};
pub use client::LocalEngineClient;
pub use coordinator::{DesktopRuntime, RuntimeEvent};
pub use mailbox::{CoalesceKey, Coalescible, CoalescingMailbox, PushResult};
pub use mapping::{MappingError, map_event};
pub use ui_graph::{UiEntityGraph, UiRootEntity};
pub use wiring::install_shell;
