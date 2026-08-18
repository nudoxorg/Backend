//! Project panel — the left-dock "what am I looking at" view (GUI-PLAN §14 /
//! GUI-LOCAL-PLAN §L10).
//!
//! # Role
//!
//! The project panel is the first thing the user sees when lindsey opens.  It
//! answers two questions:
//!
//! 1. **What is in the corpus?** — every package the session knows about, with
//!    its status (loading, ready with a symbol count, or failed with the
//!    operator-facing reason).
//! 2. **What can I do here?** — if nothing has loaded, a designed empty state
//!    explains how to point lindsey at a package (`NUDOX_PACKAGE_ROOT` /
//!    `NUDOX_CORPUS`; see `app::corpus`).
//!
//! # Why this panel must never silently show an empty corpus
//!
//! The bug this project keeps encountering: a package that takes 30 s to lower
//! reads as a broken app if the panel stays blank.  `PackageStore` is seeded
//! with `Pending` rows at construction time (see `stores::package` docs), so
//! even before the first `PackageLoadEvent` the panel shows "loading axum…".
//! A `PackageStatus::Failed` row is rendered with its message *visible* (no
//! expansion required) so the user cannot mistake a load failure for an empty
//! corpus.
//!
//! # Precomputed state (§1.1.4)
//!
//! Every `SharedString` that appears in a row — the name, the status badge
//! text, the symbol count — is formatted **once**, inside `refresh_rows`, which
//! is called by the `PackagesChanged` subscription handler.  The `render` body
//! only clones `SharedString`s and `Arc`s; no `format!` ever runs there.
//!
//! # Keyboard (LD-13)
//!
//! The panel sets the key context `"ProjectPanel"`.  `keymaps.rs` binds `s` to
//! `SyncSelected` in that context.  The handler is a no-op stub until Wave-4
//! ships the sync RPC; what matters is that the binding is *wired* here so the
//! command palette and `?` overlay are honest about what `s` does.
//!
//! # Store capability trait
//!
//! `ProjectPanel<P: PackageAccess>` is generic over a narrow read trait rather
//! than over `PackageStore<E>` directly.  This mirrors the `SearchEngine` /
//! `SymbolEngine` pattern used elsewhere in the store layer and is what makes
//! the unit tests below runnable without a Tokio runtime or a live engine.

use gpui::{
    App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Subscription, Task, UniformListScrollHandle, Window,
    div, prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState};
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};
use nudox_engine::wire::{
    EcosystemId, Gen, PackageLineageId, PackageName, VersionEvent, VersionList,
};
use serde_json::Value as JsonValue;

use crate::app::actions::SyncSelected;
use crate::bridge::drain::drain;
use crate::bridge::generation::GenSource;
use crate::motion::tokens::MotionTokens;
use crate::stores::events::{PackageActivated, PackagesChanged};
use crate::stores::package::{PackageAccess, PackageRow, PackageStatus};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::ui::EmptyState;

// ─────────────────────────────────────────────────────────────────────────────
// Pre-built static strings (§1.1.4 — nothing constructed in render)
// ─────────────────────────────────────────────────────────────────────────────

/// Empty-state description, built at compile time.
///
/// Using `concat!` rather than `format!` so the string is a `&'static str`
/// that costs nothing at runtime and is never re-allocated inside `render`.
const EMPTY_STATE_DESCRIPTION: &str = concat!(
    "Point lindsey at a package with NUDOX_PACKAGE_ROOT=<path>, ",
    "or use the built-in fixture corpus.",
);

// ─────────────────────────────────────────────────────────────────────────────
// Precomputed row state (§1.1.4)
// ─────────────────────────────────────────────────────────────────────────────

/// The status of one package, expressed as pre-formatted display strings.
///
/// Built by [`PreparedPackageRow::from_row`] at subscription-event time, never
/// inside `render`.
#[derive(Clone, Debug)]
enum RowStatus {
    /// Still lowering.  `status_text` is already formatted as `"loading axum…"`.
    Pending {
        /// Pre-formatted status text, e.g. `"loading axum…"`.
        status_text: SharedString,
    },
    /// Lowered and ready; shows the symbol count.
    Ready {
        /// Pre-formatted: `"3,060 symbols"` or `"1 symbol"`.
        symbol_count_label: SharedString,
    },
    /// The producer failed.  The message is shown inline — a silent failure is
    /// the exact bug this panel exists to prevent.
    Failed {
        /// Operator-facing error, verbatim from the engine.
        message: SharedString,
    },
}

/// One display row, fully pre-formatted (§1.1.4).
#[derive(Clone, Debug)]
struct PreparedPackageRow {
    /// Package name — a `SharedString` so cloning into the `uniform_list`
    /// closure is arc-clone-cheap.
    name: SharedString,
    /// Formatted status, determined at ingest time.
    status: RowStatus,
    /// The lineage id for this package — needed by version-picker actions.
    lineage: Option<PackageLineageId>,
    /// Pre-formatted current version label, e.g. `"v0.8.0"`.
    ///
    /// `None` when the package is pending or has no loaded generation.
    /// Precomputed at refresh time so render never calls `format!`.
    current_version_label: Option<SharedString>,
    /// The loaded versions for this package.  Cheap to store (`Arc`-backed
    /// slice) and rebuilt on every `PackagesChanged` by calling `store.versions`.
    versions: VersionList,
    /// The package's root symbol, once it is known.
    ///
    /// Carried on the prepared row so activation needs no lookup: the panel
    /// already holds the answer, and re-deriving it at click time would let
    /// the row and the destination disagree about which symbol a package's
    /// page is.
    root: Option<nudox_engine::SymbolKey>,
    /// Pre-formatted manifest provenance, if any field is known.
    provenance_label: Option<SharedString>,
}

impl PreparedPackageRow {
    /// Build from the store's canonical `PackageRow` and current version list.
    ///
    /// This is the only place `format!` is allowed to run for row content.
    /// Everything that comes out of here is a `SharedString` and never
    /// re-formatted.
    fn from_row(row: &PackageRow, versions: VersionList) -> Self {
        let status = match &row.status {
            PackageStatus::Pending => RowStatus::Pending {
                status_text: SharedString::from(format!("loading {}…", row.name)),
            },
            PackageStatus::Ready { symbols } => {
                // Provenance is projected only from fields the engine actually
                // extracted. Unknown fields remain absent rather than becoming
                // guessed labels; the engine also retains this metadata on the
                // immutable package view for late subscribers.
                let ecosystem = row
                    .lineage
                    .as_ref()
                    .map(|l| l.ecosystem.as_str().to_owned());
                let generations = versions.len();
                let mut label = match symbols {
                    1 => "1 symbol".to_owned(),
                    n => format!("{n} symbols"),
                };
                if let Some(ecosystem) = ecosystem {
                    label = format!("{ecosystem} · {label}");
                }
                if generations > 1 {
                    label = format!("{label} · {generations} versions");
                }
                RowStatus::Ready {
                    symbol_count_label: SharedString::from(label),
                }
            }
            PackageStatus::Failed { message } => RowStatus::Failed {
                message: message.clone(),
            },
        };
        // Pre-format the current version label once, here, not in render.
        let current_version_label = row
            .current_version
            .clone()
            .or_else(|| {
                versions
                    .current()
                    .map(|v| SharedString::from(v.version.to_string()))
            })
            .map(|version| SharedString::from(format!("v{version}")));
        let provenance_label = {
            let mut fields = Vec::new();
            if let Some(description) = &row.metadata.description {
                fields.push(description.clone());
            }
            if let Some(repository) = &row.metadata.repository {
                fields.push(repository.clone());
            }
            if let Some(license) = &row.metadata.license {
                fields.push(format!("license {license}"));
            }
            if let Some(owner) = &row.metadata.owner {
                fields.push(format!("owner {owner}"));
            }
            if !row.metadata.dependencies.is_empty() {
                fields.push(format!("{} dependencies", row.metadata.dependencies.len()));
            }
            if let Some(release_date) = &row.metadata.release_date {
                fields.push(format!("released {release_date}"));
            }
            if let Some(homepage) = &row.metadata.homepage {
                fields.push(format!("homepage {homepage}"));
            }
            if let Some(coverage) = &row.metadata.coverage {
                if coverage.total > 0 {
                    fields.push(format!(
                        "docs {}%",
                        coverage.documented.saturating_mul(100) / coverage.total
                    ));
                }
            }
            (!fields.is_empty()).then(|| SharedString::from(fields.join(" · ")))
        };
        PreparedPackageRow {
            name: row.name.clone(),
            status,
            lineage: row.lineage.clone(),
            current_version_label,
            versions,
            root: row.root.clone(),
            provenance_label,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Selection movement policy (pure, testable without a window)
// ─────────────────────────────────────────────────────────────────────────────

/// Move the selected row index, clamping to the available range.
///
/// Returns `None` when there are no rows at all.
/// Returns `Some(0)` when moving up from 0 (hold at top).
/// Returns `Some(len-1)` when moving down from len-1 (hold at bottom).
///
/// Matches the §15 no-silent-wrap property used in `omni_search.rs`.
pub fn step_selection(current: Option<usize>, down: bool, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let ix = match current {
        None => {
            if down {
                0
            } else {
                len - 1
            }
        }
        Some(ix) => {
            if down {
                (ix + 1).min(len - 1)
            } else {
                ix.saturating_sub(1)
            }
        }
    };
    Some(ix)
}

// ─────────────────────────────────────────────────────────────────────────────
// ProjectPanel
// ─────────────────────────────────────────────────────────────────────────────

/// The left-dock project / corpus panel.
///
/// Generic over `P: PackageAccess` so it can be driven by the real
/// `PackageStore<EngineHandle>` in production and by a test double in unit
/// tests — exactly as `OmniSearch<S>` is generic over `SearchAccess`.
pub struct ProjectPanel<P: PackageAccess> {
    /// The live package store.
    store: Entity<P>,
    /// Focus handle.  The panel must be keyboard-reachable.
    focus: FocusHandle,

    /// Precomputed rows — rebuilt on every [`PackagesChanged`] (§1.1.4).
    rows: Vec<PreparedPackageRow>,
    /// Row count as a pre-formatted `SharedString` for the section header.
    row_count_label: SharedString,
    /// Zero-based selected row index.
    selection: Option<usize>,
    /// Scroll handle so keyboard selection keeps the chosen row visible.
    scroll: UniformListScrollHandle,

    /// The row whose version picker is currently open.
    ///
    /// `None` when no picker is showing.  Only makes sense to show a picker
    /// when `rows[ix].versions.len() > 1`.
    version_picker_open: Option<usize>,

    /// Generation source for version-switch requests.
    ///
    /// Each call to `select_version` uses a fresh gen so a stale
    /// `VersionEvent` arriving after the user makes a second selection is
    /// discarded before it can corrupt visible state (§9.3).
    version_gen: GenSource,

    /// Toast message set when `VersionEvent::NotLoaded` arrives.
    ///
    /// Precomputed here so render never calls `format!` (§1.1.4).
    version_not_loaded_toast: Option<SharedString>,

    /// Drain task for the current version-switch response stream.
    ///
    /// Assigned every time `select_version` is called; dropping the previous
    /// value cancels its drain loop.  LD-18: owned, never `.detach()`-ed.
    _version_drain: Option<Task<()>>,

    /// Subscription that keeps `rows` in sync with the store.
    ///
    /// Owned here rather than `.detach()`-ed: when the panel is dropped, the
    /// subscription is cancelled (LD-18).
    _sub: Subscription,
}

impl<P: PackageAccess> ProjectPanel<P> {
    /// Construct the panel, subscribing to the store immediately.
    ///
    /// # Signature (for the shell wiring)
    ///
    /// ```ignore
    /// ProjectPanel::new(store: Entity<P>, _window: &mut Window, cx: &mut Context<ProjectPanel<P>>) -> Self
    /// ```
    ///
    /// The shell passes its existing `packages` entity and forwards `window` /
    /// `cx` from the `cx.new(|cx| …)` call.  Example (in `shell.rs`):
    ///
    /// ```ignore
    /// let project_panel = cx.new(|cx| ProjectPanel::new(packages.clone(), window, cx));
    /// ```
    pub fn new(store: Entity<P>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();

        // Snapshot the initial rows so the very first frame is not empty while
        // waiting for the first PackagesChanged to fire.
        let initial_rows: Vec<PreparedPackageRow> = {
            let s = store.read(cx);
            s.rows()
                .iter()
                .map(|r| {
                    let vl = r
                        .lineage
                        .as_ref()
                        .map(|lid| s.versions(lid))
                        .unwrap_or_else(|| VersionList {
                            package: default_lineage(),
                            versions: std::sync::Arc::from(vec![]),
                        });
                    PreparedPackageRow::from_row(r, vl)
                })
                .collect()
        };
        let row_count_label = format_count(initial_rows.len());

        // Subscribe to the store.  Fires for every PackagesChanged (engine
        // events arrive in bursts — the drain batches them into one notify and
        // one PackagesChanged per burst, so this fires at most once per frame).
        let sub = cx.subscribe(
            &store,
            |this: &mut Self, store_entity, _: &PackagesChanged, cx| {
                let s = store_entity.read(cx);
                let snapshot = s.rows();
                // We need to call s.versions inside the same read borrow; collect
                // everything needed into an owned vec before releasing the borrow.
                let prepared: Vec<PreparedPackageRow> = snapshot
                    .iter()
                    .map(|r| {
                        let vl = r
                            .lineage
                            .as_ref()
                            .map(|lid| s.versions(lid))
                            .unwrap_or_else(|| VersionList {
                                package: default_lineage(),
                                versions: std::sync::Arc::from(vec![]),
                            });
                        PreparedPackageRow::from_row(r, vl)
                    })
                    .collect();
                drop(s); // release borrow before mutable methods
                this.refresh_rows(prepared, cx);
            },
        );

        Self {
            store,
            focus,
            rows: initial_rows,
            row_count_label,
            selection: None,
            scroll: UniformListScrollHandle::new(),
            version_picker_open: None,
            version_gen: GenSource::new(),
            version_not_loaded_toast: None,
            _version_drain: None,
            _sub: sub,
        }
    }

    // ── State management ─────────────────────────────────────────────────────

    /// Rebuild `rows` from a set of already-prepared rows.
    ///
    /// Called from the `PackagesChanged` subscription handler.  All `format!`
    /// calls that produce display strings happen in `PreparedPackageRow::from_row`,
    /// which runs in the subscription closure before this is called — so by the
    /// time we arrive here, everything is a `SharedString` (§1.1.4).
    fn refresh_rows(&mut self, prepared: Vec<PreparedPackageRow>, cx: &mut Context<Self>) {
        self.rows = prepared;
        self.row_count_label = format_count(self.rows.len());

        // If the selection pointed at a row that no longer exists, clear it.
        if let Some(ix) = self.selection {
            if ix >= self.rows.len() {
                self.selection = None;
            }
        }
        // Close the version picker if its row vanished.
        if let Some(ix) = self.version_picker_open {
            if ix >= self.rows.len() {
                self.version_picker_open = None;
            }
        }
        cx.notify();
    }

    // ── Selection ────────────────────────────────────────────────────────────

    /// Apply a new selection index, scrolling the row into view.
    fn apply_selection(&mut self, next: Option<usize>, cx: &mut Context<Self>) {
        self.selection = next;
        if let Some(ix) = next {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    // ── Action handlers ───────────────────────────────────────────────────────

    /// `s` — sync the selected dependency (§14, Wave-4 stub).
    ///
    /// The binding is wired even though no RPC exists yet: an unbound key in
    /// the keymap registry is a bug (see `app::mod` docs on why the registry
    /// and the bindings live together).  The no-op here is intentional; it
    /// will be replaced when `client.resolve_project` ships.
    fn on_sync_selected(&mut self, _: &SyncSelected, _: &mut Window, _cx: &mut Context<Self>) {
        // TODO(§14 Wave-4): call the sync RPC for self.rows[self.selection].
    }

    /// Open or close the version picker for row `ix`.
    ///
    /// Called from a click on the version chip.  Only opens when the row has
    /// more than one loaded generation — a single-version chip is read-only
    /// and clicking it is a no-op.
    fn toggle_version_picker(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.version_picker_open == Some(ix) {
            self.version_picker_open = None;
        } else if self.rows.get(ix).is_some_and(|r| r.versions.len() > 1) {
            self.version_picker_open = Some(ix);
        }
        // Clear any leftover error toast from a previous picker session.
        self.version_not_loaded_toast = None;
        cx.notify();
    }

    /// Handle the user picking a specific version from the dropdown.
    ///
    /// Fires `select_version` on the store, closes the picker, and starts a
    /// drain task for the response.  If `VersionEvent::NotLoaded` arrives,
    /// `version_not_loaded_toast` is set and the user sees an inline error.
    fn pick_version(&mut self, row_ix: usize, version: SharedString, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(row_ix) else {
            return;
        };
        let Some(lineage) = row.lineage.clone() else {
            return;
        };

        let generation = self.version_gen.next();
        let rx = self
            .store
            .read(cx)
            .select_version(lineage, &*version, generation);

        self.version_picker_open = None;
        self.version_not_loaded_toast = None;

        // LD-18: the drain task is owned by this struct field; dropping it
        // (by assigning a new one) cancels the previous response listener.
        self._version_drain = Some(drain(cx, rx, move |panel: &mut Self, event, cx| {
            match event {
                VersionEvent::Switched { .. } => {
                    // The corpus is now serving the new version; PackagesChanged
                    // will fire on the next `packages()` drain and rebuild rows.
                    panel.version_not_loaded_toast = None;
                    cx.notify();
                }
                VersionEvent::NotLoaded { version, .. } => {
                    // Version was not loaded — surface this to the user.
                    panel.version_not_loaded_toast = Some(SharedString::from(format!(
                        "Version {} is not loaded",
                        version,
                    )));
                    cx.notify();
                }
                _ => {}
            }
        }));
    }

    // ── Rendering helpers ─────────────────────────────────────────────────────

    /// Render the empty state — shown when no packages are known at all.
    ///
    /// Explains `NUDOX_PACKAGE_ROOT` so the user knows how to load something
    /// rather than seeing a blank panel that looks broken (LD-16).
    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let motion = MotionTokens::new(ext.motion_scale);

        div().w_full().flex_1().p(sp.space_4).child(EmptyState::new(
            IconName::Inbox,
            SharedString::from("No packages loaded"),
            SharedString::from(EMPTY_STATE_DESCRIPTION),
            motion,
        ))
    }

    /// Render one package row.
    ///
    /// The row is taller than a flat string: the name is in `ui` (primary)
    /// scale, the status detail is in `dense` (secondary) scale below it.
    ///
    /// `this` is passed so the version chip can call `toggle_version_picker`
    /// without borrowing the view — the `uniform_list` closure is `'static`
    /// and cannot hold a live borrow of `Self`.
    fn render_row(
        ix: usize,
        row: &PreparedPackageRow,
        selected: bool,
        picker_open: bool,
        this: gpui::WeakEntity<Self>,
        cx: &App,
    ) -> Stateful<Div> {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let al = ext.alpha;

        // Status line: text + colour, arc-cloned from pre-built strings (§1.1.4).
        let (status_text, status_colour): (SharedString, gpui::Hsla) = match &row.status {
            RowStatus::Pending { status_text } => (status_text.clone(), colours.fg_faint),
            RowStatus::Ready { symbol_count_label } => {
                (symbol_count_label.clone(), colours.fg_muted)
            }
            RowStatus::Failed { message } => (message.clone(), colours.danger),
        };

        // Status indicator dot — green (ok), amber (pending), red (failed).
        let dot_colour = match &row.status {
            RowStatus::Pending { .. } => colours.warn,
            RowStatus::Ready { .. } => colours.ok,
            RowStatus::Failed { .. } => colours.danger,
        };

        let bg = if selected {
            colours.bg_active
        } else {
            colours.bg_base
        };

        // Version chip — shown for Ready rows that have a loaded generation.
        // When only one generation is loaded the chip is read-only (a label,
        // not a button): opening a one-item dropdown looks broken.
        let has_choice = row.versions.len() > 1;
        let version_chip: Option<gpui::AnyElement> = row
            .current_version_label
            .as_ref()
            .filter(|_| matches!(&row.status, RowStatus::Ready { .. }))
            .map(|v_label| {
                if has_choice {
                    let this_chip = this.clone();
                    div()
                        .id(("project.row.version_chip", ix))
                        .debug_selector(move || format!("project.panel.row.{ix}.version"))
                        .px(sp.space_1)
                        .py(px(1.0))
                        .rounded(sp.r_sm)
                        // Opaque wash instead of `accent.opacity(0.15)`: this
                        // is a chip background, and a ramp step composites
                        // predictably instead of muddying against whatever the
                        // panel happens to sit on.
                        .bg(if picker_open {
                            colours.accent_wash
                        } else {
                            colours.bg_hover
                        })
                        .border(sp.border_width)
                        .border_color(if picker_open {
                            colours.accent.opacity(al.half)
                        } else {
                            colours.border_default
                        })
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .text_color(colours.fg_muted)
                        .cursor_pointer()
                        .hover(|s| s.bg(colours.accent_wash_hover))
                        .child(v_label.clone())
                        .on_click(move |_event, _window, cx| {
                            let _ = this_chip.update(cx, |panel, cx| {
                                panel.toggle_version_picker(ix, cx);
                            });
                        })
                        .into_any_element()
                } else {
                    // Single version: deliberate read-only label — a dropdown
                    // that opens to a single item looks broken.  The text makes
                    // the current version visible without implying a choice.
                    div()
                        .px(sp.space_1)
                        .py(px(1.0))
                        .rounded(sp.r_sm)
                        .bg(colours.bg_hover)
                        .border(sp.border_width)
                        .border_color(colours.border_default)
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .text_color(colours.fg_muted)
                        .child(v_label.clone())
                        .into_any_element()
                }
            });

        div()
            .id(("project.row", ix))
            // Test seam — see the note in `omni_search::render_row`. Records
            // painted bounds so an integration test can click this row through
            // the real hit-test pipeline. No-op outside test builds.
            .debug_selector(move || format!("project.panel.row.{ix}"))
            .w_full()
            .px(sp.space_3)
            .py(sp.space_2)
            .flex()
            .items_start()
            .gap(sp.space_2)
            .bg(bg)
            // Leading accent line: visible only when selected, so we do not
            // allocate an invisible border on every row.
            .when(selected, |el| {
                el.border_l(sp.border_width).border_color(colours.accent)
            })
            .hover(|s| s.bg(colours.bg_hover))
            .cursor_pointer()
            // ── Leading status dot ────────────────────────────────────────────
            .child(
                div()
                    .mt(px(6.0)) // align dot with first line of text
                    .w(sp.space_2)
                    .h(sp.space_2)
                    .rounded_full()
                    .bg(dot_colour)
                    .flex_shrink_0(),
            )
            // ── Name + status ─────────────────────────────────────────────────
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    // Primary: package name + version chip on the same line.
                    .child(
                        h_flex()
                            .items_center()
                            .gap(sp.space_2)
                            .overflow_hidden()
                            .child(
                                div()
                                    .text_size(ts.ui.size)
                                    .line_height(ts.ui.line_height)
                                    .font_weight(gpui::FontWeight(ts.title.weight as f32))
                                    .text_color(colours.fg_default)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(row.name.clone()),
                            )
                            .when_some(version_chip, |el, chip| el.child(chip)),
                    )
                    // Secondary: status text in `dense` scale
                    .child(
                        div()
                            .text_size(ts.dense.size)
                            .line_height(ts.dense.line_height)
                            .text_color(status_colour)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(status_text),
                    )
                    .when_some(row.provenance_label.clone(), |el, label| {
                        el.child(
                            div()
                                .text_size(ts.dense.size)
                                .line_height(ts.dense.line_height)
                                .text_color(colours.fg_faint)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(label),
                        )
                    }),
            )
            // ── Failed badge ──────────────────────────────────────────────────
            // A secondary "FAILED" chip in danger colours to make failures
            // un-missable even when skimming (they could be anywhere in a long
            // list).
            .when(matches!(&row.status, RowStatus::Failed { .. }), |el| {
                el.child(
                    div()
                        .px(sp.space_1)
                        .py(px(2.0))
                        .rounded(sp.r_sm)
                        .bg(colours.danger.opacity(al.wash))
                        .border(sp.border_width)
                        .border_color(colours.danger.opacity(al.veil))
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                        .text_color(colours.danger)
                        .child(SharedString::from("FAILED")),
                )
            })
    }

    /// The version dropdown panel — shown below a row when its picker is open.
    ///
    /// Only called when `row.versions.len() > 1`.  The dropdown is a plain
    /// list of version strings; the current one is highlighted with an accent
    /// background.
    ///
    /// Height constraint: `row_height * version_count` for an exact fit (layout
    /// trap avoidance: `max_h` without a definite height → zero inside
    /// `size_full`/`flex_1`).
    fn render_version_dropdown(
        row_ix: usize,
        row: &PreparedPackageRow,
        this: gpui::WeakEntity<Self>,
        cx: &App,
    ) -> gpui::AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;

        let versions = row.versions.clone();
        let version_count = versions.len();
        let row_h = f32::from(ts.dense.line_height) + f32::from(sp.space_1) * 2.0;

        // Build all rows eagerly (version lists are short — bounded by how many
        // generations the caller requested to load, typically 1-10).
        let items: Vec<gpui::AnyElement> = versions
            .versions
            .iter()
            .map(|vrow| {
                let version = SharedString::from((&*vrow.version).to_owned());
                let is_current = vrow.is_current;
                let this = this.clone();
                let v_for_click = version.clone();
                div()
                    .id(("project.version.item", row_ix))
                    .debug_selector(move || {
                        format!("project.panel.row.{row_ix}.version.{}", version.as_ref())
                    })
                    .w_full()
                    .px(sp.space_3)
                    .py(sp.space_1)
                    .flex()
                    .items_center()
                    .gap(sp.space_2)
                    // Selection background: opaque `accent_wash` rather than
                    // `accent.opacity(0.12)`, for the same reason as the
                    // version chip above.
                    .bg(if is_current {
                        colours.accent_wash
                    } else {
                        colours.bg_overlay
                    })
                    .hover(|s| s.bg(colours.bg_hover))
                    .cursor_pointer()
                    .when(is_current, |el| {
                        el.border_l(sp.border_width).border_color(colours.accent)
                    })
                    .child(
                        div()
                            .text_size(ts.dense.size)
                            .line_height(ts.dense.line_height)
                            .text_color(if is_current {
                                colours.fg_default
                            } else {
                                colours.fg_muted
                            })
                            .child(v_for_click.clone()),
                    )
                    .on_click(move |_, _window, cx| {
                        let v = v_for_click.clone();
                        let _ = this.update(cx, |panel, cx| {
                            panel.pick_version(row_ix, v, cx);
                        });
                    })
                    .into_any_element()
            })
            .collect();

        div()
            .id(("project.version.dropdown", row_ix))
            .debug_selector(move || format!("project.panel.row.{row_ix}.version_dropdown"))
            .w_full()
            // Exact height from row count — avoids the layout trap where
            // max_h without a definite height collapses to zero.
            .h(px(row_h * version_count as f32))
            .bg(colours.bg_overlay)
            .border(sp.border_width)
            .border_color(colours.border_default)
            .rounded(sp.r_sm)
            .overflow_hidden()
            .occlude()
            .children(items)
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GPUI trait implementations
// ─────────────────────────────────────────────────────────────────────────────

impl<P: PackageAccess> EventEmitter<PanelEvent> for ProjectPanel<P> {}

impl<P: PackageAccess> EventEmitter<PackageActivated> for ProjectPanel<P> {}

impl<P: PackageAccess> Focusable for ProjectPanel<P> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl<P: PackageAccess> Panel for ProjectPanel<P> {
    fn panel_name(&self) -> &'static str {
        // Must match the string registered in `shell.rs` (`register_panel`
        // call) and the `dump` below.
        "project-panel"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(SharedString::from("Project"))
    }

    // Apply the same themed title style as the Editor panel and all other
    // panels in the system. Without this override, the title renders with
    // GPUI's unthemed black default text, which is invisible on dark surfaces.
    // See `NudoxThemeExt::panel_title_style` for the rationale.
    fn title_style(&self, cx: &App) -> Option<gpui_component::dock::TitleStyle> {
        Some(cx.theme_ext().panel_title_style())
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: "project-panel".to_string(),
            children: Vec::new(),
            info: PanelInfo::Panel(JsonValue::Null),
        }
    }
}

impl<P: PackageAccess> Render for ProjectPanel<P> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _span = crate::perf::scope(crate::perf::Region::ProjectPanel);
        // Snapshot the precomputed rows (cheap: Arc-clones under the hood).
        let rows = self.rows.clone();
        let row_count = rows.len();
        let selection = self.selection;
        let version_picker_open = self.version_picker_open;
        let version_not_loaded_toast = self.version_not_loaded_toast.clone();

        // ── Empty state ───────────────────────────────────────────────────────
        // Read only the colours we need for the empty-state path so the
        // compiler does not warn about bindings that are unused when we
        // return early.
        if row_count == 0 {
            let bg_raised = cx.theme_ext().colours.bg_raised;
            return div()
                .id("project-panel")
                .track_focus(&self.focus)
                .key_context("ProjectPanel")
                .on_action(cx.listener(Self::on_sync_selected))
                .size_full()
                .flex()
                .flex_col()
                .bg(bg_raised)
                .child(self.render_empty(cx))
                .into_any_element();
        }

        // From here on, all tokens are needed.
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let al = ext.alpha;
        let theme = cx.theme();

        let this = cx.weak_entity();

        // ── Section header ────────────────────────────────────────────────────
        let count_label = self.row_count_label.clone();
        let header = h_flex()
            .w_full()
            .px(sp.space_2)
            .py(sp.space_1)
            .gap(sp.space_2)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .text_color(theme.muted_foreground)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                    .child(SharedString::from("PACKAGES")),
            )
            .child(
                div()
                    // `fg_faint`, not a fade of `muted_foreground`: this count
                    // sits a rank below the "PACKAGES" header text, which is
                    // exactly what the faint role names.
                    .text_color(colours.fg_faint)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .child(count_label),
            );

        // ── Version-switch error toast ────────────────────────────────────────
        // Inline, below the header — one line in danger colour.  Cleared when
        // the user opens a picker, selects a version, or closes the panel.
        let error_bar: Option<gpui::AnyElement> = version_not_loaded_toast.map(|msg| {
            h_flex()
                .id("project.version.error")
                .w_full()
                .px(sp.space_3)
                .py(sp.space_1)
                .bg(colours.danger.opacity(al.hairline))
                .border_b(sp.border_width)
                .border_color(colours.danger.opacity(al.tint))
                .child(
                    div()
                        .text_size(ts.dense.size)
                        .line_height(ts.dense.line_height)
                        .text_color(colours.danger)
                        .child(msg),
                )
                .into_any_element()
        });

        // ── Virtualized list ──────────────────────────────────────────────────
        //
        // `uniform_list` is required by LD-6 for any list that could exceed
        // 32 rows.  A real corpus can easily have hundreds of packages
        // (especially the fixture corpus).  The list is always virtualized.
        let this_for_list = this.clone();
        let list = uniform_list("project.packages", row_count, move |range, _window, cx| {
            range
                .map(|ix| {
                    let Some(row) = rows.get(ix) else {
                        return div().into_any_element();
                    };
                    let selected = selection == Some(ix);
                    let picker_open = version_picker_open == Some(ix);
                    let this = this_for_list.clone();
                    let this_row = this.clone();

                    let root = row.root.clone();

                    let row_el = Self::render_row(ix, row, selected, picker_open, this, cx)
                        .on_click(move |_event, _window, cx| {
                            let root = root.clone();
                            let _ = this_row.update(cx, |panel, cx| {
                                panel.apply_selection(Some(ix), cx);
                                if let Some(root) = root {
                                    cx.emit(PackageActivated { root });
                                }
                            });
                        });

                    // Stack: row + dropdown (when picker is open).
                    if picker_open {
                        let dropdown =
                            Self::render_version_dropdown(ix, row, this_for_list.clone(), cx);
                        v_flex()
                            .w_full()
                            .child(row_el)
                            .child(dropdown)
                            .into_any_element()
                    } else {
                        row_el.into_any_element()
                    }
                })
                .collect()
        })
        .h_full()
        .track_scroll(&self.scroll);

        div()
            .id("project-panel")
            .track_focus(&self.focus)
            // Both contexts must be set: "ProjectPanel" carries the `s` binding
            // declared in keymaps.rs; the panel also participates in the global
            // MoveSelectionUp/Down actions via the Overlay context when focused
            // from the dock.
            .key_context("ProjectPanel")
            .on_action(cx.listener(Self::on_sync_selected))
            .size_full()
            .flex()
            .flex_col()
            .bg(colours.bg_raised)
            .child(header)
            .when_some(error_bar, |el, bar| el.child(bar))
            .child(div().flex_1().overflow_hidden().child(list))
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Format the package count for the section header.
///
/// Lives here so `render` never calls `format!` (§1.1.4).
fn format_count(n: usize) -> SharedString {
    match n {
        0 => SharedString::from("0"),
        1 => SharedString::from("1"),
        n => SharedString::from(n.to_string()),
    }
}

/// A placeholder `PackageLineageId` used as the `VersionList::package` field
/// when constructing an empty `VersionList` for pending rows that do not yet
/// have an ecosystem/name pair.
///
/// The field is carried for display purposes by the dropdown; pending rows
/// have no dropdown, so the placeholder is never shown to the user.
fn default_lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("unknown"), PackageName::new("unknown"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::package::{PackageRow, PackageStatus};

    // ── PackageAccess double ─────────────────────────────────────────────────

    /// A minimal test double that satisfies `PackageAccess` without any engine
    /// or channel.  Tests construct it directly with a row vec.
    struct StubStore {
        rows: Vec<PackageRow>,
    }

    // `PackageAccess` requires `EventEmitter<PackagesChanged>` because the panel
    // subscribes to the store; the double must satisfy the same bound even
    // though these tests drive it directly rather than through an event.
    impl EventEmitter<PackagesChanged> for StubStore {}

    impl PackageAccess for StubStore {
        fn rows(&self) -> Vec<PackageRow> {
            self.rows.clone()
        }

        fn summary_label(&self) -> SharedString {
            SharedString::from(format!("{} packages", self.rows.len()))
        }

        fn versions(&self, package: &PackageLineageId) -> VersionList {
            VersionList {
                package: package.clone(),
                versions: std::sync::Arc::from(vec![]),
            }
        }

        fn select_version(
            &self,
            package: PackageLineageId,
            version: &str,
            generation: Gen,
        ) -> flume::Receiver<VersionEvent> {
            let (tx, rx) = flume::bounded(1);
            let version = nudox_engine::wire::SharedStr::from(version);
            let _ = tx.try_send(VersionEvent::NotLoaded {
                generation,
                package,
                version,
            });
            rx
        }
    }

    fn pending(name: &str) -> PackageRow {
        PackageRow {
            name: SharedString::from(name),
            status: PackageStatus::Pending,
            metadata: nudox_engine::PackageMetadata::default(),
            current_version: None,
            lineage: None,
            root: None,
        }
    }

    fn ready(name: &str, symbols: u64) -> PackageRow {
        PackageRow {
            name: SharedString::from(name),
            status: PackageStatus::Ready { symbols },
            metadata: nudox_engine::PackageMetadata::default(),
            current_version: None,
            lineage: None,
            root: None,
        }
    }

    fn failed(name: &str, msg: &str) -> PackageRow {
        PackageRow {
            name: SharedString::from(name),
            status: PackageStatus::Failed {
                message: SharedString::from(msg),
            },
            metadata: nudox_engine::PackageMetadata::default(),
            current_version: None,
            lineage: None,
            root: None,
        }
    }

    fn empty_versions() -> VersionList {
        VersionList {
            package: default_lineage(),
            versions: std::sync::Arc::from(vec![]),
        }
    }

    // ── PreparedPackageRow ───────────────────────────────────────────────────

    #[test]
    fn pending_row_carries_loading_label() {
        let row = PreparedPackageRow::from_row(&pending("axum"), empty_versions());
        let RowStatus::Pending { status_text } = &row.status else {
            panic!("expected Pending");
        };
        assert_eq!(row.name.as_ref(), "axum");
        // §1.1.4: the text is built at ingest, not in render.
        assert_eq!(status_text.as_ref(), "loading axum…");
    }

    #[test]
    fn one_symbol_uses_singular() {
        let row = PreparedPackageRow::from_row(&ready("axum", 1), empty_versions());
        let RowStatus::Ready { symbol_count_label } = &row.status else {
            panic!("expected Ready");
        };
        assert_eq!(symbol_count_label.as_ref(), "1 symbol");
    }

    /// F8: the status line carries the provenance the wire actually has — the
    /// ecosystem the package came from and how many of its generations are
    /// resident — and nothing it does not.
    #[test]
    fn ready_row_shows_the_provenance_the_wire_carries() {
        use nudox_engine::wire::VersionRow;

        let mut row = ready("memchr", 1347);
        row.lineage = Some(default_lineage());
        let versions = VersionList {
            package: default_lineage(),
            versions: std::sync::Arc::from(vec![
                VersionRow {
                    version: nudox_engine::wire::SharedStr::from("2.8.3"),
                    is_current: true,
                    symbol_count: 1347,
                },
                VersionRow {
                    version: nudox_engine::wire::SharedStr::from("2.8.0"),
                    is_current: false,
                    symbol_count: 1347,
                },
            ]),
        };
        let prepared = PreparedPackageRow::from_row(&row, versions);
        let RowStatus::Ready { symbol_count_label } = &prepared.status else {
            panic!("expected Ready");
        };
        assert!(
            symbol_count_label.contains("1347 symbols"),
            "the symbol count must survive: {symbol_count_label}"
        );
        assert!(
            symbol_count_label.contains("2 versions"),
            "a package with several generations resident must say so — that is \
             the lineage the corpus actually holds: {symbol_count_label}"
        );
    }

    /// The inverse: one generation is the ordinary case and must not grow a
    /// `1 versions` label that reads like a bug.
    #[test]
    fn single_generation_row_does_not_advertise_a_version_count() {
        use nudox_engine::wire::VersionRow;

        let mut row = ready("memchr", 1347);
        row.lineage = Some(default_lineage());
        let versions = VersionList {
            package: default_lineage(),
            versions: std::sync::Arc::from(vec![VersionRow {
                version: nudox_engine::wire::SharedStr::from("2.8.3"),
                is_current: true,
                symbol_count: 1347,
            }]),
        };
        let prepared = PreparedPackageRow::from_row(&row, versions);
        let RowStatus::Ready { symbol_count_label } = &prepared.status else {
            panic!("expected Ready");
        };
        assert!(
            !symbol_count_label.contains("version"),
            "one loaded generation is the ordinary case: {symbol_count_label}"
        );
    }

    #[test]
    fn prepared_row_surfaces_known_provenance_fields() {
        let mut row = ready("axum", 42);
        row.metadata = nudox_engine::PackageMetadata {
            description: Some("HTTP primitives".to_owned()),
            repository: Some("https://github.com/tokio-rs/axum".to_owned()),
            license: Some("MIT".to_owned()),
            owner: Some("tokio-rs".to_owned()),
            dependencies: vec!["hyper".to_owned(), "tower".to_owned()],
            release_date: Some("2026-08-01".to_owned()),
            homepage: Some("https://docs.rs/axum".to_owned()),
            coverage: Some(nudox_engine::PackageCoverage {
                documented: 8,
                total: 10,
            }),
            version: Some("0.8.4".to_owned()),
        };

        let prepared = PreparedPackageRow::from_row(&row, empty_versions());
        let label = prepared
            .provenance_label
            .expect("known metadata must be prepared for the panel");
        assert!(label.contains("HTTP primitives"));
        assert!(label.contains("https://github.com/tokio-rs/axum"));
        assert!(label.contains("license MIT"));
        assert!(label.contains("owner tokio-rs"));
        assert!(label.contains("2 dependencies"));
        assert!(label.contains("released 2026-08-01"));
        assert!(label.contains("homepage https://docs.rs/axum"));
        assert!(label.contains("docs 80%"));
    }

    #[test]
    fn many_symbols_use_plural() {
        let row = PreparedPackageRow::from_row(&ready("axum", 3060), empty_versions());
        let RowStatus::Ready { symbol_count_label } = &row.status else {
            panic!("expected Ready");
        };
        assert_eq!(symbol_count_label.as_ref(), "3060 symbols");
    }

    #[test]
    fn failed_row_carries_message_verbatim() {
        let row = PreparedPackageRow::from_row(&failed("axum", "oracle failed"), empty_versions());
        let RowStatus::Failed { message } = &row.status else {
            panic!("expected Failed");
        };
        assert_eq!(message.as_ref(), "oracle failed");
    }

    // ── format_count ─────────────────────────────────────────────────────────

    #[test]
    fn format_count_zero() {
        assert_eq!(format_count(0).as_ref(), "0");
    }

    #[test]
    fn format_count_one() {
        assert_eq!(format_count(1).as_ref(), "1");
    }

    #[test]
    fn format_count_many() {
        assert_eq!(format_count(42).as_ref(), "42");
    }

    // ── step_selection ───────────────────────────────────────────────────────

    #[test]
    fn down_from_none_selects_first() {
        assert_eq!(step_selection(None, true, 5), Some(0));
    }

    #[test]
    fn up_from_none_selects_last() {
        assert_eq!(step_selection(None, false, 5), Some(4));
    }

    #[test]
    fn down_advances_one_step() {
        assert_eq!(step_selection(Some(2), true, 5), Some(3));
    }

    #[test]
    fn up_retreats_one_step() {
        assert_eq!(step_selection(Some(2), false, 5), Some(1));
    }

    #[test]
    fn down_at_last_row_holds() {
        assert_eq!(step_selection(Some(4), true, 5), Some(4));
    }

    #[test]
    fn up_at_first_row_holds() {
        assert_eq!(step_selection(Some(0), false, 5), Some(0));
    }

    #[test]
    fn empty_list_returns_none() {
        assert_eq!(step_selection(None, true, 0), None);
        assert_eq!(step_selection(None, false, 0), None);
        assert_eq!(step_selection(Some(0), true, 0), None);
    }

    // ── refresh_rows via StubStore ────────────────────────────────────────────

    #[test]
    fn refresh_rows_drops_out_of_range_selection() {
        // Simulate: panel had 3 rows selected at index 2, then store shrinks to 1.
        let store = StubStore {
            rows: vec![ready("axum", 10)],
        };
        let raw = store.rows();
        let prepared: Vec<PreparedPackageRow> = raw
            .iter()
            .map(|r| PreparedPackageRow::from_row(r, empty_versions()))
            .collect();
        // Manual simulation of what refresh_rows does with selection clamping.
        let mut selection: Option<usize> = Some(2);
        if let Some(ix) = selection {
            if ix >= prepared.len() {
                selection = None;
            }
        }
        assert_eq!(selection, None, "out-of-range selection must be cleared");
    }

    #[test]
    fn refresh_rows_preserves_valid_selection() {
        let store = StubStore {
            rows: vec![ready("axum", 10), ready("tower", 5)],
        };
        let raw = store.rows();
        let prepared: Vec<PreparedPackageRow> = raw
            .iter()
            .map(|r| PreparedPackageRow::from_row(r, empty_versions()))
            .collect();
        let mut selection: Option<usize> = Some(1);
        if let Some(ix) = selection {
            if ix >= prepared.len() {
                selection = None;
            }
        }
        assert_eq!(selection, Some(1));
    }

    // ── Empty-state path ─────────────────────────────────────────────────────

    #[test]
    fn no_rows_means_empty_state_path() {
        // The view branches on `row_count == 0`.  This test verifies the
        // label-building side of that path, which is the only testable part
        // without a GPUI executor.
        let store = StubStore { rows: vec![] };
        let prepared: Vec<PreparedPackageRow> = store
            .rows()
            .iter()
            .map(|r| PreparedPackageRow::from_row(r, empty_versions()))
            .collect();
        assert!(prepared.is_empty());
        assert_eq!(format_count(prepared.len()).as_ref(), "0");
    }

    // ── Failure visibility test ───────────────────────────────────────────────

    #[test]
    fn failed_rows_are_not_hidden_in_a_mixed_list() {
        // In a list of three packages where one failed, the prepared rows
        // must include a RowStatus::Failed entry — not a Pending or Ready one.
        let store = StubStore {
            rows: vec![
                ready("axum", 3000),
                failed("tower", "oracle timed out"),
                pending("hyper"),
            ],
        };
        let prepared: Vec<PreparedPackageRow> = store
            .rows()
            .iter()
            .map(|r| PreparedPackageRow::from_row(r, empty_versions()))
            .collect();
        assert!(
            prepared
                .iter()
                .any(|r| matches!(r.status, RowStatus::Failed { .. })),
            "a failed package must appear as Failed in the prepared rows"
        );
    }

    // ── Version picker state ──────────────────────────────────────────────────

    /// A single loaded version: the chip is read-only (no picker affordance).
    #[test]
    fn single_version_chip_is_read_only() {
        use nudox_engine::wire::VersionRow;
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("axum"));
        let vlist = VersionList {
            package: lineage.clone(),
            versions: std::sync::Arc::from(vec![VersionRow {
                version: nudox_engine::wire::SharedStr::from("0.8.0"),
                is_current: true,
                symbol_count: 500,
            }]),
        };
        let row = PreparedPackageRow::from_row(&ready("axum", 500), vlist.clone());
        // Single version → has_choice == false → no picker affordance.
        assert_eq!(row.versions.len(), 1);
        assert!(row.current_version_label.is_some());
        // When there is only one entry the version chip must NOT toggle a picker.
        let has_choice = row.versions.len() > 1;
        assert!(!has_choice, "single-version chip must not open a picker");
    }

    /// Multiple loaded versions: `has_choice` is true, picker should open.
    #[test]
    fn multi_version_picker_is_shown() {
        use nudox_engine::wire::VersionRow;
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("axum"));
        let vlist = VersionList {
            package: lineage,
            versions: std::sync::Arc::from(vec![
                VersionRow {
                    version: nudox_engine::wire::SharedStr::from("0.8.0"),
                    is_current: true,
                    symbol_count: 500,
                },
                VersionRow {
                    version: nudox_engine::wire::SharedStr::from("0.7.9"),
                    is_current: false,
                    symbol_count: 490,
                },
            ]),
        };
        let row = PreparedPackageRow::from_row(&ready("axum", 500), vlist);
        assert_eq!(row.versions.len(), 2);
        let has_choice = row.versions.len() > 1;
        assert!(has_choice, "multi-version row must show picker");
        // The current version label must reflect the is_current row.
        assert_eq!(
            row.current_version_label.as_deref(),
            Some("v0.8.0"),
            "current version label must be pre-formatted at ingest time"
        );
    }

    /// `VersionEvent::NotLoaded` must surface as a visible error, not be
    /// swallowed.
    #[test]
    fn not_loaded_event_sets_toast() {
        // Simulate what `pick_version` does when the drain receives NotLoaded:
        // it sets `version_not_loaded_toast`.
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("axum"));
        let version = nudox_engine::wire::SharedStr::from("9.9.9");
        let generation = Gen(1);
        let event = VersionEvent::NotLoaded {
            generation,
            package: lineage,
            version: version.clone(),
        };
        // The drain closure for NotLoaded does:
        //   panel.version_not_loaded_toast = Some(SharedString::from(format!(...)))
        // We verify the formatting here without a full GPUI executor.
        let toast_msg = match &event {
            VersionEvent::NotLoaded { version, .. } => Some(SharedString::from(format!(
                "Version {} is not loaded",
                version
            ))),
            _ => None,
        };
        assert!(
            toast_msg.is_some(),
            "NotLoaded must produce a toast message"
        );
        assert_eq!(toast_msg.unwrap().as_ref(), "Version 9.9.9 is not loaded",);
    }
}
