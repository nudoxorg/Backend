//! Inter-store events — the complete set of signals that cross store boundaries.
//!
//! ## Acyclicity invariant (GUI-PLAN §12.10)
//!
//! The event graph MUST remain a DAG. A cycle (A emits → B listens → B emits
//! → A listens) would cause reentrant `update` calls on the GPUI thread, which
//! panics. The flow below is the frozen topology; adding a reverse edge is a
//! review-blocking error.
//!
//! ```text
//! SettingsStore ──SettingsChanged──────────▶ everything
//! ProjectStore  ──ActiveChanged────────────▶ SearchStore · ShellStore · JobStore
//! SearchStore   ──OpenSymbol─────────────── ▶ SymbolStore.open → ShellStore + NavHistory.push
//! SymbolStore   ──HeadReady / SectionArrived▶ (views only — no store dependency)
//! RegistryStore ──GenerationChanged─────────▶ ProjectStore dep rows · status bar · Jobs
//! JobStore      ──ToastRequest──────────────▶ ShellStore toasts
//! NavHistory    ──Navigated─────────────────▶ ShellStore (activate tab / screen)
//! LogStore      ──(seq polling via drain)────▶ LogPanel only
//! ```
//!
//! Only the events that actually cross a store→store edge are declared here.
//! Events that go only to views (e.g. `HeadReady`, `SectionArrived`) are
//! defined inside their store files where they are emitted; they do NOT belong
//! here because they do not create store→store dependencies.

use nudox_engine::wire::{SymbolKey, SectionId};

use crate::stores::nav::NavEntry;
use crate::stores::symbol::TabId;

// ---------------------------------------------------------------------------
// SettingsStore → everyone
// ---------------------------------------------------------------------------

/// Emitted whenever persisted settings change.
///
/// Every store and view that depends on settings subscribes to this. The
/// payload is the full delta so subscribers can ignore fields they do not
/// care about without holding their own snapshot.
#[derive(Clone, Debug)]
pub struct SettingsChanged {
    /// Theme may have changed.
    pub theme: bool,
    /// Motion scale may have changed (drives `Motion::snap_to` vs `animate_to`).
    pub motion_scale: bool,
    /// Search debounce or mode flags may have changed.
    pub search: bool,
    /// Language-level flags (local/remote/off) may have changed.
    pub languages: bool,
}

// ---------------------------------------------------------------------------
// ProjectStore → SearchStore · ShellStore · JobStore
// ---------------------------------------------------------------------------

/// The active project changed.
///
/// Consumed by:
/// - `SearchStore` — resets the dep-set filter and re-scopes results.
/// - `ShellStore`  — updates the window title.
/// - `JobStore`    — switches which pipeline state is foregrounded.
#[derive(Clone, Debug)]
pub struct ActiveProjectChanged {
    /// The new active project id, or `None` if no project is open.
    pub project_id: Option<crate::stores::project::ProjectId>,
}

// ---------------------------------------------------------------------------
// SearchStore → SymbolStore + ShellStore + NavHistory
// ---------------------------------------------------------------------------

/// The user committed a search selection — open this symbol.
///
/// Consumed by `SymbolStore.open`, which then emits `TabActivated` to
/// `ShellStore` and pushes a `NavHistory` entry.
///
/// Disposition controls how the tab opens:
/// - `Replace`   — replace the currently active tab (default Enter).
/// - `Background`— open behind the current tab (alt-Enter).
/// - `Stay`      — open the tab but keep the omni overlay open (cmd-Enter).
#[derive(Clone, Debug)]
pub struct OpenSymbol {
    pub key: SymbolKey,
    pub disposition: OpenDisposition,
}

/// How a newly opened symbol tab should be placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenDisposition {
    /// Replace the currently active tab (or open in foreground if no tab).
    Replace,
    /// Open as a background tab; do not activate it.
    Background,
    /// Open in foreground but do not close the overlay that triggered this.
    Stay,
}

// ---------------------------------------------------------------------------
// SymbolStore → views only (NOT cross-store)
// ---------------------------------------------------------------------------
//
// `HeadReady` and `SectionArrived` are defined in `symbol.rs` because they
// only flow to views. Declaring them here would be misleading — views should
// subscribe to `SymbolStore` directly, not to a global event bus.

// ---------------------------------------------------------------------------
// RegistryStore → ProjectStore · status bar · JobStore
// ---------------------------------------------------------------------------

/// A corpus generation changed status.
///
/// `RegistryStore` is the single source of truth for generation status.
/// Consumers hold generation ids and re-read `RegistryStore` state on receipt;
/// they do not copy the status into their own fields.
#[derive(Clone, Debug)]
pub struct GenerationChanged {
    pub id: crate::stores::registry::GenerationId,
}

// ---------------------------------------------------------------------------
// JobStore → ShellStore
// ---------------------------------------------------------------------------

/// A job completed and wants a toast notification.
///
/// `ShellStore` enqueues these; the toast view renders them. The event is
/// one-shot: it is not idempotent state, it is a completion signal.
#[derive(Clone, Debug)]
pub struct ToastRequest {
    pub message: std::sync::Arc<str>,
    /// Optional action label + identity (e.g. "View" → open the job's output).
    pub action: Option<ToastAction>,
    /// `true` for job failures (renders with `danger` accent).
    pub is_error: bool,
}

/// A single optional action on a toast (GUI-PLAN §13.7).
#[derive(Clone, Debug)]
pub struct ToastAction {
    pub label: &'static str,
    pub job_id: crate::stores::job::JobId,
}

// ---------------------------------------------------------------------------
// NavHistory → ShellStore
// ---------------------------------------------------------------------------

/// The user navigated back or forward and a different entry is now current.
///
/// `ShellStore` activates the tab or screen named by the entry, or opens a
/// new tab if the entry's target is no longer open.
#[derive(Clone, Debug)]
pub struct Navigated {
    pub entry: NavEntry,
    /// When `true` the shell should animate the `nav.flash` pulse on the
    /// target tab (the entry was already-open, GUI-PLAN §5.3).
    pub flash: bool,
}

// ---------------------------------------------------------------------------
// SymbolStore → ShellStore (tab lifecycle)
// ---------------------------------------------------------------------------

/// `SymbolStore.open` completed; the shell should bring this tab to the front.
#[derive(Clone, Debug)]
pub struct TabActivated {
    pub tab_id: TabId,
    pub disposition: OpenDisposition,
}

/// A symbol page has a `SymbolHead` and is ready to paint its header.
///
/// Consumed only by views (`SymbolPage`); listed here for discoverability but
/// NOT subscribed to by any store — keeping the graph acyclic.
#[derive(Clone, Debug)]
pub struct HeadReady {
    pub tab_id: TabId,
    pub key: SymbolKey,
}

/// A new section arrived on a symbol page's stream.
///
/// Consumed only by views. Same acyclicity note as `HeadReady`.
#[derive(Clone, Debug)]
pub struct SectionArrived {
    pub tab_id: TabId,
    pub section_id: SectionId,
}

/// The reference or impl counts on a tab changed (ticking count labels).
///
/// Consumed only by views (tab label). Same acyclicity note.
#[derive(Clone, Debug)]
pub struct TabCountsChanged {
    pub tab_id: TabId,
    pub refs_count: u64,
    pub impls_count: u64,
}
