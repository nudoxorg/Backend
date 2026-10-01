//! Features as a bar of switches you can scroll along and flip. Turning one
//! on switches on everything it needs: those snap on too and lock (a small
//! padlock closes on each), held by the one that asked for them, and the
//! optional packages it pulls in are counted at the head of the bar. Trying
//! to turn off a locked one shakes it and names what holds it.
//!
//! The switches are a way to *read* the package (what turning `full` on
//! would pull in); they change nothing on disk. What is switched on is the
//! bar's own memory, starting from the package's default features.

use super::berg::lines;
use super::text::{key, one};
use crate::controls::button::wire;
use crate::controls::state::{Touch, hover_zone, track};
use crate::measure::{Measure, Space};
use crate::motion::{Spec, spec};
use crate::paint::geom::{Fill, Poly, pt};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::motion::{BOUNCE, QUICK};
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement,
    App, Bounds, ColorExt as _, ElementId, Entity, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement,
    Styled, Window, canvas, div, px,
};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// What one feature switches on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeatureNode {
    /// Other features it turns on.
    pub enables: Vec<String>,
    /// Optional packages it pulls in.
    pub deps: Vec<String>,
}

/// A package's features.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeatureFacts {
    /// Every feature, in the manifest's order (`default` excluded).
    pub names: Vec<String>,
    /// The features `default` turns on.
    pub default: Vec<String>,
    /// What each switches on.
    pub graph: BTreeMap<String, FeatureNode>,
    /// What each optional package weighs, in lines (`None`: not on this machine).
    pub sizes: BTreeMap<String, Option<usize>>,
}

/// What a set of chosen features comes to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Resolved {
    /// Features that are on, chosen or held.
    pub on: BTreeSet<String>,
    /// On because another chosen feature needs them.
    pub locked: BTreeSet<String>,
    /// Optional packages pulled in, in first-named order.
    pub pulled: Vec<String>,
    /// Their lines, where known.
    pub lines: usize,
}

impl FeatureFacts {
    fn enables(&self, name: &str) -> &[String] {
        self.graph.get(name).map_or(&[], |node| node.enables.as_slice())
    }

    /// Everything `name` turns on, transitively.
    #[must_use]
    pub fn closure(&self, name: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut stack = vec![name.to_owned()];
        while let Some(at) = stack.pop() {
            for next in self.enables(&at) {
                if out.insert(next.clone()) {
                    stack.push(next.clone());
                }
            }
        }
        out
    }

    /// The features the package turns on by default.
    #[must_use]
    pub fn defaults(&self) -> BTreeSet<String> {
        self.default.iter().filter(|f| *f != "default").cloned().collect()
    }

    /// What `chosen` comes to.
    #[must_use]
    pub fn resolve(&self, chosen: &BTreeSet<String>) -> Resolved {
        let mut on = BTreeSet::new();
        for feature in chosen {
            on.insert(feature.clone());
            on.extend(self.closure(feature));
        }
        let locked = on.difference(chosen).cloned().collect();
        let mut pulled: Vec<String> = Vec::new();
        for feature in &self.names {
            if on.contains(feature)
                && let Some(node) = self.graph.get(feature)
            {
                for dep in &node.deps {
                    if !pulled.contains(dep) {
                        pulled.push(dep.clone());
                    }
                }
            }
        }
        let lines = pulled.iter().map(|d| self.sizes.get(d).copied().flatten().unwrap_or(0)).sum();
        Resolved { on, locked, pulled, lines }
    }

    /// The chosen features that hold `name` on.
    #[must_use]
    pub fn held_by(&self, name: &str, chosen: &BTreeSet<String>) -> Vec<String> {
        chosen.iter().filter(|c| c.as_str() != name && self.closure(c).contains(name)).cloned().collect()
    }
}

/// What the bar remembers.
#[derive(Clone, Debug, Default)]
struct Memory {
    chosen: Option<BTreeSet<String>>,
    /// Bumped on every flip, so a flash is a new track each time.
    epoch: u64,
    /// The features that snapped on with the last flip.
    flashed: Vec<String>,
    /// The locked feature that was pressed, and when in `epoch`.
    shaken: Option<(String, u64)>,
}

const NAME: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::MONO_SMALL };
const SMALL: TypeRole = TypeRole { size: 10.5, line: 14.0, ..ty::MONO_SMALL };
const LABEL: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::SMALL };
const NUMBER: TypeRole = TypeRole { weight: 600.0, size: 12.0, line: 16.0, ..ty::MONO_SMALL };

/// What a host may do with each chip: its index, its name, what pressing it
/// does, and the chip itself (to wrap in a keyboard door).
pub type Wrap = Rc<dyn Fn(usize, &str, Rc<dyn Fn(&mut Window, &mut App)>, AnyElement) -> AnyElement>;

/// The bar (see [`features`]).
#[derive(IntoElement)]
pub struct FeatureBar {
    id: ElementId,
    facts: Rc<FeatureFacts>,
    measure: Measure,
    width: Pixels,
    initial: Option<BTreeSet<String>>,
    wrap: Option<Wrap>,
}

/// A bar of `facts` `width` px wide.
#[must_use]
pub fn features(id: impl Into<ElementId>, facts: Rc<FeatureFacts>, width: Pixels, measure: &Measure) -> FeatureBar {
    FeatureBar { id: id.into(), facts, measure: *measure, width, initial: None, wrap: None }
}

impl FeatureBar {
    /// Starts with these features chosen instead of the package's defaults
    /// (scenes, tests); once the reader flips one this no longer applies.
    #[must_use]
    pub fn chosen(mut self, names: &[&str]) -> Self {
        self.initial = Some(names.iter().map(|n| (*n).to_owned()).collect());
        self
    }

    /// The host's hook on each chip: it gets the chip and what pressing it
    /// does, so it can make the chip a keyboard target.
    #[must_use]
    pub fn wrap(mut self, wrap: impl Fn(usize, &str, Rc<dyn Fn(&mut Window, &mut App)>, AnyElement) -> AnyElement + 'static) -> Self {
        self.wrap = Some(Rc::new(wrap));
        self
    }
}

/// A padlock, `size` px, its shackle `open` (0 closed, 1 lifted).
fn padlock(size: f32, ink: Hsla, open: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let k = size / 12.0;
            let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let mut fill = Fill::new();
            fill.poly(&Poly::rect(ox + 2.6 * k, oy + 5.6 * k, 6.8 * k, 4.8 * k));
            let lift = open * 3.2 * k;
            let arch = [(4.0, 5.6), (4.0, 3.9), (5.0, 2.0), (7.0, 2.0), (8.0, 3.9), (8.0, 5.6)];
            let w = 1.3 * k;
            for pair in arch.windows(2) {
                let (a, b) = (pt(ox + pair[0].0 * k, oy + pair[0].1 * k - lift), pt(ox + pair[1].0 * k, oy + pair[1].1 * k - lift));
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = dx.hypot(dy).max(1e-3);
                let (nx, ny) = (-dy / len * w * 0.5, dx / len * w * 0.5);
                let quad = [pt(a.x + nx, a.y + ny), pt(b.x + nx, b.y + ny), pt(b.x - nx, b.y - ny), pt(a.x - nx, a.y - ny)];
                let area: f32 = (0..4).map(|i| quad[i].x * quad[(i + 1) % 4].y - quad[(i + 1) % 4].x * quad[i].y).sum();
                fill.poly(&if area >= 0.0 { Poly::new(quad) } else { Poly::new([quad[3], quad[2], quad[1], quad[0]]) });
            }
            fill.paint(window, ink);
        },
    )
    .flex_none()
    .size(px(size))
}

/// The switch: a short cut track and a diamond bead.
fn switch(scale: f32, on: f32, locked: f32, palette: &'static Palette) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (22.0 * scale, 12.0 * scale);
            let track = Poly::chamfer(ox, oy, w, h, 3.0 * scale);
            let off: Hsla = palette.g5.into();
            let mint: Hsla = palette.mint.base.into();
            let peri: Hsla = palette.peri.base.into();
            let held = mix(mint, peri, locked);
            let mut back = Fill::new();
            back.poly(&track);
            back.paint(window, mix(off, Hsla::from(palette.table), 0.3));
            let mut lit = Fill::new();
            lit.poly(&track);
            lit.paint(window, held.opacity(0.28 * on));
            let mut edge = Fill::new();
            for ring in track.offset(-0.6).stroke_ring(1.2 * scale.min(1.4)) {
                edge.poly(&ring);
            }
            edge.paint(window, mix(Hsla::from(palette.ink3), held, on));
            let r = 3.6 * scale;
            let cx = ox + 6.0 * scale + 10.0 * scale * on;
            let cy = oy + h * 0.5;
            let bead = Poly::new([pt(cx, cy - r), pt(cx + r, cy), pt(cx, cy + r), pt(cx - r, cy)]);
            let mut b = Fill::new();
            b.poly(&bead);
            b.paint(window, mix(Hsla::from(palette.ink3), mix(mint, Hsla::from(palette.peri_hi), locked), on));
        },
    )
    .flex_none()
    .w(px(22.0 * scale))
    .h(px(12.0 * scale))
}

#[derive(IntoElement)]
struct Chip {
    id: ElementId,
    name: SharedString,
    enables: usize,
    on: bool,
    locked: bool,
    default: bool,
    flash: f32,
    shake: f32,
    measure: Measure,
    act: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl RenderOnce for Chip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let motion = touch.motion.clone();
        let hover = motion.animate(track(&self.id, "hover"), if touch.hovered { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let on = motion.animate(track(&self.id, "on"), if self.on { 1.0 } else { 0.0 }, spec::REVEAL, window, cx);
        let locked = motion.animate(track(&self.id, "locked"), if self.locked { 1.0 } else { 0.0 }, Spec::tween(QUICK, BOUNCE), window, cx);
        let mut rest = Edge::of(Bevel::Rest, palette);
        rest.hi = palette.line3.into();
        rest.lo = palette.line2.into();
        let voiced = mix(Edge::of(Bevel::Hot, palette).hi, Edge::of(Bevel::Peri, palette).hi, locked);
        let mut on_edge = Edge::of(Bevel::Hot, palette);
        on_edge.hi = voiced;
        let edge = rest.mix(on_edge, on).mix(Edge::of(Bevel::Peri, palette), hover * (1.0 - on * 0.6)).mix(Edge::of(Bevel::Peri, palette), self.flash);
        let plate_ink: Hsla = palette.plate.into();
        let tint = mix(mix(plate_ink, palette.mint.base.into(), on * 0.09), palette.peri.base.into(), locked * 0.10);
        let ink = mix(palette.ink2.into(), palette.ink0.into(), on.max(hover));
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(measure.space(Space::Snug))
            .h(px(28.0 * scale))
            .px(measure.space(Space::Base))
            .child(switch(scale, on, locked, palette))
            .child(one(key(&self.id, "name"), self.name.clone(), NAME, ink, &measure));
        if self.locked {
            row = row.child(padlock(12.0 * scale, palette.peri_hi.into(), 1.0 - locked));
        } else if self.enables > 0 {
            row = row.child(one(key(&self.id, "enables"), format!("+{}", self.enables), SMALL, palette.ink3, &measure));
        }
        if self.default {
            row = row.child(one(key(&self.id, "default"), "default", SMALL, palette.ink3, &measure));
        }
        let shake = if self.shake > 0.0 { (self.shake * std::f32::consts::TAU * 3.0).sin() * 3.0 * scale * self.shake } else { 0.0 };
        let plate = cut()
            .chamfer(Chamfer::Px(4.0 * scale))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(tint)
            .ml(px(shake))
            .child(row)
            .id(self.id.clone());
        let act = self.act.clone();
        let plate = wire(plate, &touch, Some(act));
        hover_zone(plate, &touch, 4.0 * scale, true)
    }
}

impl RenderOnce for FeatureBar {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let facts = self.facts.clone();
        let memory: Entity<Memory> = window.use_keyed_state(self.id.clone(), cx, |_, _| Memory::default());
        let current = memory.read(cx).clone();
        let chosen = current.chosen.clone().or_else(|| self.initial.clone()).unwrap_or_else(|| facts.defaults());
        let resolved = facts.resolve(&chosen);
        let motion = crate::motion::Motion::scoped(ElementId::View(memory.entity_id()), cx);

        let head = {
            let mut head = div().flex().flex_wrap().items_baseline().gap_x(measure.space(Space::Snug));
            head = head
                .child(one(key(&self.id, "label"), "Features", LABEL, palette.ink3, &measure))
                .child(one(key(&self.id, "on"), resolved.on.len().to_string(), NUMBER, palette.ink0, &measure))
                .child(one(key(&self.id, "of"), format!("of {} on", facts.names.len()), LABEL, palette.ink3, &measure));
            if !resolved.pulled.is_empty() {
                head = head
                    .child(one(key(&self.id, "pulls"), "· pulls in", LABEL, palette.ink3, &measure))
                    .child(one(key(&self.id, "pulled"), resolved.pulled.len().to_string(), NUMBER, palette.ink0, &measure));
                if resolved.lines > 0 {
                    head = head.child(one(key(&self.id, "lines"), format!("({} lines)", lines(resolved.lines)), LABEL, palette.ink3, &measure));
                }
            }
            head
        };

        let mut row = div().id(key(&self.id, "row")).flex().gap(measure.space(Space::Tight)).pb(measure.space(Space::Snug)).overflow_x_scroll().w(self.width);
        for (index, name) in facts.names.iter().enumerate() {
            let is_on = resolved.on.contains(name);
            let is_locked = resolved.locked.contains(name);
            let flash = if current.flashed.contains(name) {
                motion.animate_from(key(&self.id, format!("flash-{name}-{}", current.epoch)), 1.0, 0.0, Spec::tween(std::time::Duration::from_millis(420), crate::tokens::motion::GLIDE), window, cx)
            } else {
                0.0
            };
            let shake = match &current.shaken {
                Some((shaken, epoch)) if shaken == name => {
                    motion.animate_from(key(&self.id, format!("shake-{name}-{epoch}")), 1.0, 0.0, Spec::tween(std::time::Duration::from_millis(360), crate::motion::LINEAR), window, cx)
                }
                _ => 0.0,
            };
            let act: Rc<dyn Fn(&mut Window, &mut App)> = {
                let (memory, facts, name, initial) = (memory.clone(), facts.clone(), name.clone(), self.initial.clone());
                Rc::new(move |_window, cx| {
                    memory.update(cx, |memory, cx| {
                        let mut chosen = memory.chosen.clone().or_else(|| initial.clone()).unwrap_or_else(|| facts.defaults());
                        let before = facts.resolve(&chosen).on;
                        memory.epoch += 1;
                        if facts.resolve(&chosen).locked.contains(&name) {
                            // Held on by another: it shakes, and says so.
                            memory.shaken = Some((name.clone(), memory.epoch));
                            memory.flashed.clear();
                        } else {
                            if !chosen.remove(&name) {
                                chosen.insert(name.clone());
                            }
                            let after = facts.resolve(&chosen).on;
                            memory.flashed = after.difference(&before).cloned().collect();
                            memory.shaken = None;
                            memory.chosen = Some(chosen);
                        }
                        cx.notify();
                    });
                })
            };
            let chip = Chip {
                id: key(&self.id, format!("chip-{index}")),
                name: name.clone().into(),
                enables: facts.enables(name).len(),
                on: is_on,
                locked: is_locked,
                default: facts.default.contains(name),
                flash,
                shake,
                measure,
                act: act.clone(),
            };
            row = match &self.wrap {
                Some(wrap) => row.child(wrap(index, name, act, chip.into_any_element())),
                None => row.child(chip),
            };
        }
        if !resolved.pulled.is_empty() {
            let shown: Vec<&str> = resolved.pulled.iter().take(8).map(String::as_str).collect();
            row = row.child(
                div().flex_none().flex().items_center().px(measure.space(Space::Roomy)).child(one(
                    key(&self.id, "deps"),
                    format!("+ {}", shown.join(" · ")),
                    NAME,
                    palette.peri_hi,
                    &measure,
                )),
            );
        }
        // What holds a locked chip on is said in its own words under the bar.
        let note = current.shaken.as_ref().and_then(|(name, _)| {
            let holders = facts.held_by(name, &chosen);
            (!holders.is_empty()).then(|| format!("{name} is held on by {}", holders.join(", ")))
        });
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Snug))
            .w(self.width)
            .child(head)
            .child(row)
            .children(note.map(|note| one(key(&self.id, "held"), note, LABEL, palette.peri_hi, &measure)))
    }
}

#[cfg(test)]
mod tests {
    use super::{FeatureFacts, FeatureNode};
    use std::collections::{BTreeMap, BTreeSet};

    fn tokio_like() -> FeatureFacts {
        let node = |enables: &[&str], deps: &[&str]| FeatureNode { enables: enables.iter().map(ToString::to_string).collect(), deps: deps.iter().map(ToString::to_string).collect() };
        FeatureFacts {
            names: ["fs", "full", "io-std", "net", "rt", "rt-multi-thread", "bytes", "mio"].map(str::to_owned).to_vec(),
            default: Vec::new(),
            graph: BTreeMap::from([
                ("fs".to_owned(), node(&[], &[])),
                ("full".to_owned(), node(&["fs", "io-std", "net", "rt-multi-thread"], &[])),
                ("io-std".to_owned(), node(&[], &[])),
                ("net".to_owned(), node(&[], &["mio"])),
                ("rt".to_owned(), node(&[], &[])),
                ("rt-multi-thread".to_owned(), node(&["rt"], &[])),
                ("bytes".to_owned(), node(&[], &["bytes"])),
                ("mio".to_owned(), node(&[], &["mio"])),
            ]),
            sizes: BTreeMap::from([("mio".to_owned(), Some(20_000)), ("bytes".to_owned(), Some(12_000))]),
        }
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn nothing_on_means_nothing_pulled() {
        let resolved = tokio_like().resolve(&BTreeSet::new());
        assert!(resolved.on.is_empty() && resolved.locked.is_empty() && resolved.pulled.is_empty());
    }

    #[test]
    fn turning_full_on_locks_everything_it_needs_and_pulls_in_what_they_bring() {
        let facts = tokio_like();
        let resolved = facts.resolve(&set(&["full"]));
        assert_eq!(resolved.on, set(&["full", "fs", "io-std", "net", "rt-multi-thread", "rt"]), "the whole closure, `rt` through `rt-multi-thread`");
        assert_eq!(resolved.locked, set(&["fs", "io-std", "net", "rt-multi-thread", "rt"]), "everything but `full` itself is held");
        assert_eq!(resolved.pulled, ["mio"], "`net` pulls mio in");
        assert_eq!(resolved.lines, 20_000);
        assert_eq!(facts.held_by("rt", &set(&["full"])), ["full"], "even through rt-multi-thread");
    }

    #[test]
    fn a_feature_chosen_on_its_own_is_not_locked_by_the_one_that_also_needs_it() {
        let facts = tokio_like();
        let resolved = facts.resolve(&set(&["full", "net"]));
        assert!(!resolved.locked.contains("net"), "chosen itself, so it can be turned off");
        assert_eq!(facts.held_by("net", &set(&["full", "net"])), ["full"]);
    }

    #[test]
    fn what_the_package_turns_on_by_default_starts_on() {
        let mut facts = tokio_like();
        facts.default = vec!["rt".to_owned(), "default".to_owned()];
        assert_eq!(facts.defaults(), set(&["rt"]));
    }
}
