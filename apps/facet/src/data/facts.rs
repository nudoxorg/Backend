//! Two quiet lines every page head uses: the facts line under a hero and
//! the lens bar (the page's tabs). Both are one element each.
//!
//! **Facts** (calm): UI words in ink3, identifiers and versions in mono
//! ink2, the one number that is yours in mint, a `·` between facts. Each
//! fact is a door ("since 0.3.0" opens the history lens, "9 uses in your
//! code" the usage lens); the line wraps between facts, never inside one.
//!
//! **Lens bar**: the page's views as words; the current one in the
//! brightest ink over a 2 px mint rule that slides to a new tab when the
//! view changes. Counts stay out of the way — ⌥ x-ray spells them. Each
//! tab is a door (a lens of what that view holds).

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::spell::{Seg, Spell, spell};
use super::text::{Shaped, shape};
use crate::measure::Measure;
use crate::motion::spec;
use crate::paint::geom::{Fill, Poly};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString, Style, Window, point,
    px, size,
};
use std::rc::Rc;

const WORDS: TypeRole = TypeRole {
    size: 12.5,
    line: 17.0,
    ..ty::SMALL
};
const MONO: TypeRole = TypeRole {
    size: 12.0,
    line: 17.0,
    ..ty::MONO_SMALL
};
const YOURS: TypeRole = TypeRole {
    weight: 600.0,
    ..WORDS
};

/// One run of a fact.
#[derive(Clone, Debug)]
pub enum Run {
    /// Plain words (ink3).
    Words(SharedString),
    /// An identifier, path or version (mono, ink2).
    Mono(SharedString),
    /// The number that is yours (mint).
    Yours(SharedString),
    /// A small mark before the words (a language, a kind).
    Mark {
        /// Asset path.
        path: SharedString,
        /// Tint.
        color: Hsla,
    },
}

/// The facts line: `facts(&m).fact([...]).fact([...])`.
pub struct Facts {
    line: Spell,
    count: usize,
    palette: &'static Palette,
}

/// An empty facts line for `measure`.
#[must_use]
pub fn facts(measure: &Measure, palette: &'static Palette) -> Facts {
    Facts {
        line: spell(measure).row_gap(4.0),
        count: 0,
        palette,
    }
}

impl Facts {
    /// Appends one fact (a door: part = its position).
    #[must_use]
    pub fn fact(mut self, runs: impl IntoIterator<Item = Run>) -> Self {
        let part = self.count;
        let p = self.palette;
        if part > 0 {
            self.line = self
                .line
                .seg(Seg::Break)
                .gap(10.0)
                .text("·", WORDS, p.ink4)
                .gap(10.0);
        }
        for run in runs {
            self.line = match run {
                Run::Words(w) => self.line.part_text(part, w, WORDS, p.ink3),
                Run::Mono(w) => self.line.part_text(part, w, MONO, p.ink2),
                Run::Yours(w) => self.line.part_text(part, w, YOURS, p.mint.base),
                Run::Mark { path, color } => self
                    .line
                    .part(part, Seg::Icon { path, size: 12.0, color })
                    .part(part, Seg::Gap(5.0)),
            };
        }
        self.count += 1;
        self
    }

    /// Keys the line and opens its facts through `door`.
    #[must_use]
    pub fn door(mut self, id: impl Into<ElementId>, door: Door) -> Self {
        self.line = self.line.id(id).door(door).side(Side::Below);
        self
    }

    /// Shows a fact as rested (scenes).
    #[must_use]
    pub fn rest(mut self, fact: Option<usize>) -> Self {
        self.line = self.line.rest(fact);
        self
    }

    /// The line.
    #[must_use]
    pub fn build(self) -> Spell {
        self.line
    }
}

impl IntoElement for Facts {
    type Element = Spell;
    fn into_element(self) -> Spell {
        self.line
    }
}

/// One tab of a lens bar.
#[derive(Clone, Debug)]
pub struct Tab {
    /// Its word.
    pub word: SharedString,
    /// How many things the view holds (spelled under x-ray).
    pub count: Option<SharedString>,
}

impl Tab {
    /// A tab named `word`.
    #[must_use]
    pub fn new(word: impl Into<SharedString>) -> Self {
        Self {
            word: word.into(),
            count: None,
        }
    }

    /// Its count.
    #[must_use]
    pub fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = Some(count.into());
        self
    }
}

const TAB: TypeRole = TypeRole {
    weight: 500.0,
    size: 13.0,
    line: 16.0,
    ..ty::SMALL
};
const TAB_COUNT: TypeRole = TypeRole {
    size: 10.5,
    line: 14.0,
    ..ty::MONO_SMALL
};

/// The lens bar. Build with [`lens_bar`].
pub struct LensBar {
    id: ElementId,
    tabs: Rc<[Tab]>,
    current: usize,
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
}

/// A lens bar of `tabs` with `current` selected, as wide as `measure`.
#[must_use]
pub fn lens_bar(id: impl Into<ElementId>, tabs: impl Into<Rc<[Tab]>>, current: usize, measure: &Measure) -> LensBar {
    LensBar {
        id: id.into(),
        tabs: tabs.into(),
        current,
        measure: *measure,
        door: None,
        rest: None,
    }
}

impl LensBar {
    /// Tabs open through `door` (part = tab index); Enter/click activates.
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a tab as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, tab: Option<usize>) -> Self {
        self.rest = tab;
        self
    }

    fn gap(&self) -> f32 {
        // clamp(14px, 2.4cqi, 28px)
        let w = self.measure.effective();
        (w * 0.024).clamp(14.0, 28.0) * self.measure.scale()
    }
}

impl IntoElement for LensBar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct LensBarLayout {
    live: Entity<Live>,
    keys: AnyElement,
    words: Vec<(Shaped, Shaped, Option<Shaped>)>,
}

impl Element for LensBar {
    type RequestLayoutState = LensBarLayout;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, LensBarLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let palette = cx.palette();
        let role = self.measure.role(TAB);
        let spell_counts = self.measure.reveal().xray;
        let words = self
            .tabs
            .iter()
            .map(|t| {
                (
                    shape(t.word.clone(), role, palette.ink3.into(), window),
                    shape(t.word.clone(), role, palette.ink0.into(), window),
                    t.count
                        .clone()
                        .filter(|_| spell_counts)
                        .map(|c| shape(c, self.measure.role(TAB_COUNT), palette.ink4.into(), window)),
                )
            })
            .collect();
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        style.size.height = px(role.line + 11.0 * self.measure.scale()).into();
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, [kid], cx),
            LensBarLayout { live, keys, words },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut LensBarLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut LensBarLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let gap = self.gap();
        let live = layout.live.clone();
        let motion = live.read(cx).motion.clone();
        let hover = live.read(cx).hover.or(self.rest);
        let walk = live::walking(&live, window, cx);

        // Tabs laid left to right.
        let mut spans: Vec<(f32, f32)> = Vec::with_capacity(layout.words.len());
        let mut x = x0;
        for (quiet, _, count) in &layout.words {
            let tw = quiet.width() + count.as_ref().map_or(0.0, |c| c.width() + 4.0 * s);
            spans.push((x, tw));
            x += tw + gap;
        }
        let baseline = y0 + layout.words.first().map_or(0.0, |(q, _, _)| q.ascent());
        for (i, ((quiet, bright, count), (tx, _))) in layout.words.iter().zip(&spans).enumerate() {
            let on = i == self.current || hover == Some(i);
            if on { bright } else { quiet }.paint(*tx, baseline, window, cx);
            if let Some(count) = count {
                count.paint(tx + quiet.width() + 4.0 * s, baseline, window, cx);
            }
        }
        // The hairline under the bar.
        let mut rule = Fill::new();
        rule.poly(&Poly::rect(x0, y0 + h - 1.0, w, 1.0));
        rule.paint(window, Hsla::from(palette.line1));
        // The current tab's mint rule slides to its tab.
        if let Some(&(tx, tw)) = spans.get(self.current) {
            let rx = motion.animate(live::key(&self.id, "rule-x"), tx - x0, spec::LIFT, window, cx);
            let rw = motion.animate(live::key(&self.id, "rule-w"), tw, spec::LIFT, window, cx);
            let mut mint = Fill::new();
            mint.poly(&Poly::rect(x0 + rx, y0 + h - 2.0 * s.max(1.0), rw, 2.0 * s.max(1.0)));
            mint.paint(window, Hsla::from(palette.mint.base));
        }
        if let Some(&(tx, tw)) = walk.and_then(|i| spans.get(i)) {
            let mut light = Fill::new();
            light.poly(&Poly::rect(tx, y0 + h + 1.0, tw, 2.0 * s.max(1.0)));
            light.paint(window, Hsla::from(palette.peri_hi));
        }

        let hit_spans = spans.clone();
        let half = gap * 0.5;
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: self.door.clone(),
                side: Side::Below,
                count: spans.len(),
                hit: Rc::new(move |p: Point<Pixels>| {
                    let px_ = f32::from(p.x);
                    let i = hit_spans.partition_point(|(x, _)| *x - half <= px_).checked_sub(1)?;
                    let (x, tw) = hit_spans[i];
                    (px_ <= x + tw + half).then_some(i)
                }),
                anchor: Rc::new(move |i| {
                    let (x, tw) = *spans.get(i)?;
                    Some(Bounds::new(point(px(x), px(y0)), size(px(tw), px(h))))
                }),
                step: Rc::new(live::linear),
            },
            &mut layout.keys,
            hitbox,
            window,
            cx,
        );
    }
}
