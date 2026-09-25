//! The shelf's pieces: the up-link, the book header, the quiet filter and
//! the rows — and [`shelf`], which stacks whichever of them it is given.
//!
//! Rows at rest are a kind mark and a mono name, nothing else ("rows at
//! rest: mark + name"). Under the pointer a row takes a faint wash and shows
//! its one quiet descriptor, if it has one; the page you are reading has a
//! 2 px mint bar and a faint mint wash; keyboard focus doubles the bevel in
//! periwinkle around the row; rows that are not yours are `ink3`. Row height
//! is the density's (`28 × row`), and everything scales with text.

use super::with_alpha;
use crate::Set;
use crate::controls::{Look, field};
use crate::icons::{self, Icon, IconSize, Kind, Stroke, variant_path};
use crate::measure::Measure;
use crate::motion::{Motion, spec};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, gem, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole};
use gpui::{
    AnyElement, App, ElementId, Entity, Hsla, InteractiveElement, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Transformation, Window,
    div, px, radians, svg,
};
use gpui_component::input::InputState;
use std::rc::Rc;
use std::sync::Arc;

/// How a row reads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum RowTone {
    /// A row of your code (`ink2`, brightening under the pointer).
    #[default]
    Yours,
    /// Not yours (`ink3`).
    Dim,
    /// An open parent (`ink1`).
    Open,
    /// The page you are reading: `ink0`, the mint bar and wash.
    Current,
}

/// One row of the shelf.
#[derive(Clone, Debug, PartialEq)]
pub struct ShelfRow {
    /// Its stable identity (the page it opens).
    pub key: ElementId,
    /// The kind mark.
    pub kind: Kind,
    /// The name, in mono.
    pub name: SharedString,
    /// Indent level (0 = the book's top level).
    pub depth: u8,
    /// How it reads.
    pub tone: RowTone,
    /// One quiet descriptor, shown only under the pointer or with focus.
    pub note: Option<SharedString>,
}

impl ShelfRow {
    /// A row of your code.
    #[must_use]
    pub fn new(key: impl Into<ElementId>, kind: Kind, name: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            kind,
            name: name.into(),
            depth: 0,
            tone: RowTone::Yours,
            note: None,
        }
    }

    /// At an indent level.
    #[must_use]
    pub const fn depth(mut self, depth: u8) -> Self {
        self.depth = depth;
        self
    }

    /// Read as `tone`.
    #[must_use]
    pub const fn tone(mut self, tone: RowTone) -> Self {
        self.tone = tone;
        self
    }

    /// A quiet descriptor for hover and focus.
    #[must_use]
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }
}

type OnRow = Rc<dyn Fn(&ElementId, &mut Window, &mut App)>;

const ROW_NAME: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 500.0,
    size: 12.5,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const NOTE: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 11.0,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};
const UP: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 500.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const BOOK_NAME: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 600.0,
    size: 14.0,
    line: 18.0,
    tracking: 0.0,
    italic: false,
};
const BOOK_VERSION: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 11.5,
    line: 15.0,
    tracking: 0.0,
    italic: false,
};

/// A kind mark at `size`, in its family's hue.
pub(crate) fn kind_mark(kind: Kind, size: Pixels, cx: &App) -> AnyElement {
    svg()
        .path(variant_path(kind.path(), Stroke::width(1.8)))
        .size(size)
        .flex_none()
        .text_color(kind.hue(cx.palette()))
        .into_any_element()
}

/// One shelf row.
#[derive(IntoElement)]
pub struct Row {
    row: ShelfRow,
    focused: bool,
    look: Look,
    measure: Measure,
    on_open: Option<OnRow>,
}

/// A shelf row for a shelf of `measure`'s width.
#[must_use]
pub fn row(row: ShelfRow, measure: &Measure) -> Row {
    Row {
        row,
        focused: false,
        look: Look::LIVE,
        measure: *measure,
        on_open: None,
    }
}

impl Row {
    /// The shell's keyboard focus is on this row (J/K walk).
    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Pins an appearance on top of the live state.
    #[must_use]
    pub const fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// Opens the row's page.
    #[must_use]
    pub fn on_open(mut self, handler: impl Fn(&ElementId, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Row {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let s = measure.scale();
        let row = self.row;
        let id = ElementId::NamedChild(Arc::new(row.key.clone()), "row".into());
        let hovered = window.use_keyed_state(id.clone(), cx, |_, _| false);
        let motion = Motion::scoped(ElementId::NamedChild(Arc::new(id.clone()), "m".into()), cx);
        let hover = motion.animate(
            "hover",
            if *hovered.read(cx) || self.look.hover { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let focus = motion.animate(
            "focus",
            if self.focused || self.look.focus { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let current = motion.animate(
            "current",
            if row.tone == RowTone::Current { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let height = px(28.0 * measure.density().row() * s);
        let rest_ink: Hsla = match row.tone {
            RowTone::Yours => palette.ink2.into(),
            RowTone::Dim => palette.ink3.into(),
            RowTone::Open => palette.ink1.into(),
            RowTone::Current => palette.ink0.into(),
        };
        let ink = mix(rest_ink, palette.ink0.into(), hover * 0.6);
        let mint: Hsla = palette.mint.base.into();
        let wash = mix(
            with_alpha(palette.tint.into(), hover),
            with_alpha(mint, 0.05),
            current,
        );
        let pad = px((12.0 + 16.0 * f32::from(row.depth)) * s);
        let show_note = row.note.is_some() && (hover > 0.0 || focus > 0.0);
        let rest = Edge::of(Bevel::Rest, palette);
        let hidden = Edge {
            hi: with_alpha(rest.hi, 0.0),
            lo: with_alpha(rest.lo, 0.0),
            ..rest
        };
        let edge = hidden.mix(Edge::of(Bevel::Focus, palette), focus);
        let mut line = cut()
            .chamfer(Chamfer::Px(6.0 * s))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(wash)
            .relative()
            .flex()
            .items_center()
            .gap(px(9.0 * s))
            .h(height)
            .pl(pad)
            .pr(px(12.0 * s))
            .child(kind_mark(row.kind, measure.icon(14.0), cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .set(ROW_NAME, &measure)
                    .text_color(ink)
                    .child(row.name.clone()),
            );
        if show_note && let Some(note) = row.note.clone() {
            line = line.child(
                div()
                    .flex_none()
                    .set(NOTE, &measure)
                    .text_color(with_alpha(palette.ink3.into(), hover.max(focus)))
                    .child(note),
            );
        }
        if current > 0.0 {
            line = line.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(5.0 * s))
                    .bottom(px(5.0 * s))
                    .w(px(2.0 * s))
                    .bg(with_alpha(mint, current)),
            );
        }
        let hovered_set = hovered.clone();
        let mut line = line
            .id(id)
            .cursor_pointer()
            .on_hover(move |inside, _window, cx| {
                let inside = *inside;
                hovered_set.update(cx, |value, cx| {
                    if *value != inside {
                        *value = inside;
                        cx.notify();
                    }
                });
            });
        if let Some(handler) = self.on_open {
            let key = row.key.clone();
            line = line.on_click(move |_, window, cx| handler(&key, window, cx));
        }
        line
    }
}

/// The up-link: back to the workspace (`‹ backend`).
#[must_use]
pub fn up_link(label: impl Into<SharedString>, measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    div()
        .flex()
        .items_center()
        .gap(px(6.0 * s))
        .h(px(30.0 * s))
        .px(px(14.0 * s))
        .child(
            icons::chevron(IconSize::S12, palette.ink3)
                .size(measure.icon(12.0))
                .with_transformation(Transformation::rotate(radians(std::f32::consts::PI))),
        )
        .child(
            div()
                .set(UP, measure)
                .text_color(palette.ink3.hsla())
                .child(label.into()),
        )
        .into_any_element()
}

/// The book header: the package's gem, its name and version.
#[must_use]
pub fn book_header(
    kind: Kind,
    name: impl Into<SharedString>,
    version: impl Into<SharedString>,
    measure: &Measure,
    cx: &App,
) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    div()
        .flex()
        .items_center()
        .gap(px(10.0 * s))
        .pt(px(8.0 * s))
        .px(px(14.0 * s))
        .pb(px(12.0 * s))
        .child(gem(kind).size(28.0 * s))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0 * s))
                .min_w(px(0.0))
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .set(BOOK_NAME, measure)
                        .text_color(palette.ink0.hsla())
                        .child(name.into()),
                )
                .child(
                    div()
                        .set(BOOK_VERSION, measure)
                        .text_color(palette.ink3.hsla())
                        .child(version.into()),
                ),
        )
        .into_any_element()
}

/// The book plate (the shelf's header on the first v4 board): a cut plate
/// holding the package's gem, name, language and version, and — when given
/// — its release comb underneath.
#[must_use]
pub fn book_plate(
    kind: Kind,
    name: impl Into<SharedString>,
    version: impl Into<SharedString>,
    lang: Option<icons::Lang>,
    comb: Option<AnyElement>,
    measure: &Measure,
    cx: &App,
) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    let head = div()
        .flex()
        .items_center()
        .gap(px(12.0 * s))
        .child(gem(kind).size(34.0 * s))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0 * s))
                .flex_1()
                .min_w(px(0.0))
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .set(crate::tokens::ty::BOOK, measure)
                        .text_color(palette.ink0.hsla())
                        .child(name.into()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0 * s))
                        .children(lang.map(|lang| icons::lang_mark(lang, 14.0).size(measure.icon(14.0))))
                        .child(
                            div()
                                .set(BOOK_VERSION, measure)
                                .text_color(palette.ink2.hsla())
                                .child(version.into()),
                        ),
                ),
        );
    cut()
        .chamfer(Chamfer::Sm)
        .bevel(Bevel::Rest)
        .plate(Plate::Flat)
        .flex()
        .flex_col()
        .gap(px(12.0 * s))
        .mx(px(10.0 * s))
        .pt(px(12.0 * s))
        .px(px(12.0 * s))
        .pb(px(10.0 * s))
        .child(head)
        .children(comb)
        .into_any_element()
}

/// The measure for whatever sits inside a [`book_plate`] of `measure`'s
/// width (its release comb).
#[must_use]
pub fn book_inner(measure: &Measure) -> Measure {
    measure.inset(px(22.0 * measure.scale()))
}

/// Everything a shelf can hold; every piece is optional.
#[derive(Clone)]
pub struct ShelfData {
    /// The up-link's label (`backend`).
    pub up: Option<SharedString>,
    /// The book: kind, name, version.
    pub book: Option<(Kind, SharedString, SharedString)>,
    /// The filter's editing state.
    pub filter: Option<Entity<InputState>>,
    /// The rows.
    pub rows: Vec<ShelfRow>,
    /// The row the shell's keyboard focus is on.
    pub focused: Option<usize>,
}

/// The shelf: whichever pieces `data` has, stacked, for a shelf of
/// `measure`'s width.
#[derive(IntoElement)]
pub struct Shelf {
    id: ElementId,
    data: ShelfData,
    measure: Measure,
    on_open: Option<OnRow>,
}

/// A shelf showing `data`.
#[must_use]
pub fn shelf(id: impl Into<ElementId>, data: ShelfData, measure: &Measure) -> Shelf {
    Shelf {
        id: id.into(),
        data,
        measure: *measure,
        on_open: None,
    }
}

impl Shelf {
    /// A row was opened.
    #[must_use]
    pub fn on_open(mut self, handler: impl Fn(&ElementId, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Shelf {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let s = measure.scale();
        let data = self.data;
        let mut column = div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .flex_none()
            .w(measure.width())
            .h_full()
            .py(px(6.0 * s))
            .bg(palette.pane)
            .border_r_1()
            .border_color(palette.line1.hsla())
            .overflow_hidden();
        if let Some(up) = data.up {
            column = column.child(up_link(up, &measure, cx));
        }
        if let Some((kind, name, version)) = data.book {
            column = column.child(book_header(kind, name, version, &measure, cx));
        }
        if let Some(filter) = &data.filter {
            let inner = measure.inset(px(10.0 * s));
            column = column.child(
                div()
                    .px(px(10.0 * s))
                    .pb(px(8.0 * s))
                    .child(
                        field(
                            ElementId::NamedChild(Arc::new(self.id.clone()), "filter".into()),
                            filter,
                            &inner,
                        )
                        .icon(Icon::Filter)
                        .quiet(),
                    ),
            );
        }
        let rows = data.rows.into_iter().enumerate().map(|(index, shelf_row)| {
            let mut element = row(shelf_row, &measure).focused(data.focused == Some(index));
            if let Some(handler) = self.on_open.clone() {
                element = element.on_open(move |key, window, cx| handler(key, window, cx));
            }
            element
        });
        column.child(div().flex().flex_col().px(px(0.0)).children(rows))
    }
}

/// The spine: the shelf folded to a column of kind marks (42 px).
#[derive(IntoElement)]
pub struct Spine {
    id: ElementId,
    marks: Vec<(ElementId, Kind, SharedString)>,
    current: Option<usize>,
    measure: Measure,
    on_open: Option<OnRow>,
}

/// A spine of `marks` (key, kind, name for the tooltip), `current` lit.
#[must_use]
pub fn spine(
    id: impl Into<ElementId>,
    marks: Vec<(ElementId, Kind, SharedString)>,
    current: Option<usize>,
    measure: &Measure,
) -> Spine {
    Spine {
        id: id.into(),
        marks,
        current,
        measure: *measure,
        on_open: None,
    }
}

impl Spine {
    /// A mark was clicked.
    #[must_use]
    pub fn on_open(mut self, handler: impl Fn(&ElementId, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Spine {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        use crate::overlay::tooltip::Tipped;
        let palette = cx.palette();
        let measure = self.measure;
        let s = measure.scale();
        let motion = Motion::scoped(ElementId::NamedChild(Arc::new(self.id.clone()), "m".into()), cx);
        let mut column = div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap(px(6.0 * s))
            .w(px(42.0 * s))
            .h_full()
            .py(px(12.0 * s))
            .bg(palette.pane)
            .border_r_1()
            .border_color(palette.line1.hsla());
        for (index, (key, kind, name)) in self.marks.into_iter().enumerate() {
            let on = self.current == Some(index);
            let mark_id = ElementId::NamedChild(Arc::new(key.clone()), "spine".into());
            let hovered = window.use_keyed_state(mark_id.clone(), cx, |_, _| false);
            let lit = motion.animate(
                ElementId::NamedChild(Arc::new(key.clone()), "lit".into()),
                if on || *hovered.read(cx) { 1.0 } else { 0.55 },
                spec::HOVER,
                window,
                cx,
            );
            let hovered_set = hovered.clone();
            let mut cell = div()
                .id(mark_id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.0 * s))
                .cursor_pointer()
                .opacity(lit)
                .child(kind_mark(kind, measure.icon(14.0), cx))
                .on_hover(move |inside, _window, cx| {
                    let inside = *inside;
                    hovered_set.update(cx, |value, cx| {
                        if *value != inside {
                            *value = inside;
                            cx.notify();
                        }
                    });
                });
            if let Some(handler) = self.on_open.clone() {
                let key = key.clone();
                cell = cell.on_click(move |_, window, cx| handler(&key, window, cx));
            }
            column = column.child(cell.tip(name));
        }
        column
    }
}

/// The status bar: the address, quietly.
#[must_use]
pub fn status_bar(address: impl Into<SharedString>, measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(measure.width())
        .h(px(26.0 * s))
        .px(px(12.0 * s))
        .bg(palette.pane)
        .border_t_1()
        .border_color(palette.line1.hsla())
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .set(
                    TypeRole {
                        face: Face::Mono,
                        weight: 400.0,
                        size: 11.0,
                        line: 14.0,
                        tracking: 0.0,
                        italic: false,
                    },
                    measure,
                )
                .text_color(palette.ink4.hsla())
                .child(address.into()),
        )
        .into_any_element()
}

/// The pinned column's frame (shown at vast widths): a quiet column at the
/// right holding the float layer's pins.
#[must_use]
pub fn pins_frame(measure: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    let inner = measure.inset(px(14.0 * s));
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(measure.width())
        .h_full()
        .py(px(16.0 * s))
        .px(px(14.0 * s))
        .bg(palette.pane)
        .border_l_1()
        .border_color(palette.line1.hsla())
        .child(crate::overlay::float::pinned_column(&inner, window, cx))
        .into_any_element()
}
