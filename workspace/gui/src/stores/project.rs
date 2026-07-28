//! `ProjectStore` — GUI-PLAN §12.2
//!
//! Tracks open projects, the active project, and per-project dependency
//! resolution state. This is a minimal skeleton: the full dep-resolution
//! stream is specified in §12.2 but depends on `client.resolve_project`
//! which is a Wave-4 capability. The types and entry points are real; the
//! stream wiring is forward-declared as stubs so later milestones can fill
//! them without restructuring.

use gpui::{Context, Entity};

/// Opaque newtype for project identity.
///
/// In production this will derive from a workspace root path hash; for now it
/// is an incrementing integer assigned at `open()` time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectId(pub u64);

/// A single row in the recent-projects list.
#[derive(Clone, Debug)]
pub struct ProjectRow {
    pub id: ProjectId,
    /// Filesystem path to the workspace root.
    pub root: std::path::PathBuf,
    /// Display label (last path component or workspace name).
    pub label: gpui::SharedString,
}

/// Minimal `ProjectStore` state needed so `events.rs` can reference
/// `ProjectId` without a circular dependency.
pub struct ProjectStore {
    pub projects: Vec<ProjectRow>,
    pub active: Option<ProjectId>,
    _next_id: u64,
}

impl ProjectStore {
    pub fn new() -> Self {
        Self {
            projects: Vec::new(),
            active: None,
            _next_id: 1,
        }
    }

    /// Record a project as open and make it active.
    pub fn set_active(&mut self, id: Option<ProjectId>, cx: &mut Context<Self>) {
        self.active = id;
        cx.emit(crate::stores::events::ActiveProjectChanged { project_id: id });
        cx.notify();
    }
}

impl Default for ProjectStore {
    fn default() -> Self {
        Self::new()
    }
}

impl gpui::EventEmitter<crate::stores::events::ActiveProjectChanged> for ProjectStore {}
