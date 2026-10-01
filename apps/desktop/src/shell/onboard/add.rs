//! The add-a-folder dialog's content: a path field that answers as you type.
//!
//! Keyboard first. Type or paste a path; the folders it could continue to are
//! listed under the field (↑ ↓ walk them, Tab takes one, or goes as far as
//! every one of them agrees); a path that is a folder says what it is; ↵ adds
//! it. A refusal is said once, in place, under the field, and stays until the
//! next edit. Nothing is written to the shelf until ↵.
//!
//! The disk is asked off the UI thread, latest keystroke wins: a slow volume
//! delays the answer under the field, never a key.

use super::path::{self, Completion, Folder, Refusal};
use crate::core::LocalProjectId;
use crate::navigation::{Intent, OrbitRoute, Route};
use crate::shell::kit::text;
use crate::shell::region::Links;
use facet::controls::{KbdVoice, button, field, kbd};
use facet::icons::{Icon, IconSize, ui};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Space};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    Render, SharedString, Styled, Subscription, Task, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};

/// The rows the space under the field always holds: the verdict, the
/// suggestions, and the line that says how many more there were. The dialog
/// never changes height as the answers come and go.
const RESERVED_ROWS: f32 = 1.0 + 4.0 + 1.0;

/// What the disk said about the text last asked of it.
#[derive(Clone, Debug, Default)]
struct Probe {
    /// The text this answers.
    text: String,
    /// The folder it names, or why it names none.
    admitted: Option<Result<Folder, Refusal>>,
    /// The folders it could continue to.
    completion: Completion,
}

/// The dialog's content: the field, what the disk says under it, the buttons.
pub(crate) struct Form {
    links: Links,
    input: Entity<InputState>,
    /// The measure of the plate's inner width, handed in by the dialog each
    /// frame it draws this.
    measure: Option<Measure>,
    /// What the disk said about the latest text (an older answer is dropped).
    probe: Probe,
    /// Bumps with each edit: an answer for an older edit is not applied.
    edit: u64,
    /// The suggestion ↑ ↓ are on; `None` until they walk.
    walking: Option<usize>,
    /// The refusal said under the field, once, until the next edit.
    said: Option<Refusal>,
    asking: Option<Task<()>>,
    /// ↵ put a folder on the shelf (or went to one already there): the
    /// dialog closes onto a Library that changed under it.
    landed: bool,
    _subscriptions: Vec<Subscription>,
}

impl Form {
    pub(crate) fn new(links: Links, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Path to a project folder"));
        let events = cx.subscribe_in(&input, window, |add: &mut Self, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                let typed = input.read(cx).value().to_string();
                add.edited(typed, cx);
            }
            InputEvent::PressEnter { .. } => add.submit(window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            links,
            input,
            measure: None,
            probe: Probe::default(),
            edit: 0,
            walking: None,
            said: None,
            asking: None,
            landed: false,
            _subscriptions: vec![events],
        }
    }

    /// Opens fresh: an empty field, focused, nothing said.
    pub(crate) fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.probe = Probe::default();
        self.walking = None;
        self.said = None;
        self.asking = None;
        self.landed = false;
        self.edit = self.edit.wrapping_add(1);
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// The plate's measure for this frame (the dialog computes it).
    pub(crate) fn measured(&mut self, measure: &Measure) {
        self.measure = Some(*measure);
    }

    /// Whether ↵ landed a folder on the shelf since the dialog opened.
    pub(crate) const fn landed(&self) -> bool {
        self.landed
    }

    /// The field, for the dialog to focus.
    pub(crate) const fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// The text in the field.
    pub(crate) fn typed(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The field's edit: ask the disk, off this thread, about the new text.
    fn edited(&mut self, typed: String, cx: &mut Context<Self>) {
        if typed == self.probe.text && self.said.is_none() && self.asking.is_none() {
            return;
        }
        self.edit = self.edit.wrapping_add(1);
        self.walking = None;
        self.said = None;
        let edit = self.edit;
        let shelf = self.shelf(cx);
        let asked = typed.clone();
        self.asking = Some(cx.spawn(async move |add, cx| {
            let probe = cx
                .background_executor()
                .spawn(async move {
                    let home = path::home();
                    let admitted = (!asked.trim().is_empty()).then(|| path::admit(&asked, home.as_deref(), &shelf));
                    Probe { completion: path::complete(&asked, home.as_deref()), admitted, text: asked }
                })
                .await;
            let _ = add.update(cx, |add, cx| {
                if add.edit == edit {
                    add.probe = probe;
                    add.asking = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn shelf(&self, cx: &App) -> Vec<LocalProjectId> {
        self.links.snapshot(cx).workspace().projects.iter().map(|project| project.id.clone()).collect()
    }

    /// ↑ ↓: walks the suggestions.
    fn walk(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.probe.completion.shown.len();
        if count == 0 {
            return;
        }
        let next = match self.walking {
            None if delta > 0 => 0,
            None => count - 1,
            Some(at) => at.saturating_add_signed(delta).min(count - 1),
        };
        self.walking = Some(next);
        cx.notify();
    }

    /// Tab: takes the suggestion walked to, else goes as far as every
    /// suggestion agrees. Returns whether the text moved.
    fn complete(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let typed = self.typed(cx);
        let continued = match self.walking {
            Some(at) => self.probe.completion.take(&typed, at),
            None if self.probe.completion.shown.len() == 1 => self.probe.completion.take(&typed, 0),
            None => self.probe.completion.common(&typed),
        };
        let Some(continued) = continued else { return false };
        self.input.update(cx, |input, cx| input.set_value(continued.clone(), window, cx));
        self.edited(continued, cx);
        true
    }

    /// ↵: takes the suggestion walked to; otherwise adds the folder the text
    /// names (or says why it names none).
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.walking.is_some() && self.complete(window, cx) {
            return;
        }
        let typed = self.typed(cx);
        match path::admit(&typed, path::home().as_deref(), &self.shelf(cx)) {
            Ok(folder) if folder.on_shelf => {
                self.landed = true;
                self.links.dispatch(Intent::ActivateProject(folder.id), cx);
                self.links.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Home)), cx);
            }
            Ok(folder) => {
                self.landed = true;
                self.links.dispatch(Intent::AddProject { project: folder.id }, cx);
                // The Library is where a new project shows what it is doing;
                // arriving there also closes the dialog.
                self.links.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Home)), cx);
            }
            Err(refusal) => {
                self.said = Some(refusal);
                cx.notify();
            }
        }
    }

    /// The line that says what the text names, under the field.
    fn verdict(&self, measure: &Measure, palette: &Palette) -> Option<AnyElement> {
        let folder = self.probe.admitted.as_ref()?.as_ref().ok()?;
        let (words, ink) = match (folder.on_shelf, folder.project) {
            (true, _) => (format!("{} is on your shelf already. ↵ goes to it.", folder.name), palette.ink1),
            (false, Some((ecosystem, marker))) => (format!("{} · a {} project ({marker})", folder.name, ecosystem.name()), palette.ink0),
            (false, None) => (
                format!("{} · no project file here; Nudox reads the source it recognises", folder.name),
                palette.ink1,
            ),
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Roomy))
                .h(measure.row())
                .child(ui(Icon::Folder, IconSize::S14, palette.mint.base).size(measure.icon(14.0)))
                .child(text(ty::ROW, measure, ink).min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(words))
                .into_any_element(),
        )
    }

    /// The folders the text could continue to.
    fn suggestions(&self, measure: &Measure, palette: &Palette) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for (index, suggestion) in self.probe.completion.shown.iter().enumerate() {
            let on = self.walking == Some(index);
            let mut row = div()
                .id(("add-suggestion", index))
                .flex()
                .items_center()
                .gap(measure.space(Space::Roomy))
                .h(measure.row())
                .px(measure.space(Space::Base))
                .child(ui(Icon::Folder, IconSize::S14, palette.ink3).size(measure.icon(14.0)))
                .child(
                    text(ty::MONO_ROW, measure, if on { palette.ink0 } else { palette.ink1 })
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(suggestion.name.clone()),
                );
            if let Some(ecosystem) = suggestion.ecosystem {
                row = row.child(text(ty::SMALL, measure, palette.ink3).flex_none().child(ecosystem.name()));
            }
            if on {
                row = row.bg(palette.plate2);
            }
            rows.push(row.into_any_element());
        }
        if self.probe.completion.more > 0 {
            rows.push(
                div()
                    .h(measure.row())
                    .px(measure.space(Space::Base))
                    .flex()
                    .items_center()
                    .child(text(ty::SMALL, measure, palette.ink3).child(format!("and {} more: keep typing", self.probe.completion.more)))
                    .into_any_element(),
            );
        }
        rows
    }

    /// The keys, when there is nothing else to say.
    fn legend(measure: &Measure, palette: &Palette) -> AnyElement {
        let entry = |key: &'static str, does: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Snug))
                .child(kbd(key, measure).voice(KbdVoice::Quiet))
                .child(text(ty::SMALL, measure, palette.ink3).child(does))
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(measure.space(Space::Wide))
            .gap_y(measure.space(Space::Snug))
            .child(text(ty::SMALL, measure, palette.ink2).w_full().child("Type or paste the path of a folder."))
            .child(entry("↵", "add it"))
            .child(entry("Tab", "complete"))
            .child(entry("↑↓", "choose"))
            .child(entry("Esc", "close"))
            .into_any_element()
    }
}

impl Render for Form {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = self.measure.unwrap_or_else(|| facet.measure(px(440.0)));
        let typed_nothing = self.typed(cx).trim().is_empty();
        let mut below = div().flex().flex_col().gap(measure.space(Space::Tight)).min_h(measure.row() * RESERVED_ROWS);
        let verdict = self.verdict(&measure, palette);
        let suggestions = self.suggestions(&measure, palette);
        if typed_nothing {
            below = below.child(Self::legend(&measure, palette));
        } else {
            below = below.children(verdict).children(suggestions);
        }
        let chosen = cx.entity();
        let cancelled = self.links.clone();
        let pick = self.links.clone();
        let mut field = field("add-folder-path", &self.input, &measure).icon(Icon::Folder);
        if let Some(refusal) = &self.said {
            field = field.fault(SharedString::from(refusal.says()));
        }
        div()
            .id("add-folder")
            .flex()
            .flex_col()
            .gap(measure.space(Space::Roomy))
            .capture_key_down(cx.listener(|add, event: &KeyDownEvent, window, cx| {
                let plain = !event.keystroke.modifiers.platform && !event.keystroke.modifiers.control && !event.keystroke.modifiers.alt;
                match event.keystroke.key.as_str() {
                    "down" if plain => add.walk(1, cx),
                    "up" if plain => add.walk(-1, cx),
                    "tab" if plain && !event.keystroke.modifiers.shift => {
                        if !add.complete(window, cx) {
                            return;
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(field)
            .child(below)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(measure.space(Space::Roomy))
                    .child(
                        button("add-folder-choose", "Choose folder…", &measure)
                            .ghost()
                            .icon(Icon::Folder)
                            .on_click(move |_, cx| pick.dispatch(Intent::OpenFolderPicker, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(measure.space(Space::Base))
                            .child(button("add-folder-cancel", "Cancel", &measure).on_click(move |_, cx| cancelled.dispatch(Intent::DismissOverlay, cx)))
                            .child(
                                button("add-folder-add", "Add", &measure)
                                    .primary()
                                    .disabled(typed_nothing)
                                    .on_click(move |window, cx| chosen.update(cx, |add, cx| add.submit(window, cx))),
                            ),
                    ),
            )
    }
}
