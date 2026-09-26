//! The text field and the select trigger.
//!
//! A field is a cut plate with a caret in it. The editing engine is
//! `gpui_component`'s (`InputState`: IME composition, selection, undo, the
//! clipboard); its own chrome is switched off and FACET paints the frame:
//! focus turns the bevel into the doubled periwinkle, a field that refuses
//! its input turns it coral and says why once, in place; nothing else moves.
//! The caret is mint, the selection periwinkle, the placeholder `ink3`.
//!
//! The select trigger is the same plate holding a mark, the chosen value and
//! a chevron that turns over while its menu is open. Given its choices
//! ([`Select::options`], [`Select::menu`]) it opens them itself as an
//! `overlay::menu` below the plate (a press, ↵, Space or ↓); the float layer
//! (W-Float) owns the menu, its keys and its dismissal.

use super::button::{Handler, sunk, wire};
use super::state::{Look, Touch, hover_zone, track};
use super::{muted, with_alpha};
use crate::Set;
use crate::icons::{self, Icon, IconSize, Lang, ui};
use crate::measure::{Control, Measure, Space};
use crate::motion::spec;
use crate::overlay::Side;
use crate::overlay::menu::{self, Menu, MenuItem};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::ty;
use gpui::{
    App, ElementId, Entity, Focusable, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, RenderOnce, SharedString, Styled, Transformation, Window, div, px,
    radians,
};
use gpui_component::input::{Input, InputState};
use std::rc::Rc;
use std::sync::Arc;

/// Points the text engine's colours at the active palette: the caret mint,
/// the selection periwinkle, text `ink0`, the placeholder `ink3`. Writes only
/// when something differs, so it settles after one frame.
pub fn sync_text_engine(cx: &mut App) {
    let palette = cx.palette();
    let foreground: Hsla = palette.ink0.into();
    let muted_foreground: Hsla = palette.ink3.into();
    let caret: Hsla = palette.mint.base.into();
    let selection: Hsla = with_alpha(palette.peri.base.into(), 0.32);
    let theme = gpui_component::Theme::global(cx);
    if theme.foreground == foreground
        && theme.muted_foreground == muted_foreground
        && theme.caret == caret
        && theme.selection == selection
    {
        return;
    }
    let theme = gpui_component::Theme::global_mut(cx);
    theme.foreground = foreground;
    theme.muted_foreground = muted_foreground;
    theme.caret = caret;
    theme.selection = selection;
    gpui_component::Theme::sync_base(cx);
}

/// A text field over a caller-owned `InputState`.
#[derive(IntoElement)]
pub struct Field {
    id: ElementId,
    state: Entity<InputState>,
    icon: Option<Icon>,
    fault: Option<SharedString>,
    disabled: bool,
    quiet: bool,
    look: Look,
    measure: Measure,
}

/// A field editing `state`, sized for `measure` (it fills its width).
#[must_use]
pub fn field(id: impl Into<ElementId>, state: &Entity<InputState>, measure: &Measure) -> Field {
    Field {
        id: id.into(),
        state: state.clone(),
        icon: None,
        fault: None,
        disabled: false,
        quiet: false,
        look: Look::LIVE,
        measure: *measure,
    }
}

impl Field {
    /// The quiet field (the shelf's filter): shorter, smaller type, a
    /// hairline instead of a bevel until it has focus.
    #[must_use]
    pub const fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// A leading icon (`ink3`; an alert while at fault).
    #[must_use]
    pub const fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The field refuses its input: a coral bevel and this reason, said
    /// once under the field.
    #[must_use]
    pub fn fault(mut self, reason: impl Into<SharedString>) -> Self {
        self.fault = Some(reason.into());
        self
    }

    /// Disabled: .42 opacity, not editable.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Pins an appearance on top of the live state.
    #[must_use]
    pub const fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }
}

impl RenderOnce for Field {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        sync_text_engine(cx);
        let palette = cx.palette();
        let measure = self.measure;
        let active = !self.disabled;
        let touch = Touch::read(&self.id, self.look, active, window, cx);
        let motion = touch.motion.clone();
        let id = self.id.clone();
        // A field shows focus however it arrived: the caret is in it.
        let focused = active
            && (self.look.focus || self.state.read(cx).focus_handle(cx).is_focused(window));
        let height = if self.quiet {
            measure.control(Control::Small) * (28.0 / 24.0)
        } else {
            measure.control(Control::Large)
        };
        let s = f32::from(measure.control(Control::Large)) / 38.0;
        let chamfer = if self.quiet { 6.0 * s } else { 9.0 * s };

        let hover = motion.animate(
            track(&id, "hover"),
            if touch.hovered && !focused { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let focus = motion.animate(track(&id, "focus"), if focused { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let fault = motion.animate(
            track(&id, "fault"),
            if self.fault.is_some() { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let rest = if self.quiet {
            let line: Hsla = palette.line1.into();
            Edge {
                hi: line,
                lo: line,
                ..Edge::of(Bevel::Rest, palette)
            }
        } else {
            Edge::of(Bevel::Rest, palette)
        };
        let edge = rest
            .mix(Edge::of(Bevel::Focus, palette), focus)
            .mix(Edge::of(Bevel::Coral, palette), fault);
        let fill = if self.quiet {
            let tint: Hsla = palette.tint.into();
            mix(with_alpha(tint, 0.3 * tint.alpha), tint, hover)
        } else {
            mix(palette.plate.into(), palette.plate2.into(), hover)
        };

        let icon_ink = mix(palette.ink3.into(), palette.coral.base.into(), fault);
        let icon = if self.fault.is_some() {
            Some(Icon::Alert)
        } else {
            self.icon
        };
        let input = Input::new(&self.state)
            .appearance(false)
            .bordered(false)
            .focus_bordered(false)
            .disabled(self.disabled)
            .set(if self.quiet { ty::SMALL } else { ty::ROW }, &measure)
            .px(px(0.0))
            .flex_1()
            .min_w(px(0.0))
            .h(height);
        let plate = cut()
            .chamfer(Chamfer::Px(chamfer))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(fill)
            .flex()
            .items_center()
            .gap(measure.space(Space::Roomy) * 0.8)
            .pl(measure.space(Space::Roomy) + px(1.0))
            .pr(measure.space(Space::Base))
            .h(height)
            .w_full()
            .children(icon.map(|icon| ui(icon, IconSize::S14, icon_ink).size(measure.icon(14.0))))
            .child(input)
            .id(id)
            .opacity(if self.disabled { 0.42 } else { 1.0 });
        let column = div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Snug))
            .w_full()
            .child(hover_zone(plate, &touch, chamfer, active))
            .children(self.fault.map(|reason| {
                div()
                    .set(ty::SMALL, &measure)
                    .text_color(with_alpha(palette.coral.base.into(), fault))
                    .child(reason)
            }));
        column
    }
}

/// The select trigger: a mark, the value, a chevron that turns over when
/// open.
#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    value: SharedString,
    lang: Option<Lang>,
    icon: Option<Icon>,
    open: bool,
    disabled: bool,
    look: Look,
    measure: Measure,
    on_open: Option<Handler>,
    menu: Option<Menu>,
}

/// A select trigger reading `value`, sized for `measure`.
#[must_use]
pub fn select(id: impl Into<ElementId>, value: impl Into<SharedString>, measure: &Measure) -> Select {
    Select {
        id: id.into(),
        value: value.into(),
        lang: None,
        icon: None,
        open: false,
        disabled: false,
        look: Look::LIVE,
        measure: *measure,
        on_open: None,
        menu: None,
    }
}

impl Select {
    /// A language mark before the value.
    #[must_use]
    pub const fn lang(mut self, lang: Lang) -> Self {
        self.lang = Some(lang);
        self
    }

    /// An icon before the value.
    #[must_use]
    pub const fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Its menu is open (the bevel turns periwinkle, the chevron over).
    #[must_use]
    pub const fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Disabled: .42 opacity, deaf.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Pins an appearance on top of the live state.
    #[must_use]
    pub const fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// Called on click, Enter or Space when the trigger has no menu of its
    /// own (the caller opens something).
    #[must_use]
    pub fn on_open(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Its menu: a press, ↵, Space or ↓ opens it below the plate on the
    /// float layer; the chevron turns over while it is open. The menu
    /// closes itself on a choice.
    #[must_use]
    pub fn menu(mut self, menu: Menu) -> Self {
        self.menu = Some(menu);
        self
    }

    /// Its choices as a plain menu of labels; `on_choose` gets the index.
    #[must_use]
    pub fn options<S: Into<SharedString>>(
        self,
        options: impl IntoIterator<Item = S>,
        on_choose: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.menu(Menu::new(options.into_iter().map(MenuItem::new).collect(), on_choose))
    }
}

/// The float key of the menu a select trigger `id` opens.
#[must_use]
pub fn select_menu_key(id: &ElementId) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), "menu".into())
}

impl RenderOnce for Select {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let active = !self.disabled;
        let touch = Touch::read(&self.id, self.look, active, window, cx);
        let motion = touch.motion.clone();
        let id = self.id.clone();
        let height = measure.control(Control::Medium) + px(2.0 * measure.scale());
        let s = f32::from(height) / 32.0;
        let chamfer = 9.0 * s;
        // With a menu of its own, the trigger opens it and reads its state
        // from the float layer.
        let menu_key = select_menu_key(&id);
        let open = self.open || (self.menu.is_some() && crate::overlay::float::is_open(&menu_key, window, cx));
        let open_menu: Option<Handler> = self.menu.clone().map(|menu| {
            let frame = touch.frame.clone();
            let focus = touch.focus.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                // The trigger holds focus first, so the menu hands it back
                // here when it closes (a press opens before the click would
                // have focused it).
                window.focus(&focus, cx);
                menu::open(menu_key.clone(), frame.get(), Side::Below, menu.clone(), window, cx);
            }) as Handler
        });

        let hover = motion.animate(
            track(&id, "hover"),
            if touch.hovered { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let press = motion.animate(
            track(&id, "press"),
            if touch.pressed { 1.0 } else { 0.0 },
            spec::PRESS,
            window,
            cx,
        );
        let lit = motion.animate(
            track(&id, "open"),
            if open || touch.focused { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let turn = motion.animate(
            track(&id, "turn"),
            if open { 1.0 } else { 0.0 },
            spec::LIFT,
            window,
            cx,
        );
        let rest = Edge::of(Bevel::Rest, palette);
        let edge = rest
            .mix(sunk(rest), press)
            .mix(Edge::of(Bevel::Focus, palette), lit);
        let fill = mix(palette.plate.into(), palette.plate2.into(), hover);
        let mut ink: Hsla = palette.ink0.into();
        if self.disabled {
            ink = muted(ink);
        }
        let chevron = icons::chevron(IconSize::S12, mix(palette.ink3.into(), palette.ink1.into(), hover))
            .size(measure.icon(12.0))
            .with_transformation(Transformation::rotate(radians(
                std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * turn,
            )));
        let mark = self
            .lang
            .map(|lang| icons::lang_mark(lang, 16.0).size(measure.icon(16.0)).into_any_element())
            .or_else(|| {
                self.icon.map(|icon| {
                    ui(icon, IconSize::S14, palette.ink2).size(measure.icon(14.0)).into_any_element()
                })
            });
        let plate = cut()
            .chamfer(Chamfer::Px(chamfer))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(fill)
            .flex()
            .items_center()
            .gap(measure.space(Space::Roomy) * 0.8)
            .px(measure.space(Space::Roomy))
            .h(height)
            .min_w(px(176.0) * measure.scale())
            .children(mark)
            .child(
                div()
                    .flex_1()
                    .set(ty::BODY, &measure)
                    .text_color(ink)
                    .whitespace_nowrap()
                    .child(self.value.clone()),
            )
            .child(chevron)
            .id(id)
            .opacity(if self.disabled { 0.42 } else { 1.0 });
        let plate = match (active, open_menu) {
            (false, _) => plate.into_any_element(),
            (true, None) => wire(plate, &touch, self.on_open).into_any_element(),
            (true, Some(open_menu)) => {
                // The pointer opens on press, as menus do (a press on the
                // open trigger closes it: the layer's outside press, and
                // this same press does not reopen it); keys open on ↵,
                // Space and ↓.
                let pressed = open_menu.clone();
                let keyed = open_menu.clone();
                let plate = plate
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| pressed(window, cx))
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "down" && !event.keystroke.modifiers.modified() {
                            keyed(window, cx);
                            cx.stop_propagation();
                        }
                    });
                let by_key: Handler = Rc::new(move |window: &mut Window, cx: &mut App| {
                    if window.last_input_was_keyboard() {
                        open_menu(window, cx);
                    }
                });
                wire(plate, &touch, Some(by_key)).into_any_element()
            }
        };
        hover_zone(plate, &touch, chamfer, active)
    }
}
