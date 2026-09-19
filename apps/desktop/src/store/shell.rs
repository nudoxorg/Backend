//! Panels, focus, transient notices, and the persisted preferences.
//! Panel widths are springs; preferences are written atomically on change.
//! The animation loop exists only while something is moving, then stops.
//!
//! This is the only sink in the event graph. It listens to nothing and is
//! driven entirely by the reader: a key, a drag, a preference. That is what
//! makes the "idle window schedules no frames" property checkable — the single
//! source of continuous work in this application is here, it is a loop that
//! exits when every spring has settled, and a test can assert exactly that.

use super::events::ShellEvent;
use super::marks::{self, Marks, Recent};
use super::prefs::{self, EditorScheme, Preferences};
use crate::motion::spring::{Spring, Stiffness};
use crate::theme::palette::Appearance;
use crate::theme::tokens::{InterfaceSize, PanelWidth};
use gpui::AppContext as _;
use gpui::{Context, EventEmitter, Task};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Animation step, about one hundred and twenty frames a second.
const FRAME: Duration = Duration::from_millis(8);

/// How long a confirmation notice stays on screen.
const NOTICE_LIFETIME: Duration = Duration::from_millis(2200);

/// Which region owns the keyboard.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Focus {
    /// The omnibar in the titlebar.
    Omnibar,
    /// The library panel on the left.
    Library,
    /// The reader in the centre.
    #[default]
    Reader,
}

/// Which side a panel is on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Side {
    /// The library panel.
    Library,
    /// The context panel.
    Context,
}

/// A short confirmation. Failures are never notices; they are faults in place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Notice {
    text: String,
    detail: Option<String>,
}

impl Notice {
    /// Returns the notice text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the exact value the notice is about, when it has one.
    pub(crate) fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// One page of the settings sheet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SettingsPage {
    /// Palette, size, motion.
    #[default]
    Appearance,
    /// Where source opens.
    Editor,
    /// Connecting an agent over MCP.
    Agents,
    /// Capabilities, lanes, endpoint, revision.
    Diagnostics,
    /// Every mark and what it means.
    Legend,
}

impl SettingsPage {
    /// Every page, in sidebar order.
    pub(crate) const ALL: [Self; 5] = [
        Self::Appearance,
        Self::Editor,
        Self::Agents,
        Self::Diagnostics,
        Self::Legend,
    ];

    /// Returns the sidebar label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::Agents => "Agents",
            Self::Diagnostics => "Diagnostics",
            Self::Legend => "Legend",
        }
    }
}

/// Panels, focus, notices, and preferences.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one window: is anything moving, is the settings sheet up, is the window too narrow, is there a page for the context panel"
)]
pub(crate) struct ShellStore {
    data: PathBuf,
    prefs: Preferences,
    marks: Marks,
    library: Spring,
    context: Spring,
    focus: Focus,
    settings: bool,
    settings_page: SettingsPage,
    notice: Option<Notice>,
    notice_task: Option<Task<()>>,
    animation: Option<Task<()>>,
    animating: bool,
    narrow: bool,
    context_only: bool,
    context_available: bool,
}

impl EventEmitter<ShellEvent> for ShellStore {}

impl ShellStore {
    /// Restores the shell from the preferences beside a workspace.
    pub(crate) fn new(data: PathBuf, prefs: Preferences) -> Self {
        Self {
            marks: marks::load(&data),
            library: Spring::at(
                open_width(prefs.library_open(), prefs.library_width()),
                Stiffness::PANEL,
            ),
            context: Spring::at(0.0, Stiffness::PANEL),
            focus: Focus::Reader,
            settings: false,
            settings_page: SettingsPage::default(),
            notice: None,
            notice_task: None,
            animation: None,
            animating: false,
            narrow: false,
            context_only: false,
            context_available: false,
            data,
            prefs,
        }
    }

    /// Returns the durable workspace directory preferences live beside.
    pub(crate) fn data(&self) -> &std::path::Path {
        &self.data
    }

    /// Returns the settings page being shown.
    pub(crate) const fn settings_page(&self) -> SettingsPage {
        self.settings_page
    }

    /// Shows one settings page.
    pub(crate) fn show_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.settings_page = page;
        self.settings = true;
        cx.notify();
    }

    /// Records whether the open page has anything for the context panel.
    ///
    /// The panel is not a fixture. With nothing open it has nothing to say,
    /// so it is not shown at all — not as a rail, not as an empty column.
    pub(crate) fn set_context_available(&mut self, available: bool, cx: &mut Context<Self>) {
        if self.context_available == available {
            return;
        }
        self.context_available = available;
        self.retarget(cx);
    }

    /// Returns the persisted preferences.
    pub(crate) const fn prefs(&self) -> Preferences {
        self.prefs
    }

    /// Returns the reader's pins and recents.
    pub(crate) const fn marks(&self) -> &Marks {
        &self.marks
    }

    /// Pins or unpins one coordinate and persists it; returns whether it is now pinned.
    pub(crate) fn toggle_pin(&mut self, coordinate: &str, cx: &mut Context<Self>) -> bool {
        let pinned = self.marks.toggle_pin(coordinate);
        self.persist_marks(cx);
        cx.emit(ShellEvent::Preferences);
        cx.notify();
        pinned
    }

    /// Records one subject as the most recently opened and persists it.
    pub(crate) fn remember(&mut self, entry: Recent, cx: &mut Context<Self>) {
        self.marks.remember(entry);
        self.persist_marks(cx);
        cx.notify();
    }

    /// Forgets a project that left the shelf: its pin and its recent pages.
    pub(crate) fn forget_project(&mut self, root: &str, cx: &mut Context<Self>) {
        self.marks.unpin(root);
        self.marks.forget_project(root);
        self.persist_marks(cx);
        cx.notify();
    }

    fn persist_marks(&self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        let marks = self.marks.clone();
        cx.background_spawn(async move {
            let _ = marks::save(&data, &marks);
        })
        .detach();
    }

    /// Returns the library panel's current width in pixels.
    pub(crate) fn library_width(&self) -> f32 {
        self.library.value()
    }

    /// Returns the context panel's current width in pixels.
    pub(crate) fn context_width(&self) -> f32 {
        self.context.value()
    }

    /// Returns whether the library panel is meant to be open.
    pub(crate) const fn library_open(&self) -> bool {
        self.prefs.library_open()
    }


    /// Returns which region owns the keyboard.
    pub(crate) const fn focus(&self) -> Focus {
        self.focus
    }

    /// Returns whether the settings sheet is showing.
    pub(crate) const fn settings_open(&self) -> bool {
        self.settings
    }

    /// Returns the current confirmation notice.
    pub(crate) const fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Returns whether motion is suppressed.
    pub(crate) const fn reduced_motion(&self) -> bool {
        self.prefs.reduced_motion()
    }

    /// Returns the external editor scheme.
    pub(crate) const fn editor(&self) -> EditorScheme {
        self.prefs.editor()
    }

    /// Moves keyboard ownership.
    pub(crate) fn focus_on(&mut self, focus: Focus, cx: &mut Context<Self>) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        cx.notify();
    }

    /// Opens or closes the settings sheet.
    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = !self.settings;
        cx.notify();
    }

    /// Opens or closes one panel and persists the choice.
    pub(crate) fn toggle_panel(&mut self, side: Side, cx: &mut Context<Self>) {
        let (library, context) = match side {
            Side::Library => (!self.prefs.library_open(), self.prefs.context_open()),
            Side::Context => (self.prefs.library_open(), !self.prefs.context_open()),
        };
        self.prefs = self.prefs.with_open(library, context);
        self.retarget(cx);
        self.persist(cx);
        cx.emit(ShellEvent::Layout);
    }

    /// Collapses panels that a narrow window cannot hold.
    ///
    /// This runs on every frame, so it must be a no-op on every frame where
    /// the window has not actually crossed a threshold. Both thresholds have
    /// to be remembered for that to hold: comparing only the narrower one made
    /// every frame of a window between the two widths retarget the springs and
    /// restart the animation loop, which notified, which drew, which ran this
    /// again — a window that never stopped drawing for as long as it sat at a
    /// perfectly ordinary size.
    pub(crate) fn fit_to(&mut self, width: f32, cx: &mut Context<Self>) {
        let narrow = width < PanelWidth::AUTO_COLLAPSE_WINDOW;
        let context_only = width < PanelWidth::AUTO_COLLAPSE_CONTEXT;
        if (narrow, context_only) == (self.narrow, self.context_only) {
            return;
        }
        self.narrow = narrow;
        self.context_only = context_only;
        self.retarget(cx);
    }

    /// Switches appearance and persists it.
    pub(crate) fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.prefs = self.prefs.with_appearance(appearance);
        self.persist(cx);
        cx.emit(ShellEvent::Preferences);
        cx.notify();
    }

    /// Changes the reading size and persists it.
    pub(crate) fn set_interface(&mut self, interface: InterfaceSize, cx: &mut Context<Self>) {
        self.prefs = self.prefs.with_interface(interface);
        self.persist(cx);
        cx.emit(ShellEvent::Preferences);
        cx.notify();
    }

    /// Suppresses or restores motion and persists the choice.
    pub(crate) fn set_reduced_motion(&mut self, reduced: bool, cx: &mut Context<Self>) {
        self.prefs = self.prefs.with_reduced_motion(reduced);
        if reduced {
            self.library.snap(self.library.target());
            self.context.snap(self.context.target());
            self.animating = false;
        }
        self.persist(cx);
        cx.emit(ShellEvent::Preferences);
        cx.notify();
    }

    /// Chooses the external editor and persists it.
    pub(crate) fn set_editor(&mut self, editor: EditorScheme, cx: &mut Context<Self>) {
        self.prefs = self.prefs.with_editor(editor);
        self.persist(cx);
        cx.emit(ShellEvent::Preferences);
        cx.notify();
    }

    /// Raises a short statement of something that just happened.
    pub(crate) fn notify(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notify_copied(text, None, cx);
    }

    /// Raises a short confirmation.
    pub(crate) fn notify_copied(
        &mut self,
        text: impl Into<String>,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.notice = Some(Notice {
            text: text.into(),
            detail,
        });
        cx.emit(ShellEvent::Notice);
        cx.notify();
        self.notice_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_LIFETIME).await;
            let _ = this.update(cx, |this, cx| {
                this.notice = None;
                cx.emit(ShellEvent::Notice);
                cx.notify();
            });
        }));
    }

    fn retarget(&mut self, cx: &mut Context<Self>) {
        self.library.retarget(open_width(
            self.prefs.library_open() && !self.narrow,
            self.prefs.library_width(),
        ));
        let context_open =
            self.prefs.context_open() && !self.narrow && !self.context_only;
        self.context.retarget(if self.context_available {
            open_width(context_open, self.prefs.context_width())
        } else {
            0.0
        });
        self.start(cx);
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.prefs.reduced_motion() {
            self.library.snap(self.library.target());
            self.context.snap(self.context.target());
            self.animating = false;
            cx.notify();
            return;
        }
        if self.animating {
            return;
        }
        self.animating = true;
        cx.notify();
        self.animation = Some(cx.spawn(async move |this, cx| {
            let mut last = Instant::now();
            loop {
                cx.background_executor().timer(FRAME).await;
                let now = Instant::now();
                let delta = now.saturating_duration_since(last);
                last = now;
                let moving = this.update(cx, |this, cx| this.advance(delta, cx));
                if !matches!(moving, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn advance(&mut self, delta: Duration, cx: &mut Context<Self>) -> bool {
        let library = self.library.advance(delta);
        let context = self.context.advance(delta);
        cx.notify();
        self.animating = library || context;
        self.animating
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        let prefs = self.prefs;
        cx.background_spawn(async move {
            let _ = prefs::save(&data, prefs);
        })
        .detach();
    }
}

fn open_width(open: bool, width: f32) -> f32 {
    if open { width } else { PanelWidth::RAIL }
}
