//! The page laid out: a header, the call, the docs, "If it fails", "What it
//! is", "What you can do with it", "In your workspace", and the context rail.
//! The main column is capped at 860 px with the rail's 280 px at its right,
//! the pair centred in the reader; below 1100 px of reader width the rail
//! moves under the content, never above it. [`Layout`] decides every width.

use super::body;
use super::call::call;
use super::card::{self, Cards};
use super::host::Host;
use super::key::{Key, Part, Sec, Slot};
use super::kit::{Env, caps, ink, prose, roles, said, spot};
use super::layout::{Layout, Side};
use super::side::{kind_mark, rail};
use super::view::{Uses, View};
use crate::fluid::Modes;
use crate::icons::KindSize;
use crate::measure::Measure;
use crate::tokens::Palette;
use gpui::{AnyElement, InteractiveElement, IntoElement, ParentElement, Styled, div};

/// What the shell draws for the page's two shared elements: the kind mark
/// (the header's gem) and the name (the title, shared with the rows that
/// open it).
pub struct Chrome {
    /// The kind mark.
    pub gem: AnyElement,
    /// The name.
    pub title: AnyElement,
}

fn header(env: &Env<'_>, view: &View, chrome: Chrome) -> AnyElement {
    let i = ink(env.p);
    let head = &view.head;
    let mut line = div().flex().flex_wrap().items_center().gap_x(env.k(8.0)).gap_y(env.k(4.0)).child(div().flex_none().child(chrome.gem)).child(said(env, &Key::of(Part::Kind), caps(head.kind.word()), roles::KIND, i.ink3));
    if !head.path.is_empty() {
        let mut path = div().flex().flex_wrap().items_center().gap(env.k(6.0));
        for (n, part) in head.path.iter().enumerate() {
            let key = Key::of(Part::Path).at(n);
            if n > 0 {
                path = path.child(said(env, &key.field(Slot::Sep), "›", roles::PATH, i.ink3));
            }
            path = path.child(said(env, &key, part.clone(), roles::PATH, if n + 1 == head.path.len() { i.ink2 } else { i.ink3 }));
        }
        line = line.child(path);
    }
    if !head.lang.tag().is_empty() {
        line = line.child(div().flex_none().px(env.k(6.0)).border_1().border_color(i.line3).child(said(env, &Key::of(Part::Lang), head.lang.tag(), roles::LANG, i.ink2)));
    }
    let mut out = div().flex().flex_col().child(line).child(div().mt(env.k(6.0)).mb(env.k(4.0)).child(chrome.title));
    if let Some(lede) = &head.lede {
        out = out.child(div().max_w(env.s(720.0)).child(prose(env, &Key::of(Part::Lede), lede, roles::LEDE, i.ink1)));
    }
    out.into_any_element()
}

/// The page for `view`, with the workspace's `uses`, in the room `measure`
/// describes.
pub fn page(view: &View, uses: &Uses, chrome: Chrome, measure: &Measure, palette: &'static Palette, host: &dyn Host, modes: &Modes) -> AnyElement {
    let ui = host.ui();
    let spots = host.spots();
    let lay = Layout::of(measure, modes);
    // A mode that changes what is drawn is an epoch: the parts that move flow.
    let flow = host.flow();
    flow.epoch((lay.epoch, measure.scale().to_bits()));
    let env = Env { m: measure.within(lay.main), p: palette, host, lay };
    let cards: Cards = card::prepare(&env, view, uses);
    let part = |sec: Sec, el: AnyElement| flow.item(Key::of(Part::Sec(sec)).field(Slot::Flow).id(), el);
    let mut main = div().flex().flex_col().min_w_0().w(lay.main).child(header(&env, view, chrome));
    if let Some(el) = call(&env, &cards, view) {
        main = main.child(part(Sec::Call, el));
    }
    if let Some(el) = body::docs(&env, &view.docs) {
        main = main.child(part(Sec::Docs, el));
    }
    if let Some(fails) = &view.fails {
        main = main.child(part(Sec::Fails, body::fails(&env, &cards, fails)));
    }
    if let Some(shape) = &view.shape {
        main = main.child(part(Sec::Shape, body::shape(&env, &cards, shape)));
    }
    if let Some(el) = body::verbs(&env, &view.verbs) {
        main = main.child(part(Sec::Verbs, el));
    }
    main = main.child(part(Sec::Uses, spot(Sec::Uses, &spots, super::workspace::workspace(&env, uses, &ui))));
    let side_width = match lay.side {
        Side::Beside(width) => width,
        Side::Below => measure.width(),
    };
    let side_env = Env { m: measure.within(side_width), ..env };
    let blocks = rail(&side_env, &view.rail, uses, &ui);
    let i = ink(palette);
    let root = div().id(Key::of(Part::Sec(Sec::Head)).id()).w_full().flex();
    let main = flow.item(Key::of(Part::Sec(Sec::Head)).field(Slot::Main).id(), main);
    let rail_key = Key::of(Part::Sec(Sec::Rail));
    match lay.side {
        Side::Beside(width) => {
            let rail = div().id(rail_key.id()).flex().flex_col().gap(env.k(22.0)).w(width).flex_none().children(blocks);
            root.justify_center().child(div().flex().items_start().gap(lay.gap).child(main).child(flow.item(rail_key.field(Slot::Flow).id(), rail))).into_any_element()
        }
        Side::Below => {
            let rail = div()
                .id(rail_key.id())
                .mt(env.k(44.0))
                .pt(env.k(22.0))
                .border_t_1()
                .border_color(i.line1)
                .flex()
                .flex_wrap()
                .gap_x(env.k(28.0))
                .gap_y(env.k(14.0))
                .children(blocks.into_iter().map(|b| div().min_w(env.s(220.0).min(measure.width())).flex_1().child(b)));
            root.flex_col().child(main).child(flow.item(rail_key.field(Slot::Flow).id(), rail)).into_any_element()
        }
    }
}

/// The header's kind mark for a shell that draws no shared element.
#[must_use]
pub fn gem(view: &View, measure: &Measure, palette: &'static Palette) -> AnyElement {
    let host = super::host::Fixed::new(super::host::Ui::default());
    let env = Env { m: *measure, p: palette, host: &host, lay: Layout::of(measure, &Modes::new()) };
    kind_mark(&env, view.head.kind, KindSize::Md)
}
