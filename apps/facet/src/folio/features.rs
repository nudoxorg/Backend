//! A read-only projection of manifest feature defaults. A real feature change
//! needs a runtime configuration and compilation request; this view does not
//! simulate either one.

use super::berg::lines;
use super::text::{key, one};
use crate::measure::{Measure, Space};
use crate::paint::geom::{Fill, Poly, pt};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    App, Bounds, ColorExt as _, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, ParentElement, Pixels, Refineable, RenderOnce, SharedString, Style,
    ScrollHandle, StyleRefinement, Styled, Window, div, px,
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

/// What an authoritative feature profile enables.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Resolved {
    /// Features enabled directly or required by the profile.
    pub on: BTreeSet<String>,
    /// On because another profile feature requires them.
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

    /// Resolves the profile and its transitive feature requirements.
    #[must_use]
    pub fn resolve(&self, profile: &BTreeSet<String>) -> Resolved {
        let mut on = BTreeSet::new();
        for feature in profile {
            on.insert(feature.clone());
            on.extend(self.closure(feature));
        }
        let locked = on.difference(profile).cloned().collect();
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
        let lines = pulled
            .iter()
            .fold(0usize, |total, dependency| total.saturating_add(self.sizes.get(dependency).copied().flatten().unwrap_or(0)));
        Resolved { on, locked, pulled, lines }
    }

    /// The profile features that require `name`.
    #[must_use]
    pub fn held_by(&self, name: &str, profile: &BTreeSet<String>) -> Vec<String> {
        profile.iter().filter(|feature| feature.as_str() != name && self.closure(feature).contains(name)).cloned().collect()
    }
}

const NAME: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::MONO_SMALL };
const SMALL: TypeRole = TypeRole { size: 10.5, line: 14.0, ..ty::MONO_SMALL };
const LABEL: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::SMALL };
const NUMBER: TypeRole = TypeRole { weight: 600.0, size: 12.0, line: 16.0, ..ty::MONO_SMALL };

/// A read-only projection of a manifest's default feature profile.
///
/// Feature changes require a real runtime configuration and compilation
/// request, so this view deliberately has no toggle, focus, or action API.
#[derive(IntoElement)]
pub struct FeaturePreview {
    id: ElementId,
    facts: Rc<FeatureFacts>,
    measure: Measure,
    width: Pixels,
}

struct PreviewScroll {
    handle: ScrollHandle,
}

/// Shows source manifest defaults without suggesting they can be changed here.
#[must_use]
pub fn feature_preview(
    id: impl Into<ElementId>,
    facts: Rc<FeatureFacts>,
    width: Pixels,
    measure: &Measure,
) -> FeaturePreview {
    FeaturePreview { id: id.into(), facts, measure: *measure, width }
}

struct PreviewScrollProbe {
    key: SharedString,
    handle: ScrollHandle,
    style: StyleRefinement,
}

impl Styled for PreviewScrollProbe {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for PreviewScrollProbe {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for PreviewScrollProbe {
    type RequestLayoutState = Style;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style.clone(), [], cx), style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut Window,
        cx: &mut App,
    ) {
        if !crate::probe::enabled(cx) {
            return;
        }
        let viewport = self.handle.bounds();
        let reach = self.handle.max_offset();
        let content = Bounds::new(
            viewport.origin,
            gpui::size(viewport.size.width + reach.x, viewport.size.height + reach.y),
        );
        crate::probe::record_scroll_with_offset(
            cx,
            &ElementId::Name(self.key.clone()),
            viewport,
            content,
            self.handle.offset(),
        );
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
}

fn scroll_probe(key: impl Into<SharedString>, handle: ScrollHandle) -> impl IntoElement {
    PreviewScrollProbe { key: key.into(), handle, style: StyleRefinement::default() }
        .absolute()
        .size_0()
}

/// A padlock, `size` px, its shackle `open` (0 closed, 1 lifted).
fn padlock(size: f32, ink: Hsla, open: f32) -> impl IntoElement {
    Padlock {
        size,
        ink,
        open,
        style: StyleRefinement::default(),
    }
    .flex_none()
    .size(px(size))
}

struct Padlock {
    size: f32,
    ink: Hsla,
    open: f32,
    style: StyleRefinement,
}

impl Styled for Padlock {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Padlock {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Padlock {
    type RequestLayoutState = Style;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (size, ink, open) = (self.size, self.ink, self.open);
        style.paint(bounds, window, cx, |window, _| {
            let k = size / 12.0;
            let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let mut fill = Fill::new();
            fill.poly(&Poly::rect(ox + 2.6 * k, oy + 5.6 * k, 6.8 * k, 4.8 * k));
            let lift = open * 3.2 * k;
            let arch = [
                (4.0, 5.6),
                (4.0, 3.9),
                (5.0, 2.0),
                (7.0, 2.0),
                (8.0, 3.9),
                (8.0, 5.6),
            ];
            let w = 1.3 * k;
            for pair in arch.windows(2) {
                let (a, b) = (
                    pt(ox + pair[0].0 * k, oy + pair[0].1 * k - lift),
                    pt(ox + pair[1].0 * k, oy + pair[1].1 * k - lift),
                );
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = dx.hypot(dy).max(1e-3);
                let (nx, ny) = (-dy / len * w * 0.5, dx / len * w * 0.5);
                let quad = [
                    pt(a.x + nx, a.y + ny),
                    pt(b.x + nx, b.y + ny),
                    pt(b.x - nx, b.y - ny),
                    pt(a.x - nx, a.y - ny),
                ];
                let area: f32 = (0..4)
                    .map(|i| quad[i].x * quad[(i + 1) % 4].y - quad[(i + 1) % 4].x * quad[i].y)
                    .sum();
                fill.poly(&if area >= 0.0 {
                    Poly::new(quad)
                } else {
                    Poly::new([quad[3], quad[2], quad[1], quad[0]])
                });
            }
            fill.paint(window, ink);
        });
    }
}

/// A static status mark; it is deliberately not a switch control.
fn feature_mark(scale: f32, on: bool, locked: bool, palette: &'static Palette) -> impl IntoElement {
    FeatureMark { scale, on, locked, palette, style: StyleRefinement::default() }
        .flex_none()
        .size(px(10.0 * scale))
}

struct FeatureMark {
    scale: f32,
    on: bool,
    locked: bool,
    palette: &'static Palette,
    style: StyleRefinement,
}

impl Styled for FeatureMark {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for FeatureMark {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for FeatureMark {
    type RequestLayoutState = Style;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut Window,
        _: &mut App,
    ) {}

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (scale, on, locked, palette) = (self.scale, self.on, self.locked, self.palette);
        style.paint(bounds, window, cx, |window, _| {
            let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let center = 5.0 * scale;
            let radius = 3.8 * scale;
            let diamond = Poly::new([
                pt(ox + center, oy + center - radius),
                pt(ox + center + radius, oy + center),
                pt(ox + center, oy + center + radius),
                pt(ox + center - radius, oy + center),
            ]);
            let tone = if locked { palette.peri_hi } else if on { palette.mint.base } else { palette.ink3 };
            let mut fill = Fill::new();
            fill.poly(&diamond);
            fill.paint(window, Hsla::from(tone).opacity(if on { 0.92 } else { 0.52 }));
        });
    }
}

#[derive(IntoElement)]
struct Chip {
    id: ElementId,
    name: SharedString,
    enables: usize,
    on: bool,
    locked: bool,
    default: bool,
    measure: Measure,
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let ink = if self.on { palette.ink0 } else { palette.ink2 };
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(measure.space(Space::Snug))
            .h(px((28.0 * scale).max(24.0)))
            .px(measure.space(Space::Base))
            .child(feature_mark(scale, self.on, self.locked, palette))
            .child(one(key(&self.id, "name"), self.name.clone(), NAME, ink, &measure));
        if self.locked {
            row = row
                .child(padlock(12.0 * scale, palette.peri_hi.into(), 0.0))
                .child(one(key(&self.id, "required"), "required", SMALL, palette.peri_hi, &measure));
        } else if self.default {
            row = row.child(one(key(&self.id, "default"), "default", SMALL, palette.ink3, &measure));
        } else {
            row = row.child(one(key(&self.id, "not-default"), "not default", SMALL, palette.ink3, &measure));
        }
        if self.enables > 0 {
            row = row.child(one(key(&self.id, "enables"), format!("+{}", self.enables), SMALL, palette.ink3, &measure));
        }
        row.id(self.id.clone())
    }
}

impl RenderOnce for FeaturePreview {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let facts = &self.facts;
        let chosen = facts.defaults();
        let resolved = facts.resolve(&chosen);
        let scroll_state = window.use_keyed_state(key(&self.id, "scroll"), cx, |_, _| PreviewScroll { handle: ScrollHandle::new() });
        let scroll = scroll_state.read(cx).handle.clone();
        let scroll_key = format!("folio-feature-scroll-{}", self.id);

        let mut head = div().flex().flex_wrap().items_baseline().gap_x(measure.space(Space::Snug));
        head = head
            .child(one(key(&self.id, "label"), "Features", LABEL, palette.ink3, &measure))
            .child(one(key(&self.id, "profile"), "read only · manifest defaults", SMALL, palette.ink3, &measure))
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

        let mut row = div()
            .id(key(&self.id, "row"))
            .flex()
            .gap(measure.space(Space::Tight))
            .pb(measure.space(Space::Snug))
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .w(self.width);
        for (index, name) in facts.names.iter().enumerate() {
            row = row.child(Chip {
                id: key(&self.id, format!("chip-{index}")),
                name: name.clone().into(),
                enables: facts.enables(name).len(),
                on: resolved.on.contains(name),
                locked: resolved.locked.contains(name),
                default: facts.default.contains(name),
                measure,
            });
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

        let row = crate::probe::scroll_scope(scroll_key.clone(), row);
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Snug))
            .w(self.width)
            .child(head)
            .child(row)
            .child(scroll_probe(scroll_key, scroll))
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
    fn a_full_profile_locks_everything_it_needs_and_pulls_in_what_they_bring() {
        let facts = tokio_like();
        let resolved = facts.resolve(&set(&["full"]));
        assert_eq!(resolved.on, set(&["full", "fs", "io-std", "net", "rt-multi-thread", "rt"]), "the whole closure, `rt` through `rt-multi-thread`");
        assert_eq!(resolved.locked, set(&["fs", "io-std", "net", "rt-multi-thread", "rt"]), "everything but `full` itself is held");
        assert_eq!(resolved.pulled, ["mio"], "`net` pulls mio in");
        assert_eq!(resolved.lines, 20_000);
        assert_eq!(facts.held_by("rt", &set(&["full"])), ["full"], "even through rt-multi-thread");
    }

    #[test]
    fn an_explicit_profile_entry_is_not_locked_by_another_entry_that_needs_it() {
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

    #[test]
    fn pulled_line_totals_saturate() {
        let mut facts = tokio_like();
        facts.sizes.insert("mio".to_owned(), Some(usize::MAX));
        facts.sizes.insert("bytes".to_owned(), Some(1));
        let resolved = facts.resolve(&set(&["bytes", "net"]));
        assert_eq!(resolved.pulled, ["mio", "bytes"]);
        assert_eq!(resolved.lines, usize::MAX);
    }
}
