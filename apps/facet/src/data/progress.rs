//! Progress without a progress bar: the **seam** (a 2 px staged strip under
//! the thing being worked on) and **gem progress** (the kind's own stone
//! filling facet by facet).
//!
//! Both read the same stages. A stage is done (mint), now (a marching mint
//! hatch on the ambient pulse), stalled (an amber hatch, still), bad (coral)
//! or to do (the quiet track). In the gem, the twelve facets are shared out
//! among the stages by weight; the fill flows at a steady pace facet by
//! facet toward the new total rather than jumping, working stones flash
//! their lit facets clockwise, a stall turns the stone amber, a failure
//! cracks it coral. Every stage is a door (its tip says what it is doing).

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::text::shape;
use crate::icons::Kind;
use crate::measure::Measure;
use crate::motion::{Spec, pulse, spec};
use crate::paint::geom::{Fill, Poly};
use crate::paint::hatch::Hatch;
use crate::paint::{GemState, gem};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, motion::Bezier, ty};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString, Style, Window,
    point, px, size,
};
use std::rc::Rc;
use std::time::Duration;

/// What one stage says.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum StageState {
    /// Not started.
    #[default]
    Todo,
    /// Complete.
    Done,
    /// Running now.
    Now,
    /// Waiting on something.
    Stall,
    /// Failed.
    Bad,
}

/// One stage.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    /// What it does ("resolve", "index", "seal").
    pub name: SharedString,
    /// Its share of the whole (1.0 = an equal share).
    pub weight: f32,
    /// What it says.
    pub state: StageState,
    /// How far a running stage has got, `0..=1`.
    pub done: f32,
}

impl Stage {
    /// A stage named `name` in `state`, weight 1.
    #[must_use]
    pub fn new(name: impl Into<SharedString>, state: StageState) -> Self {
        Self {
            name: name.into(),
            weight: 1.0,
            state,
            done: match state {
                StageState::Done | StageState::Bad => 1.0,
                _ => 0.0,
            },
        }
    }

    /// Its share of the whole.
    #[must_use]
    pub const fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// How far a running stage has got.
    #[must_use]
    pub const fn done(mut self, done: f32) -> Self {
        self.done = done;
        self
    }
}

/// Lit facets (`0..=12`) for `stages`: done stages count whole, the running
/// or stalled one counts its fraction, the rest nothing.
#[must_use]
pub fn facets(stages: &[Stage]) -> f32 {
    let total: f32 = stages.iter().map(|s| s.weight.max(0.0)).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let lit: f32 = stages
        .iter()
        .map(|s| {
            s.weight.max(0.0)
                * match s.state {
                    StageState::Done | StageState::Bad => 1.0,
                    StageState::Now | StageState::Stall => s.done.clamp(0.0, 1.0),
                    StageState::Todo => 0.0,
                }
        })
        .sum();
    12.0 * lit / total
}

/// The stage facet `f` (0..12, clockwise from 12 o'clock) belongs to.
#[must_use]
pub fn stage_of_facet(stages: &[Stage], f: usize) -> Option<usize> {
    let total: f32 = stages.iter().map(|s| s.weight.max(0.0)).sum();
    if total <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let at = (f as f32 + 0.5) / 12.0 * total;
    let mut acc = 0.0;
    for (i, s) in stages.iter().enumerate() {
        acc += s.weight.max(0.0);
        if at < acc {
            return Some(i);
        }
    }
    stages.len().checked_sub(1)
}

/// The facet under a point `(dx, dy)` from the stone's centre (angle,
/// clockwise from 12 o'clock, twelve equal sectors).
#[must_use]
pub fn facet_at(dx: f32, dy: f32) -> usize {
    let angle = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let f = (angle / (std::f32::consts::TAU / 12.0)) as usize;
    f.min(11)
}

fn state_of(stages: &[Stage]) -> GemState {
    if stages.iter().any(|s| s.state == StageState::Bad) {
        GemState::Cracked
    } else if stages.iter().any(|s| s.state == StageState::Stall) {
        GemState::Stalled
    } else if stages.iter().any(|s| s.state == StageState::Now) {
        GemState::Working
    } else {
        GemState::Normal
    }
}

/// The facet fill's pace: a steady flow, one facet per this long.
const PER_FACET: Duration = Duration::from_millis(70);
const LINEAR: Bezier = Bezier {
    x1: 0.0,
    y1: 0.0,
    x2: 1.0,
    y2: 1.0,
};

/// Gem progress. Build with [`gem_progress`].
pub struct GemProgress {
    id: ElementId,
    kind: Kind,
    stages: Rc<[Stage]>,
    measure: Measure,
    size: f32,
    door: Option<Door>,
    rest: Option<usize>,
}

/// The `kind`'s stone as the progress of `stages`, `size` px at 100 %.
#[must_use]
pub fn gem_progress(id: impl Into<ElementId>, kind: Kind, stages: impl Into<Rc<[Stage]>>, measure: &Measure) -> GemProgress {
    GemProgress {
        id: id.into(),
        kind,
        stages: stages.into(),
        measure: *measure,
        size: 28.0,
        door: None,
        rest: None,
    }
}

impl GemProgress {
    /// Edge length, px at 100 %.
    #[must_use]
    pub const fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Stages open through `door` (part = stage index).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a stage as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, stage: Option<usize>) -> Self {
        self.rest = stage;
        self
    }
}

impl IntoElement for GemProgress {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct GemLayout {
    live: Entity<Live>,
    keys: AnyElement,
    stone: AnyElement,
    word: Option<super::text::Shaped>,
}

impl Element for GemProgress {
    type RequestLayoutState = GemLayout;
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
    ) -> (LayoutId, GemLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let motion = live.read(cx).motion.clone();
        let s = self.measure.scale();
        let edge = self.size * s;
        let target = facets(&self.stages);
        // A steady flow: the tween's length follows the distance to go.
        let fill = {
            let previous = live.read(cx).memo;
            let from = if previous < 0.0 { target } else { previous };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let millis = ((target - from).abs() * PER_FACET.as_millis() as f32) as u64;
            let spec = Spec::tween(Duration::from_millis(millis.max(1)), LINEAR);
            live.update(cx, |state, _| state.memo = target);
            motion.animate(live::key(&self.id, "flow"), target, spec, window, cx)
        };
        let state = state_of(&self.stages);
        let phase = if state == GemState::Working {
            pulse::lease(window, cx).phase(2.4)
        } else {
            0.0
        };
        let mut stone = gem(self.kind)
            .size(edge)
            .progress(fill)
            .state(state)
            .phase(phase)
            .into_any_element();
        let stone_id = stone.request_layout(window, cx);
        let word = self.measure.reveal().xray.then(|| {
            let palette = cx.palette();
            let current = self
                .stages
                .iter()
                .find(|s| matches!(s.state, StageState::Now | StageState::Stall | StageState::Bad))
                .or_else(|| self.stages.last());
            shape(
                current.map_or_else(SharedString::default, |s| s.name.clone()),
                self.measure.role(ty::MONO_SMALL),
                palette.ink3.into(),
                window,
            )
        });
        let mut style = Style::default();
        style.size.width = px(edge + word.as_ref().map_or(0.0, |w| w.width() + 6.0 * s)).into();
        style.size.height = px(edge).into();
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, [kid, stone_id], cx),
            GemLayout {
                live,
                keys,
                stone,
                word,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut GemLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        layout.stone.prepaint(window, cx);
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut GemLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let edge = self.size * s;
        layout.stone.paint(window, cx);
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let centre = (x + edge * 0.5, y + edge * 0.5);
        let live = layout.live.clone();
        let hover = live.read(cx).hover.or(self.rest);
        let walk = live::walking(&live, window, cx);
        // The rested stage: its facets' rim traced in periwinkle.
        if let Some(stage) = hover.or(walk) {
            let k = edge / 48.0;
            let mut ring = Fill::new();
            for (f, tri) in gem::FACETS.iter().enumerate() {
                if stage_of_facet(&self.stages, f) != Some(stage) {
                    continue;
                }
                // The facet's outer edge (its first two points lie on the rim
                // or the table; draw the rim-side pair).
                let p = |(px_, py_): (f32, f32)| crate::paint::geom::pt(x + px_ * k, y + py_ * k);
                let rim: Vec<_> = tri
                    .iter()
                    .copied()
                    .filter(|&(a, b)| ((a - 24.0).abs() + (b - 24.0).abs() - 22.0).abs() < 0.5)
                    .map(p)
                    .collect();
                if rim.len() == 2 {
                    ring.poly(&super::compass::segment(rim[0], rim[1], 1.5 * s));
                }
            }
            ring.paint(
                window,
                Hsla::from(if walk.is_some() { palette.peri_hi } else { palette.peri.base }),
            );
        }
        if let Some(word) = &layout.word {
            word.paint(x + edge + 6.0 * s, centre.1 + word.role.size * 0.36, window, cx);
        }
        let stages = self.stages.clone();
        let anchor_stages = stages.clone();
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: self.door.clone(),
                side: Side::Right,
                count: stages.len(),
                hit: Rc::new(move |p: Point<Pixels>| {
                    let (dx, dy) = (f32::from(p.x) - centre.0, f32::from(p.y) - centre.1);
                    // Inside the diamond: |dx| + |dy| ≤ half the edge.
                    if dx.abs() + dy.abs() > edge * 0.5 {
                        return None;
                    }
                    stage_of_facet(&stages, facet_at(dx, dy))
                }),
                anchor: Rc::new(move |i| {
                    (i < anchor_stages.len()).then(|| Bounds::new(point(px(x), px(y)), size(px(edge), px(edge))))
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

/// The seam. Build with [`seam`].
pub struct Seam {
    id: ElementId,
    stages: Rc<[Stage]>,
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
}

/// A 2 px staged strip of `stages`, as wide as `measure` gives it.
#[must_use]
pub fn seam(id: impl Into<ElementId>, stages: impl Into<Rc<[Stage]>>, measure: &Measure) -> Seam {
    Seam {
        id: id.into(),
        stages: stages.into(),
        measure: *measure,
        door: None,
        rest: None,
    }
}

impl Seam {
    /// Stages open through `door`.
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a stage as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, stage: Option<usize>) -> Self {
        self.rest = stage;
        self
    }
}

impl IntoElement for Seam {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct SeamLayout {
    live: Entity<Live>,
    keys: AnyElement,
    phase: f32,
}

/// Each stage's `(x, width)` along `w` px with `gap` between.
#[must_use]
pub fn spans(stages: &[Stage], w: f32, gap: f32) -> Vec<(f32, f32)> {
    let total: f32 = stages.iter().map(|s| s.weight.max(0.0)).sum::<f32>().max(1e-3);
    #[allow(clippy::cast_precision_loss)]
    let room = (w - gap * stages.len().saturating_sub(1) as f32).max(0.0);
    let mut x = 0.0;
    stages
        .iter()
        .map(|s| {
            let sw = room * s.weight.max(0.0) / total;
            let span = (x, sw);
            x += sw + gap;
            span
        })
        .collect()
}

fn seam_ink(state: StageState, palette: &Palette) -> Hsla {
    match state {
        StageState::Todo => palette.line2.into(),
        StageState::Done | StageState::Now => palette.mint.base.into(),
        StageState::Stall => palette.amber.base.into(),
        StageState::Bad => palette.coral.base.into(),
    }
}

impl Element for Seam {
    type RequestLayoutState = SeamLayout;
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
    ) -> (LayoutId, SeamLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        // The marching hatch rides the leased pulse (≤ 12 fps), only while a
        // stage runs.
        let phase = if self.stages.iter().any(|s| s.state == StageState::Now) {
            pulse::lease(window, cx).phase(0.5)
        } else {
            0.0
        };
        let s = self.measure.scale();
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        // 2 px drawn; the hit band is taller so the seam can be rested on.
        style.size.height = px(10.0 * s).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [kid], cx), SeamLayout { live, keys, phase })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut SeamLayout,
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
        layout: &mut SeamLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let line = 2.0 * s.max(1.0);
        let y = y0 + (h - line) * 0.5;
        let live = layout.live.clone();
        let motion = live.read(cx).motion.clone();
        let hover = live.read(cx).hover.or(self.rest);
        let walk = live::walking(&live, window, cx);
        let spans = spans(&self.stages, w, 2.0 * s);
        for (i, (stage, (sx, sw))) in self.stages.iter().zip(&spans).enumerate() {
            let (sx, sw) = (x0 + sx, *sw);
            // Each stage's fill sweeps in when it starts, never snaps.
            let target = if stage.state == StageState::Todo { 0.0 } else { 1.0 };
            let fill = motion.animate(live::key_n(&self.id, "fill", i as u64), target, spec::LIFT, window, cx).clamp(0.0, 1.0);
            let lit = hover == Some(i) || walk == Some(i);
            let weight = if lit { line + s } else { line };
            let yy = y - (weight - line) * 0.5;
            let mut track = Fill::new();
            track.poly(&Poly::rect(sx, yy, sw, weight));
            track.paint(window, Hsla::from(palette.line2));
            let fw = sw * fill;
            if fw <= 0.0 {
                continue;
            }
            let rect = Poly::rect(sx, yy, fw, weight);
            let frame = Bounds::new(point(px(sx), px(yy)), size(px(fw), px(weight)));
            match stage.state {
                StageState::Now => {
                    let mut soft = Fill::new();
                    soft.poly(&rect);
                    soft.paint(window, Hsla::from(palette.mint.soft));
                    Hatch::vertical(4.0 * s, 8.0 * s)
                        .phase(layout.phase)
                        .paint(window, &rect, frame, seam_ink(stage.state, palette));
                }
                StageState::Stall => {
                    Hatch::vertical(4.0 * s, 8.0 * s).paint(window, &rect, frame, seam_ink(stage.state, palette));
                }
                _ => {
                    let mut solid = Fill::new();
                    solid.poly(&rect);
                    solid.paint(window, seam_ink(stage.state, palette));
                }
            }
        }
        let hit_spans = spans.clone();
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: self.door.clone(),
                side: Side::Below,
                count: self.stages.len(),
                hit: Rc::new(move |p: Point<Pixels>| {
                    let along = f32::from(p.x) - x0;
                    let i = hit_spans.partition_point(|(sx, _)| *sx <= along).checked_sub(1)?;
                    Some(i)
                }),
                anchor: Rc::new(move |i| {
                    let (sx, sw) = *spans.get(i)?;
                    Some(Bounds::new(point(px(x0 + sx), px(y0)), size(px(sw), px(h))))
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

#[cfg(test)]
mod tests {
    use super::{Stage, StageState, facet_at, facets, spans, stage_of_facet};

    fn stages() -> Vec<Stage> {
        vec![
            Stage::new("resolve", StageState::Done),
            Stage::new("fetch", StageState::Done).weight(2.0),
            Stage::new("index", StageState::Now).done(0.5),
            Stage::new("seal", StageState::Todo),
        ]
    }

    #[test]
    fn facets_follow_the_weights() {
        // 5 weight units: done 3, half of 1 running → 3.5 / 5 of 12.
        assert!((facets(&stages()) - 12.0 * 3.5 / 5.0).abs() < 1e-5);
        assert!(facets(&[]).abs() < f32::EPSILON);
        let all: Vec<Stage> = (0..3).map(|_| Stage::new("x", StageState::Done)).collect();
        assert!((facets(&all) - 12.0).abs() < 1e-5);
    }

    #[test]
    fn every_facet_belongs_to_the_stage_that_owns_its_share() {
        let owners: Vec<usize> = (0..12).filter_map(|f| stage_of_facet(&stages(), f)).collect();
        // Weights 1:2:1:1 over 12 facets → 2.4 / 4.8 / 2.4 / 2.4 facets
        // each; a facet goes to the stage its centre falls in.
        assert_eq!(owners, vec![0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 3, 3]);
    }

    #[test]
    fn the_facet_under_the_pointer_runs_clockwise_from_twelve() {
        assert_eq!(facet_at(0.1, -10.0), 0);
        assert_eq!(facet_at(10.0, -0.1), 2);
        assert_eq!(facet_at(10.0, 0.1), 3);
        assert_eq!(facet_at(0.1, 10.0), 5);
        assert_eq!(facet_at(-0.1, 10.0), 6);
        assert_eq!(facet_at(-10.0, 0.1), 8);
        assert_eq!(facet_at(-10.0, -0.1), 9);
        assert_eq!(facet_at(-0.1, -10.0), 11);
    }

    #[test]
    fn spans_share_the_width_by_weight() {
        let sp = spans(&stages(), 102.0, 2.0);
        let total: f32 = sp.iter().map(|s| s.1).sum();
        assert!((total - 96.0).abs() < 1e-4);
        assert!((sp[1].1 - 2.0 * sp[0].1).abs() < 1e-4);
        assert!((sp[3].0 + sp[3].1 - 102.0).abs() < 1e-4);
    }
}
