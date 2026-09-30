//! The sigil: a kind's mark that carries its facts (the v6 board's
//! `symbol.js` `sigil`, ported).
//!
//! One drawing per kind, the same at every size, so a stack of methods is a
//! row of small pipes you can tell apart:
//!
//! - **a callable** is a chamfered plate. One prong enters from the left per
//!   input (at most five), an arrow leaves right when it gives something, a
//!   coral drop falls below when it can fail, a stem enters from above when
//!   it is a method, the plate's outline is dashed when it waits (async),
//!   and an amber corner says you uphold its rules (unsafe). A macro's plate
//!   carries a bang;
//! - **an enum** is a diamond with one bar per case (at most six), a struct
//!   one tick per field (at most five, a bare line when it holds none), an
//!   alias a diamond inside its own diamond;
//! - **a trait** is a socket: a plate cut with a notch per member you write
//!   (at most four) and a tab per member you get (at most four);
//! - **a value** is a stone ringed by its own outline;
//! - **mint dots** on the lower right, one per crate of yours that names it
//!   (at most six).
//!
//! The drawing is 64 units square; strokes are 2 units from 48 px up, 3
//! from 24 px and 4.6 below, so a 16 px mark keeps a legible line.

use super::page::hue;
use super::plan::{DeclKind, DropVerb, Fam, PagePlan, Spec};
use crate::tokens::Palette;
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, PathBuilder, Pixels, Point, Refineable, Style, StyleRefinement, Styled, Window, point, px,
};

/// What a sigil draws.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Form {
    /// A free function.
    Fn = 0,
    /// A method.
    Method = 1,
    /// A macro.
    Macro = 2,
    /// An enum.
    Enum = 3,
    /// A struct, class or union.
    Struct = 4,
    /// A type alias.
    Alias = 5,
    /// A trait or interface.
    Trait = 6,
    /// A constant, a field, a module: a stone.
    Value = 7,
}

/// A mark's facts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sigil {
    /// The drawing.
    pub form: Form,
    /// The family hue.
    pub fam: Fam,
    /// Inputs (prongs).
    pub ins: u8,
    /// Gives something back (an arrow).
    pub gives: bool,
    /// Can fail (a coral drop).
    pub fails: bool,
    /// Called on something (a stem from above).
    pub recv: bool,
    /// Waits (a dashed outline).
    pub is_async: bool,
    /// You uphold its rules (an amber corner).
    pub is_unsafe: bool,
    /// An enum's cases (bars).
    pub cases: u8,
    /// A struct's fields (ticks).
    pub fields: u8,
    /// A struct that holds nothing (a unit or marker).
    pub marker: bool,
    /// A trait's members you write (notches).
    pub write: u8,
    /// A trait's members you get (tabs).
    pub get: u8,
    /// Crates of yours that name it (mint dots).
    pub yours: u8,
}

impl Sigil {
    /// A bare mark of `form`.
    #[must_use]
    pub const fn new(form: Form, fam: Fam) -> Self {
        Self { form, fam, ins: 0, gives: false, fails: false, recv: false, is_async: false, is_unsafe: false, cases: 0, fields: 0, marker: false, write: 0, get: 0, yours: 0 }
    }

    /// The mark a page's plan says: the kind, and every fact its drawing
    /// holds.
    #[must_use]
    pub fn of(plan: &PagePlan) -> Self {
        let kind = plan.hero.kind;
        let fam = plan.hero.fam;
        let mut sigil = Self::new(form_of(kind), fam);
        match &plan.spec {
            Spec::Callable(callable) => {
                sigil.ins = u8::try_from(callable.ports.len()).unwrap_or(u8::MAX);
                sigil.gives = callable.gives.is_some();
                sigil.fails = callable.drops.iter().any(|drop| drop.verb != DropVerb::Panics);
                sigil.recv = callable.receiver.is_some();
                sigil.is_async = callable.flags.iter().any(|flag| flag.contains("async"));
                sigil.is_unsafe = callable.flags.iter().any(|flag| flag.contains("unsafe"));
                if sigil.form == Form::Fn && callable.receiver.is_some() {
                    sigil.form = Form::Method;
                }
            }
            Spec::Choice(choice) => {
                sigil.form = if kind == DeclKind::Alias { Form::Alias } else { Form::Enum };
                sigil.cases = u8::try_from(choice.cases.len()).unwrap_or(u8::MAX);
            }
            Spec::Record(record) => {
                sigil.form = if kind == DeclKind::Alias { Form::Alias } else { Form::Struct };
                sigil.fields = u8::try_from(record.rungs.len() + record.private as usize).unwrap_or(u8::MAX);
            }
            Spec::Contract(contract) => {
                sigil.form = Form::Trait;
                sigil.write = u8::try_from(contract.write.len()).unwrap_or(u8::MAX);
                sigil.get = u8::try_from(contract.get.len()).unwrap_or(u8::MAX);
            }
            Spec::None => {
                if matches!(kind, DeclKind::Struct | DeclKind::Class | DeclKind::Union) {
                    sigil.marker = true;
                }
            }
        }
        sigil
    }

    /// The same mark with `yours` crates named.
    #[must_use]
    pub const fn yours(mut self, yours: u8) -> Self {
        self.yours = yours;
        self
    }
}

const fn form_of(kind: DeclKind) -> Form {
    match kind {
        DeclKind::Function => Form::Fn,
        DeclKind::Method => Form::Method,
        DeclKind::Enum => Form::Enum,
        DeclKind::Struct | DeclKind::Class | DeclKind::Union => Form::Struct,
        DeclKind::Alias => Form::Alias,
        DeclKind::Trait | DeclKind::Interface => Form::Trait,
        _ => Form::Value,
    }
}

/// The stroke weight in drawing units at `size` px.
#[must_use]
pub fn weight(size: f32) -> f32 {
    if size >= 48.0 { 2.0 } else if size >= 24.0 { 3.0 } else { 4.6 }
}

/// A sigil `size` px square.
#[must_use]
pub fn sigil(facts: Sigil, size: f32, palette: &Palette) -> AnyElement {
    let colors = Colors::of(facts.fam, palette);
    SigilElement { facts, size, colors, style: StyleRefinement::default() }
        .w(px(size))
        .h(px(size))
        .flex_none()
        .into_any_element()
}

struct SigilElement {
    facts: Sigil,
    size: f32,
    colors: Colors,
    style: StyleRefinement,
}

impl Styled for SigilElement {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for SigilElement {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for SigilElement {
    type RequestLayoutState = Style;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Style, _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, style: &mut Style, _: &mut (), window: &mut Window, cx: &mut App) {
        let (facts, size, colors) = (self.facts, self.size, self.colors);
        style.paint(bounds, window, cx, |window, _| paint(&facts, bounds, size, colors, window));
    }
}

/// The inks a sigil is drawn in.
#[derive(Clone, Copy)]
pub struct Colors {
    /// The family hue.
    pub hue: Hsla,
    /// A failure's drop.
    pub coral: Hsla,
    /// Unsafe's corner.
    pub amber: Hsla,
    /// Yours.
    pub mint: Hsla,
    /// A contract's plate is quieter.
    pub contract: bool,
}

impl Colors {
    /// The inks for `fam` in `palette`.
    #[must_use]
    pub fn of(fam: Fam, palette: &Palette) -> Self {
        Self {
            hue: hue(fam, palette).hsla(),
            coral: palette.coral.base.hsla(),
            amber: palette.amber.base.hsla(),
            mint: palette.mint.base.hsla(),
            contract: fam == Fam::Contract,
        }
    }
}

/// Paints `facts` in `bounds` (drawn `size` px square from its origin).
pub fn paint(facts: &Sigil, bounds: Bounds<Pixels>, size: f32, colors: Colors, window: &mut Window) {
    let k = size / 64.0;
    let origin = bounds.origin;
    let p = |x: f32, y: f32| -> Point<Pixels> { point(origin.x + px(x * k), origin.y + px(y * k)) };
    let sw = px(weight(size) * k);
    let (hue, coral, amber, mint) = (colors.hue, colors.coral, colors.amber, colors.mint);
    let plate_fill = hue.opacity(if colors.contract { 0.12 } else { 0.24 });

    let stroke = |window: &mut Window, points: &[Point<Pixels>], color: Hsla, closed: bool| {
        let mut path = PathBuilder::stroke(sw);
        if closed {
            path.add_polygon(points, true);
        } else {
            for (n, at) in points.iter().enumerate() {
                if n == 0 { path.move_to(*at); } else { path.line_to(*at); }
            }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    let fill = |window: &mut Window, points: &[Point<Pixels>], color: Hsla| {
        let mut path = PathBuilder::fill();
        path.add_polygon(points, true);
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    let plate = |window: &mut Window, points: &[Point<Pixels>], dashed: bool| {
        fill(window, points, plate_fill);
        let mut path = PathBuilder::stroke(sw);
        if dashed {
            path = path.dash_array(&[px(5.0 * k), px(3.0 * k)]);
        }
        path.add_polygon(points, true);
        if let Ok(path) = path.build() {
            window.paint_path(path, hue);
        }
    };

    match facts.form {
        Form::Fn | Form::Method | Form::Macro => {
            plate(window, &[p(24.0, 19.0), p(47.0, 19.0), p(47.0, 39.0), p(41.0, 45.0), p(17.0, 45.0), p(17.0, 25.0)], facts.is_async);
            if facts.form == Form::Macro {
                stroke(window, &[p(32.0, 24.0), p(32.0, 35.0)], hue, false);
                stroke(window, &[p(32.0, 39.0), p(32.0, 40.5)], hue, false);
            }
            let n = usize::from(facts.ins.min(5));
            for i in 0..n {
                let y = if n == 1 { 32.0 } else { 21.0 + 22.0 * i as f32 / (n - 1) as f32 };
                stroke(window, &[p(2.0, y), p(8.0, y), p(17.0, 32.0 + (y - 32.0) * 0.3)], hue, false);
            }
            if facts.gives {
                stroke(window, &[p(47.0, 32.0), p(60.0, 32.0)], hue, false);
                stroke(window, &[p(55.0, 27.0), p(60.0, 32.0), p(55.0, 37.0)], hue, false);
            }
            if facts.fails {
                stroke(window, &[p(29.0, 45.0), p(29.0, 57.0), p(38.0, 57.0)], coral, false);
                fill(window, &[p(38.0, 53.0), p(44.0, 53.0), p(44.0, 59.0), p(38.0, 59.0)], coral);
            }
            if facts.recv {
                stroke(window, &[p(32.0, 2.0), p(32.0, 19.0)], hue, false);
                stroke(window, &[p(26.0, 2.0), p(38.0, 2.0)], hue, false);
            }
            if facts.is_unsafe {
                fill(window, &[p(47.0, 19.0), p(47.0, 29.0), p(37.0, 19.0)], amber);
            }
        }
        Form::Trait => {
            let req = usize::from(facts.write.min(4));
            let prov = usize::from(facts.get.min(4));
            // The plate's outline runs up the cut left edge: each notch is a
            // wedge bitten into it.
            let mut outline = vec![p(18.0, 8.0), p(50.0, 8.0), p(50.0, 50.0), p(44.0, 56.0), p(18.0, 56.0)];
            for i in (0..req).rev() {
                let y = 16.0 + (40.0 - 8.0) * (i as f32 + 0.5) / req.max(1) as f32;
                outline.push(p(18.0, y + 5.0));
                outline.push(p(26.0, y));
                outline.push(p(18.0, y - 5.0));
            }
            plate(window, &outline, false);
            for i in 0..prov {
                let y = 16.0 + 32.0 * (i as f32 + 0.5) / prov.max(1) as f32;
                stroke(window, &[p(50.0, y - 3.0), p(57.0, y - 3.0), p(57.0, y + 3.0), p(50.0, y + 3.0)], hue, false);
            }
        }
        Form::Enum | Form::Struct | Form::Alias => {
            plate(window, &[p(32.0, 3.0), p(61.0, 32.0), p(32.0, 61.0), p(3.0, 32.0)], false);
            match facts.form {
                Form::Alias => stroke(window, &[p(32.0, 13.0), p(51.0, 32.0), p(32.0, 51.0), p(13.0, 32.0)], hue, true),
                Form::Enum => {
                    let n = usize::from(facts.cases.min(6));
                    for i in 0..n {
                        let y = 32.0 + (i as f32 - (n as f32 - 1.0) / 2.0) * 5.2;
                        let w = 13.0 - (y - 32.0).abs() * 0.55;
                        stroke(window, &[p(32.0 - w, y), p(32.0 + w, y)], hue, false);
                    }
                }
                _ if !facts.marker => {
                    let n = usize::from(facts.fields.min(5));
                    for i in 0..n {
                        let x = 32.0 + (i as f32 - (n as f32 - 1.0) / 2.0) * 6.0;
                        stroke(window, &[p(x, 27.0), p(x, 37.0)], hue, false);
                    }
                    if n == 0 {
                        stroke(window, &[p(24.0, 32.0), p(40.0, 32.0)], hue, false);
                    }
                }
                _ => {}
            }
        }
        Form::Value => {
            let ring = |r: f32| (0..24).map(|i| {
                let a = i as f32 / 24.0 * std::f32::consts::TAU;
                p(32.0 + r * a.cos(), 32.0 + r * a.sin())
            }).collect::<Vec<_>>();
            plate(window, &ring(13.0), false);
            stroke(window, &ring(21.0), hue, true);
        }
    }

    // Yours: one mint dot per crate of yours that names it, lower right.
    let r = if size >= 40.0 { 2.4 } else { 3.6 };
    for i in 0..usize::from(facts.yours.min(6)) {
        let a = (20.0 + i as f32 * 11.0).to_radians();
        let c = p(32.0 + 31.0 * a.cos(), 32.0 + 31.0 * a.sin());
        let dot = (0..10).map(|n| {
            let t = n as f32 / 10.0 * std::f32::consts::TAU;
            point(c.x + px(r * k * t.cos()), c.y + px(r * k * t.sin()))
        }).collect::<Vec<_>>();
        fill(window, &dot, mint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_thicken_as_the_mark_shrinks() {
        assert!(weight(64.0) < weight(30.0) && weight(30.0) < weight(16.0));
        assert_eq!(weight(64.0), 2.0);
    }

    #[test]
    fn a_kind_draws_its_own_form() {
        assert_eq!(form_of(DeclKind::Function), Form::Fn);
        assert_eq!(form_of(DeclKind::Method), Form::Method);
        assert_eq!(form_of(DeclKind::Enum), Form::Enum);
        assert_eq!(form_of(DeclKind::Interface), Form::Trait);
        assert_eq!(form_of(DeclKind::Constant), Form::Value);
    }
}
