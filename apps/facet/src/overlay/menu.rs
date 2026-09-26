//! Menus: context and dropdown, on the float layer ([`FloatKind::Menu`]).
//!
//! A menu opens from a click or a key ([`open`]), so it takes focus and
//! keeps it until it closes; the dismissal triad (outside press, Esc, a
//! choice) is the layer's. Inside: ↑ ↓ walk (wrapping, skipping disabled
//! rows), Home / End jump, typing jumps to the first row whose label starts
//! with what was typed (the query resets after a pause), ↵ chooses. The
//! selection is one plate that glides between rows on a spring.
//!
//! Calm: rows are an icon and a label; shortcuts are quiet text, and key caps
//! only while ⌘ is held.

use super::float::{self, FloatKind, FloatRequest, Side};
use crate::controls::{KbdVoice, keys};
use crate::icons::{self, Icon, IconSize};
use crate::measure::{Measure, Set};
use crate::motion::{self, Motion, spec};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole};
use gpui::{
    AnyElement, App, Bounds, ElementId, InteractiveElement, IntoElement, Keystroke, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// One row.
#[derive(Clone, Debug, Default)]
pub struct MenuItem {
    /// A leading icon.
    pub icon: Option<Icon>,
    /// The label.
    pub label: SharedString,
    /// A shortcut (`⌘`, `K`).
    pub chord: Vec<SharedString>,
    /// A destructive action (coral).
    pub danger: bool,
    /// Shown, not choosable.
    pub disabled: bool,
    /// A hairline after this row.
    pub separator_after: bool,
}

impl MenuItem {
    /// A row reading `label`.
    #[must_use]
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }

    /// With a leading icon.
    #[must_use]
    pub const fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// With a shortcut.
    #[must_use]
    pub fn chord(mut self, chord: &[&str]) -> Self {
        self.chord = chord.iter().map(|key| SharedString::from((*key).to_owned())).collect();
        self
    }

    /// Destructive.
    #[must_use]
    pub const fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Not choosable.
    #[must_use]
    pub const fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    /// A hairline after it.
    #[must_use]
    pub const fn separated(mut self) -> Self {
        self.separator_after = true;
        self
    }
}

type Choose = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A menu: rows and what choosing one does.
#[derive(Clone)]
pub struct Menu {
    /// The rows.
    pub items: Vec<MenuItem>,
    /// Called with the chosen row's index (the menu closes itself).
    pub on_choose: Choose,
}

impl Menu {
    /// A menu of `items` calling `on_choose(index)`.
    pub fn new(items: Vec<MenuItem>, on_choose: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        Self {
            items,
            on_choose: Rc::new(on_choose),
        }
    }
}

// ------------------------------------------------------------------ keys

/// The next choosable row from `active` by `delta` (wrapping), skipping
/// disabled rows; `None` when nothing is choosable.
#[must_use]
pub fn step(active: Option<usize>, disabled: &[bool], delta: isize) -> Option<usize> {
    let count = disabled.len();
    if count == 0 || disabled.iter().all(|off| *off) {
        return None;
    }
    let count_i = isize::try_from(count).unwrap_or(isize::MAX);
    let mut at = match active {
        Some(active) => isize::try_from(active).unwrap_or(0),
        None if delta >= 0 => -1,
        None => count_i,
    };
    for _ in 0..count {
        at = (at + delta.signum()).rem_euclid(count_i);
        let index = usize::try_from(at).unwrap_or(0);
        if !disabled[index] {
            return Some(index);
        }
    }
    None
}

/// The first choosable row at or after `from` (wrapping) whose label starts
/// with `query`, ignoring case.
#[must_use]
pub fn type_ahead(labels: &[&str], disabled: &[bool], query: &str, from: usize) -> Option<usize> {
    if query.is_empty() || labels.is_empty() {
        return None;
    }
    let query = query.to_lowercase();
    (0..labels.len())
        .map(|offset| (from + offset) % labels.len())
        .find(|index| !disabled[*index] && labels[*index].to_lowercase().starts_with(&query))
}

/// The type-ahead query resets after this pause.
pub const TYPE_AHEAD_RESET: Duration = Duration::from_millis(700);

#[derive(Default)]
struct State {
    active: Option<usize>,
    query: String,
    typed_at: Option<Instant>,
}

// ------------------------------------------------------------------ render

const LABEL: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 13.0,
    line: 18.0,
    tracking: 0.0,
    italic: false,
};

const CHORD: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 11.0,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};

fn row_height(measure: &Measure) -> Pixels {
    px(28.0 * measure.scale() * measure.density().row())
}

fn separator_height(measure: &Measure) -> Pixels {
    px(9.0 * measure.scale())
}

const PAD: f32 = 4.0;

/// The top of row `index` inside the menu's padding box.
fn row_top(items: &[MenuItem], index: usize, measure: &Measure) -> Pixels {
    let mut top = px(PAD * measure.scale());
    for item in &items[..index] {
        top += row_height(measure);
        if item.separator_after {
            top += separator_height(measure);
        }
    }
    top
}

/// The content builder for the float layer.
fn content(key: ElementId, menu: Menu) -> impl Fn(&Measure, &mut Window, &mut App) -> AnyElement + 'static {
    let state = Rc::new(RefCell::new(State::default()));
    let motion = Motion::new();
    move |measure: &Measure, window: &mut Window, cx: &mut App| {
        let palette = cx.facet().palette();
        let disabled: Vec<bool> = menu.items.iter().map(|item| item.disabled).collect();
        {
            let mut state = state.borrow_mut();
            if state.active.is_none() {
                state.active = step(None, &disabled, 1);
            }
        }
        // Keys, before the layer's own (Esc still steps back).
        {
            let state = state.clone();
            let menu = menu.clone();
            let key = key.clone();
            let disabled = disabled.clone();
            float::on_key(
                move |keystroke: &Keystroke, window, cx| {
                    handle(&state, &menu, &key, &disabled, keystroke, window, cx)
                },
                window,
                cx,
            );
        }
        let active = state.borrow().active;
        let width = px(220.0 * measure.scale()).max(measure.width().min(px(248.0 * measure.scale())));
        let target = active.map_or(0.0, |index| f32::from(row_top(&menu.items, index, measure)));
        let plate_y = motion.animate("menu-plate", target, spec::FOLLOW, window, cx);
        let shown = motion.animate("menu-plate-shown", if active.is_some() { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let keys_held = measure.reveal().keys;
        let mut column = div()
            .relative()
            .w(width)
            .flex()
            .flex_col()
            .py(px(PAD * measure.scale()));
        // The selection plate: one element, gliding.
        column = column.child(
            div()
                .absolute()
                .left(px(PAD * measure.scale()))
                .right(px(PAD * measure.scale()))
                .top(px(plate_y))
                .h(row_height(measure))
                .bg(palette.peri.soft.hsla())
                .border_l_2()
                .border_color(palette.peri.base.hsla())
                .opacity(shown),
        );
        for (index, item) in menu.items.iter().enumerate() {
            let ink = if item.disabled {
                palette.ink4.hsla()
            } else if item.danger {
                palette.coral.base.hsla()
            } else if active == Some(index) {
                palette.ink0.hsla()
            } else {
                palette.ink1.hsla()
            };
            let mut row = div()
                .id(("menu-row", index))
                .relative()
                .h(row_height(measure))
                .mx(px(PAD * measure.scale()))
                .px(px(10.0 * measure.scale()))
                .flex()
                .items_center()
                .gap(px(10.0 * measure.scale()));
            if let Some(icon) = item.icon {
                row = row.child(icons::ui(icon, IconSize::S14, ink).size(measure.icon(14.0)));
            }
            row = row.child(div().flex_1().set(LABEL, measure).text_color(ink).child(item.label.clone()));
            if !item.chord.is_empty() {
                let chord: Vec<&str> = item.chord.iter().map(SharedString::as_ref).collect();
                row = row.child(if keys_held {
                    keys(&chord, KbdVoice::Plain, measure).into_any_element()
                } else {
                    div()
                        .set(CHORD, measure)
                        .text_color(palette.ink4.hsla())
                        .child(chord.concat())
                        .into_any_element()
                });
            }
            if !item.disabled {
                let hover_state = state.clone();
                let choose_menu = menu.clone();
                let choose_key = key.clone();
                row = row
                    .on_mouse_move(move |_, window, _cx| {
                        let mut state = hover_state.borrow_mut();
                        if state.active != Some(index) {
                            state.active = Some(index);
                            drop(state);
                            float::refresh(window, _cx);
                        }
                    })
                    .on_click(move |_, window, cx| {
                        choose(&choose_menu, &choose_key, index, window, cx);
                    });
            }
            column = column.child(row);
            if item.separator_after {
                column = column.child(
                    div()
                        .h(separator_height(measure))
                        .flex()
                        .items_center()
                        .child(div().h(px(1.0)).w_full().bg(palette.line1.hsla())),
                );
            }
        }
        column.into_any_element()
    }
}

fn choose(menu: &Menu, key: &ElementId, index: usize, window: &mut Window, cx: &mut App) {
    if menu.items.get(index).is_none_or(|item| item.disabled) {
        return;
    }
    float::close(key, window, cx);
    (menu.on_choose)(index, window, cx);
}

fn handle(
    state: &Rc<RefCell<State>>,
    menu: &Menu,
    key: &ElementId,
    disabled: &[bool],
    keystroke: &Keystroke,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let mods = keystroke.modifiers;
    if mods.platform || mods.control || mods.alt {
        return false;
    }
    let active = state.borrow().active;
    let next = match keystroke.key.as_str() {
        "down" => step(active, disabled, 1),
        "up" => step(active, disabled, -1),
        "home" => step(None, disabled, 1),
        "end" => step(None, disabled, -1),
        "enter" => {
            if let Some(index) = active {
                choose(menu, key, index, window, cx);
            }
            return true;
        }
        _ => {
            let Some(ch) = keystroke.key_char.as_deref().filter(|text| {
                text.chars().count() == 1 && text.chars().all(|ch| ch.is_alphanumeric())
            }) else {
                return false;
            };
            let now = motion::now(cx);
            let mut state = state.borrow_mut();
            if state
                .typed_at
                .is_none_or(|at| now.saturating_duration_since(at) > TYPE_AHEAD_RESET)
            {
                state.query.clear();
            }
            state.query.push_str(ch);
            state.typed_at = Some(now);
            let labels: Vec<&str> = menu.items.iter().map(|item| item.label.as_ref()).collect();
            let from = if state.query.chars().count() == 1 {
                active.map_or(0, |active| active + 1)
            } else {
                active.unwrap_or(0)
            };
            type_ahead(&labels, disabled, &state.query, from).or(state.active)
        }
    };
    state.borrow_mut().active = next;
    float::refresh(window, cx);
    true
}

/// Opens `menu` below (or beside) `anchor` for the trigger `key`: a click or
/// a key; focus moves into the menu and returns when it closes.
pub fn open(key: impl Into<ElementId>, anchor: Bounds<Pixels>, side: Side, menu: Menu, window: &mut Window, cx: &mut App) {
    let key = key.into();
    let request = FloatRequest::new(key.clone(), anchor, FloatKind::Menu, content(key, menu)).side(side);
    float::open(request, window, cx);
}

/// A context menu at the pointer.
pub fn context(key: impl Into<ElementId>, at: gpui::Point<Pixels>, menu: Menu, window: &mut Window, cx: &mut App) {
    let anchor = Bounds::new(at, gpui::size(px(1.0), px(1.0)));
    open(key, anchor, Side::Below, menu, window, cx);
}

#[cfg(test)]
mod tests {
    use super::{step, type_ahead};

    #[test]
    fn stepping_wraps_and_skips_disabled_rows() {
        let disabled = [false, true, false, false];
        assert_eq!(step(None, &disabled, 1), Some(0));
        assert_eq!(step(Some(0), &disabled, 1), Some(2), "skips the disabled row");
        assert_eq!(step(Some(3), &disabled, 1), Some(0), "wraps");
        assert_eq!(step(Some(0), &disabled, -1), Some(3), "wraps back");
        assert_eq!(step(None, &disabled, -1), Some(3));
        assert_eq!(step(Some(2), &[true, true, true], 1), None);
        assert_eq!(step(None, &[], 1), None);
    }

    #[test]
    fn type_ahead_finds_the_next_match_from_the_active_row() {
        let labels = ["Copy", "Copy path", "Cut", "Delete", "Duplicate"];
        let disabled = [false, false, true, false, false];
        assert_eq!(type_ahead(&labels, &disabled, "c", 0), Some(0));
        assert_eq!(type_ahead(&labels, &disabled, "c", 1), Some(1));
        assert_eq!(type_ahead(&labels, &disabled, "cu", 0), None, "Cut is disabled");
        assert_eq!(type_ahead(&labels, &disabled, "D", 4), Some(4), "case-insensitive, from the active row");
        assert_eq!(type_ahead(&labels, &disabled, "d", 0), Some(3));
        assert_eq!(type_ahead(&labels, &disabled, "x", 0), None);
    }
}
