//! The typed events stores emit, and the direction they are allowed to flow.
//! Workspace is the only source of truth; shell is the only sink.
//! An event names what changed, never what a view should do about it.
//!
//! Keeping the graph acyclic is what stops "update storms": a shelf delta can
//! reach the reader, and the reader can reach the status bar, but nothing the
//! status bar does can travel back. Views subscribe; stores never subscribe to
//! views.

use backend_present::Fault;

/// Something changed in the shelf, the live feed, or the engine's health.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceEvent {
    /// The set of projects, or one project's readiness, changed.
    ShelfChanged,
    /// The visible revision advanced; rows and coverage are new.
    RowsChanged,
    /// A bounded snapshot is being hydrated; geometry is reserved.
    Hydrating,
    /// The live feed or a command failed.
    Faulted(Box<Fault>),
    /// The live feed recovered after a failure.
    Recovered,
    /// The capability inventory changed.
    CapabilitiesChanged,
}

/// Something changed in the omnibar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchEvent {
    /// Query, mode, results, or selection changed.
    Changed,
    /// The reader chose a result or a command.
    Accepted,
    /// The sheet closed.
    Dismissed,
}

/// Something changed in the reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DocumentEvent {
    /// A tab opened, closed, or became active.
    TabsChanged,
    /// The active tab navigated to another page.
    Navigated,
    /// A hover card became available or went away.
    HoverChanged,
}

/// Something changed in the set of durable intents in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobsEvent {
    /// A job was submitted, progressed, completed, or failed.
    Changed,
}

/// Something changed in the shell: panels, preferences, or notices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellEvent {
    /// A panel opened, closed, or resized.
    Layout,
    /// A persisted preference changed.
    Preferences,
    /// A transient notice was raised or dismissed.
    Notice,
}

/// Something changed in the registry catalog behind the add-a-project field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogEvent {
    /// A catalog lookup started, answered, or failed.
    Changed,
}
