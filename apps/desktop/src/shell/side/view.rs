//! Painting the sidebar: the column (the way out, the scope's title, the
//! comb, the lens strip, the narrowing line, the rows) and the spine it
//! collapses to. Everything drawn was decided in the model; nothing here
//! chooses what a row says or does.

use super::{Shelf, ShelfHeightBudget, StickyGeometry};
use super::glyph::{self, Marked};
use super::hold::{self, Chip, Step};
use super::lens::{Counts, Lens};
use super::listing::{Head, Releases, StepDoes, StepOut, Title};
use super::row::{Do, Fold, Heading, Item, Mark, Row, Trailing};
use crate::navigation::Intent;
use crate::shell::focus::{FocusRepresentative, Zone};
use crate::shell::kit::{kind_mark, text};
use facet::icons::{self, IconSize, Kind, KindSize};
use facet::tokens::fluid::SideForm;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Space};
use gpui::{
    AnyElement, App, AppContext as _, AvailableSpace, Bounds, ClickEvent, Context, Element, ElementId, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, ListState, ParentElement,
    Pixels, ScrollHandle, SharedString, Size, StatefulInteractiveElement, Styled, Transformation, Window,
    div, list, point, px, radians,
};
use std::collections::HashSet;
use std::f32::consts::{FRAC_PI_2, PI};
use std::rc::Rc;

/// The list establishes its current-frame scroll top during prepaint. Build
/// the ordinary sticky Div only after that prepaint, so a cold reveal and a
/// resized viewport use the same measured top and bounds in their first paint.
pub(super) struct MeasuredStickyList {
    pub(super) list: AnyElement,
    pub(super) scroll: ListState,
    pub(super) overlay: Box<dyn FnMut(usize, Pixels, &mut App) -> Option<AnyElement>>,
}

impl IntoElement for MeasuredStickyList {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for MeasuredStickyList {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.list.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.list.prepaint(window, cx);
        let first = self.scroll.logical_scroll_top().item_ix;
        let mut overlay = (self.overlay)(first, bounds.size.height, cx)?;
        // A separate root has no containing block for an absolute child;
        // the native Div supplies the list's exact measured viewport.
        overlay = div().relative().size_full().child(overlay).into_any_element();
        overlay.layout_as_root(bounds.size.into(), window, cx);
        overlay.prepaint_at(bounds.origin, window, cx);
        Some(overlay)
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        _: Bounds<Pixels>, _: &mut (), overlay: &mut Option<AnyElement>, window: &mut Window,
        cx: &mut App) {
        self.list.paint(window, cx);
        if let Some(overlay) = overlay { overlay.paint(window, cx); }
    }
}

/// Measure the actual native chrome at the current width, then constrain its
/// scroll viewports before any children prepaint. A container's old bounds or
/// a guessed number of title/Trail lines cannot consume the declaration list.
struct MeasuredColumn {
    package: AnyElement,
    controls: AnyElement,
    rows: AnyElement,
    trail: AnyElement,
    row_height: Pixels,
}

impl IntoElement for MeasuredColumn {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for MeasuredColumn {
    type RequestLayoutState = ();
    type PrepaintState = ShelfHeightBudget;

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = gpui::relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> ShelfHeightBudget {
        let natural = Size { width: AvailableSpace::Definite(bounds.size.width), height: AvailableSpace::MaxContent };
        let package = self.package.layout_as_root(natural, window, cx).height;
        let controls = self.controls.layout_as_root(natural, window, cx).height;
        let trail = self.trail.layout_as_root(natural, window, cx).height;
        let budget = ShelfHeightBudget::new(bounds.size.height, self.row_height, package, controls, trail);
        let mut y = bounds.top();
        for (key, section, height) in [
            ("shelf-package-viewport", &mut self.package, budget.package),
            ("shelf-controls-viewport", &mut self.controls, budget.controls),
            ("shelf-row-viewport", &mut self.rows, budget.rows),
            ("shelf-trail-viewport", &mut self.trail, budget.trail),
        ] {
            if height > px(0.0) {
                let size = Size { width: bounds.size.width, height };
                section.layout_as_root(size.map(AvailableSpace::Definite), window, cx);
                section.prepaint_at(point(bounds.left(), y), window, cx);
                facet::probe::record_bounds(cx, &key.into(), Bounds::new(point(bounds.left(), y), size));
            }
            y += height;
        }
        budget
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        _: Bounds<Pixels>, _: &mut (), budget: &mut ShelfHeightBudget, window: &mut Window, cx: &mut App) {
        for (section, height) in [
            (&mut self.package, budget.package), (&mut self.controls, budget.controls),
            (&mut self.rows, budget.rows), (&mut self.trail, budget.trail),
        ] {
            if height > px(0.0) { section.paint(window, cx); }
        }
    }
}

/// The body retains its natural height while its native viewport can shrink.
/// The probe follows that very handle and clip, so wheel hit testing and
/// reachability evidence refer to the same mounted scroll area.
fn chrome_viewport(key: &'static str, label: &'static str, body: gpui::Div, scroll: &ScrollHandle) -> AnyElement {
    div().relative().size_full()
        .child(facet::probe::scroll_scope(key,
            div().id(key).role(gpui::Role::Group).aria_label(label)
                .size_full().overflow_y_scroll().track_scroll(scroll)
                .flex().flex_col().child(body.w_full().flex_none())))
        .child(facet::probe::scroll_probe(key, scroll.clone()))
        .into_any_element()
}

/// The narrowing line's caret: its width and its height at 100 % text.
const CARET: (f32, f32) = (1.5, 14.0);

/// Where a row's name is measured (its box is what a click hands to a title).
fn name_id(key: &SharedString) -> SharedString {
    SharedString::from(format!("{key}#name"))
}

impl Shelf {
    /// A click in the sidebar gives it the keyboard. Deferred: the shell
    /// re-renders the zone it leaves and the zone it enters, and this shelf
    /// is being updated by the click that asks.
    fn take_keyboard(&self, window: &Window, cx: &mut Context<Self>) {
        let links = self.links.clone();
        let snapshot = links.snapshot(cx);
        let drawer = self.overlay_surface;
        let attachment = links.store.read(cx).current_owner_attachment();
        let scope = links.shell.upgrade().and_then(|shell| shell.read(cx).shelf_input_scope(drawer, cx));
        window.defer(cx, move |window, cx| {
            let current = links.snapshot(cx);
            if current.route() != snapshot.route() || current.overlay() != snapshot.overlay()
                || current.session().reading.current.id != snapshot.session().reading.current.id
                || !current.key().same_authority(snapshot.key())
                || links.store.read(cx).current_owner_attachment() != attachment { return; }
            links.shell(cx, |shell, cx| {
                if scope.as_ref().is_some_and(|scope| shell.admits_shelf_input_scope(scope, cx)) { shell.take_zone(Zone::Shelf, window, cx); }
            });
        });
    }

    /// The full column, drawn at its resting width.
    pub(super) fn column(
        &mut self,
        head: &Head,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gutter = measure.space(Space::Roomy);
        let mut package = div()
            .flex()
            .flex_col()
            .pt(measure.space(Space::Roomy));
        if let Some(step) = &head.step_out {
            package = package.child(self.step_out_row(step, measure, palette, cx));
        }
        #[cfg(test)]
        self.upgrade.replace(None);
        if let Some(title) = &head.title {
            package = package.child(title_block(title, measure, palette));
            if let Some(releases) = &head.releases
                && !releases.list.is_empty()
            {
                for element in self.comb_block(releases, measure, palette) {
                    package = package.child(
                        div()
                            .px(gutter)
                            .pb(measure.space(Space::Roomy))
                            .child(element),
                    );
                }
            }
        }
        if !head.held.is_empty() {
            package = package.child(self.held_chips(&head.held, measure, palette, cx));
        }
        let mut controls = div().flex().flex_col();
        if let Some(counts) = head.counts {
            controls = controls
                .child(self.lens_strip(counts, measure, palette, cx))
                .child(self.narrow_line(measure, palette, cx));
        }
        let top = self.layout.scroll.logical_scroll_top().item_ix;
        let viewport = self.layout.scroll.viewport_bounds();
        let ordinary = f32::from(measure.row() + measure.space(Space::Tight)).max(1.0);
        let capacity = (f32::from(viewport.size.height) / ordinary).ceil() as usize + 32;
        let from = top.saturating_sub(16);
        let through = top.saturating_add(capacity).min(self.layout.rows.len());
        let visible: HashSet<SharedString> = self.layout.rows.get(from..through).unwrap_or(&[])
            .iter().filter_map(Row::item).map(|item| item.key.clone())
            .collect();
        let focused = self.layout.focused_in_list.as_ref().map(|(key, _, _)| key.clone());
        if self.input_surface.is_some_and(|surface|
            matches!(surface, super::super::root::ShelfNativeSurface::Docked | super::super::root::ShelfNativeSurface::Drawer)) {
            self.targets.retain_native_handles(|key| visible.contains(key)
                || focused.as_ref().is_some_and(|focused| focused.as_ref() == key));
        }
        let row_measure = *measure;
        let list = list(self.layout.scroll.clone(),
            cx.processor(move |shelf: &mut Self, index: usize, window, cx| {
                shelf.render_row(index, &row_measure, window, cx)
            }))
            .size_full().into_any_element();
        let shelf = cx.weak_entity();
        let sticky_measure = *measure;
        let sticky_palette = *palette;
        let list = MeasuredStickyList {
            list,
            scroll: self.layout.scroll.clone(),
            overlay: Box::new(move |first, height, cx| shelf.upgrade().and_then(|shelf|
                shelf.update(cx, |shelf, cx| shelf.sticky(first, height, &sticky_measure, &sticky_palette, cx)))),
        };
        let rows = div().relative().size_full()
            .child(
                facet::probe::scroll_scope(
                    "shelf-rows",
                    div()
                    .relative()
                    .size_full()
                    .child(list),
                ),
            )
            .child(facet::probe::list_scroll_probe(
                "shelf-rows",
                self.layout.scroll.clone(),
            ))
            .into_any_element();
        let trail = div().flex().flex_col().children(
            (!head.trail.is_empty()).then(|| self.trail_block(&head.trail, measure, palette, cx)));
        MeasuredColumn {
            package: chrome_viewport("shelf-package", "Package context", package, &self.chrome_scroll.package),
            controls: chrome_viewport("shelf-controls", "Library view controls", controls, &self.chrome_scroll.controls),
            rows,
            trail: chrome_viewport("shelf-trail", "Reading trail", trail, &self.chrome_scroll.trail),
            row_height: measure.row() + measure.space(Space::Tight),
        }.into_any_element()
    }

    /// Sticky ancestors (VS Code's Explorer): in a long list, the chain of
    /// parents of the first visible row stays pinned to the top, so you always
    /// see which module and type you are inside.
    fn sticky(
        &self,
        first: usize,
        viewport_height: Pixels,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let height = measure.row() + measure.space(Space::Tight);
        // The owning list has already prepainted. Its exact current-frame
        // logical top and this wrapper's bounds decide the chain and clip.
        let chain = self.layout.sticky_ancestors(first);
        if chain.is_empty() {
            return None;
        }
        let row_count = chain.len();
        let mut pinned = div()
            .w_full()
            .flex()
            .flex_col()
            .bg(palette.g1)
            .border_b_1()
            .border_color(palette.line1.hsla());
        for (index, item) in chain {
            let guard = self.action_guard(cx);
            let indent =
                measure.space(Space::Roomy) + measure.space(Space::Gutter) * f32::from(item.depth);
            pinned = pinned.child(
                div()
                    .id(SharedString::from(format!("{}#sticky", item.key)))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Return to {}", item.name))
                    .h(height)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Base))
                    .pl(indent)
                    .pr(measure.space(Space::Roomy))
                    .cursor_pointer()
                    .child(mark(item.mark, measure, palette))
                    .child(
                        text(ty::MONO_ROW, measure, palette.ink2)
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(item.name.clone()),
                    )
                    .on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                        if !guard(cx) { return; }
                        shelf.layout.scroll_to_user(index);
                        cx.notify();
                    })),
            );
        }
        // A newly shortened viewport must still leave one real row
        // unobscured in this very paint. Current parent constraints clip the
        // full chain before the list publishes its new measured viewport.
        Some(clipped_sticky_chain(pinned, viewport_height, height, row_count))
    }

    /// What you hold, above the lenses: the hand's cards as chips, each with
    /// the key that goes to it. The shell's own keys (⌘1–⌘5) are the only ones.
    fn held_chips(
        &self,
        chips: &[Chip],
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div()
            .flex()
            .flex_wrap()
            .gap(measure.space(Space::Snug))
            .px(measure.space(Space::Roomy))
            .pb(measure.space(Space::Base));
        for chip in chips {
            let card = chip.card;
            let guard = self.action_guard(cx);
            row = row.child(
                div()
                    .id(SharedString::from(format!("shelf-held-{card}")))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Show {}", chip.name))
                    .focusable()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .px(measure.space(Space::Snug))
                    .py(measure.space(Space::Hair))
                    .cursor_pointer()
                    .border_1()
                    .border_color(palette.line2.hsla())
                    .hover(|style| style.bg(palette.tint))
                    .child(kind_mark(chip.kind, KindSize::Sm, measure, palette))
                    .child(
                        text(ty::MONO_SMALL, measure, palette.ink1)
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(chip.name.clone()),
                    )
                    .child(
                        text(ty::MONO_SMALL, measure, palette.ink3)
                            .flex_none()
                            .child(hold::cap(card)),
                    )
                    .on_click(cx.listener(move |shelf, _: &ClickEvent, window, cx| {
                        if !guard(cx) { return; }
                        shelf.take_keyboard(window, cx);
                        let links = shelf.links.clone();
                        cx.defer(move |cx| links.shell(cx, |shell, cx| shell.hand_card(card, cx)));
                    })),
            );
        }
        row.into_any_element()
    }

    /// Where you have been, at the foot: the history made visible and clickable.
    fn trail_block(
        &self,
        steps: &[Step],
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut places = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(measure.space(Space::Snug));
        for (index, step) in steps.iter().enumerate() {
            let route = step.route.clone();
            let guard = self.action_guard(cx);
            places = places.child(
                div()
                    .id(SharedString::from(format!("shelf-trail-{index}")))
                    .role(gpui::Role::Link)
                    .aria_label(step.label.clone())
                    .focusable()
                    .w_full()
                    .flex_none()
                    .min_w(px(0.0))
                    .max_w_full()
                    .cursor_pointer()
                    .child(text(ty::MONO_SMALL, measure, palette.ink3)
                        .w_full().min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_ellipsis()
                        .child(step.label.clone()))
                    .on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                        if !guard(cx) { return; }
                        shelf.perform(&Do::Go(route.clone()), cx)
                    })),
            );
        }
        div()
            .flex_none()
            .border_t_1()
            .border_color(palette.line1.hsla())
            .px(measure.space(Space::Roomy))
            .py(measure.space(Space::Base))
            .flex()
            .flex_col()
            .gap(measure.space(Space::Hair))
            .child(text(ty::LABEL, measure, palette.ink3).child("TRAIL"))
            .child(places)
            .into_any_element()
    }

    /// "‹ Library": the way out. Browsing: the reader does not move.
    fn step_out_row(
        &self,
        step: &StepOut,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let does = step.does.clone();
        let guard = self.action_guard(cx);
        div()
            .id("shelf-crumb")
            .role(gpui::Role::Button)
            .aria_label(step.label.clone())
            .focusable()
            .flex()
            .items_center()
            .gap(measure.space(Space::Snug))
            .px(measure.space(Space::Roomy))
            .pb(measure.space(Space::Base))
            .cursor_pointer()
            .child(
                icons::chevron(IconSize::S12, palette.ink3)
                    .size(measure.icon(12.0))
                    .with_transformation(Transformation::rotate(radians(PI))),
            )
            .child(text(ty::SMALL, measure, palette.ink2).child(step.label.clone()))
            .on_click(cx.listener(move |shelf, _: &ClickEvent, window, cx| {
                if !guard(cx) { return; }
                shelf.take_keyboard(window, cx);
                match &does {
                    StepDoes::Pop => {
                        shelf.step_out(cx);
                    }
                    StepDoes::Go(route) => {
                        shelf.links.dispatch(Intent::Navigate(route.clone()), cx)
                    }
                }
            }))
            .into_any_element()
    }

    /// The comb's slot under the name (W-Controls' `version_comb`), and away
    /// from the pin the upgrade lens's crate-wide line: choosing a release
    /// re-scopes the route in place.
    fn comb_block(
        &self,
        releases: &Releases,
        measure: &Measure,
        palette: &Palette,
    ) -> Vec<AnyElement> {
        let gutter = measure.space(Space::Roomy);
        #[cfg(test)]
        {
            self.comb.set(Some((releases.pinned, releases.viewing)));
            *self.comb_versions.borrow_mut() = releases
                .list
                .iter()
                .map(|release| release.version.clone())
                .collect();
        }
        let links = self.links.clone();
        let pinned = releases.pinned.map(|index| releases.list[index].id.clone());
        let mut comb = facet::controls::version_comb(
            "shelf-versions",
            Rc::clone(&releases.list),
            &measure.inset(gutter),
        )
        .on_select(move |selected, _, cx| {
            let at = (Some(&selected.0) != pinned.as_ref())
                .then(|| crate::navigation::ReleaseId::new(&selected.0.0).ok())
                .flatten();
            links.dispatch(Intent::SetRelease(at), cx);
        });
        if let Some(index) = releases.pinned {
            comb = comb.pinned(index);
        }
        if let Some(index) = releases.viewing {
            comb = comb.selected(index);
        }
        let mut elements = vec![comb.into_any_element()];
        if let (Some(diffs), Some(pinned), Some(viewing)) =
            (releases.diffs.as_ref(), releases.pinned, releases.viewing)
            && pinned != viewing
        {
            let spelled = |index: usize| {
                let version = &releases.list[index].version;
                crate::runtime::releases::spelled(diffs, version).unwrap_or_else(|| version.clone())
            };
            let (from, to) = (spelled(pinned), spelled(viewing));
            let summary = facet::data::release::summary(diffs, &from, &to);
            #[cfg(test)]
            self.upgrade.replace(Some((from.clone(), to.clone())));
            elements.push(
                facet::data::release::view::shelf_line(
                    "shelf-upgrade",
                    &summary,
                    &from,
                    &to,
                    &measure.inset(gutter),
                    palette,
                )
                .into_any_element(),
            );
        }
        elements
    }

    /// Contents · Versions · Rests on · Used by. The lens you are on shows
    /// how much it holds; choosing one changes the list, never the page. While
    /// a `G` waits, each tab shows the letter that follows it.
    fn lens_strip(
        &self,
        counts: Counts,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut bar = div()
            .id("shelf-lenses")
            .role(gpui::Role::TabList)
            .aria_label("Library views")
            .flex()
            .items_center()
            .justify_between()
            .mx(measure.space(Space::Base))
            .mb(measure.space(Space::Base))
            .border_b_1()
            .border_color(palette.line1.hsla());
        for lens in Lens::ALL {
            let on = lens == self.lens;
            // A lens you are not on says only its chord letter when the room is tight.
            let short = self.form == SideForm::Tight && !on;
            let words = if short {
                lens.chord().to_ascii_uppercase().to_string()
            } else {
                lens.label().to_owned()
            };
            let mut tab = div()
                .id(SharedString::from(format!("shelf-lens-{}", lens.key())))
                .role(gpui::Role::Tab)
                .aria_label(lens.label())
                .aria_selected(on)
                .focusable()
                .relative()
                .flex()
                .items_center()
                .gap(measure.space(Space::Tight))
                .px(measure.space(Space::Hair))
                .py(measure.space(Space::Base))
                .min_w(px(0.0))
                .cursor_pointer()
                // A label gives way (an ellipsis) before the strip overflows the column.
                .child(
                    text(
                        ty::SMALL,
                        measure,
                        if on { palette.ink0 } else { palette.ink3 },
                    )
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(words),
                );
            if self.chord.is_armed() {
                tab = tab.child(
                    text(ty::MONO_SMALL, measure, palette.peri_hi)
                        .flex_none()
                        .child(lens.chord().to_ascii_uppercase().to_string()),
                );
            } else if on && let Some(count) = counts.of(lens) {
                tab = tab.child(
                    text(ty::MONO_SMALL, measure, palette.ink3)
                        .flex_none()
                        .child(count.to_string()),
                );
            }
            if on {
                tab = tab.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(-1.0))
                        .h(px(2.0))
                        .bg(palette.peri.base),
                );
            }
            // A tab is a control a person points at (the keyboard reaches
            // lenses by their `G` chords, not by walking): published as a
            // target, not a stop on the walk.
            let visit = self.reading_visit;
            let guard = self.action_guard(cx);
            let tab = tab.on_click(cx.listener(move |shelf, _: &ClickEvent, window, cx| {
                if !guard(cx) { return; }
                if shelf.links.snapshot(cx).session().reading.current.id != visit { return; }
                shelf.take_keyboard(window, cx);
                shelf.perform(&Do::Lens(lens), cx);
            }));
            bar = bar.child(
                self.targets
                    .track(format!("shelf-lens-{}", lens.key()), tab),
            );
        }
        bar.into_any_element()
    }

    /// No field: what you type narrows the scope. The line says so until you
    /// do, then shows the words and how much of the scope matched ("9 of 191");
    /// with one of your crates chosen it says what it is narrowed to.
    fn narrow_line(&self, measure: &Measure, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let height = measure.row() + measure.space(Space::Tight);
        let guard = self.action_guard(cx);
        let line = div()
            .id("shelf-narrow")
            .on_click(cx.listener(move |shelf, _, window, cx| {
                if !guard(cx) { return; }
                shelf.take_keyboard(window, cx);
                cx.stop_propagation();
            }))
            .h(height)
            .px(measure.space(Space::Roomy))
            .flex()
            .items_center()
            .gap(measure.space(Space::Snug));
        if let Some(via) = &self.via {
            return line
                .child(text(ty::SMALL, measure, palette.ink2).child("only what"))
                .child(text(ty::MONO_SMALL, measure, palette.mint.base).child(via.shared()))
                .child(text(ty::SMALL, measure, palette.ink2).child("uses"))
                .child(div().ml_auto().child(facet::controls::kbd("esc", measure)))
                .into_any_element();
        }
        if self.narrow.is_empty() {
            return line
                .child(text(ty::SMALL, measure, palette.ink3).child("Type to narrow"))
                .into_any_element();
        }
        let mut line = line
            .child(
                text(ty::MONO_ROW, measure, palette.ink0)
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(self.narrow.query().to_owned()),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(CARET.0))
                    .h(px(CARET.1 * measure.scale()))
                    .bg(palette.peri.base),
            );
        if let Some(matched) = self.matched {
            line = line.child(
                text(ty::MONO_SMALL, measure, palette.ink3)
                    .ml_auto()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(format!("{} of {}", matched.shown, matched.of)),
            );
        }
        line.into_any_element()
    }

    fn render_row(
        &mut self,
        index: usize,
        measure: &Measure,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = cx.facet().palette();
        let row = self.layout.rows.get(index).cloned().expect("list item belongs to row layout");
        self.line(index, &row, measure, palette, window, cx)
    }

    fn line(
        &mut self,
        index: usize,
        row: &Row,
        measure: &Measure,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = measure.row() + measure.space(Space::Tight);
        match row {
            Row::Item(item) => self.item(item, height, measure, palette, window, cx),
            Row::Heading(Heading { words, count }) => div()
                .id(SharedString::from(format!("shelf-heading-{index}")))
                .role(gpui::Role::Heading)
                .aria_level(2)
                .aria_label(match count {
                    Some(count) => format!("{words}, {count}").into(),
                    None => words.clone(),
                })
                .h(height)
                .w_full()
                .flex()
                .items_end()
                .gap(measure.space(Space::Base))
                .px(measure.space(Space::Roomy))
                .pb(measure.space(Space::Hair))
                .child(
                    text(ty::LABEL, measure, palette.ink3)
                        .flex_none()
                        .whitespace_nowrap()
                        .child(words.to_uppercase()),
                )
                .children(count.map(|count| {
                    text(ty::MONO_SMALL, measure, palette.ink3).child(count.to_string())
                }))
                .into_any_element(),
            Row::Note(words) => div()
                .id(SharedString::from(format!("shelf-note-{index}")))
                .role(gpui::Role::Label)
                .aria_label(words.clone())
                .min_h(height)
                .w_full()
                .flex()
                .items_center()
                .px(measure.space(Space::Roomy))
                .py(measure.space(Space::Base))
                .child(
                    text(ty::CAPTION, measure, palette.ink3)
                        .min_w(px(0.0))
                        .w_full()
                        .whitespace_normal()
                        .child(words.clone()),
                )
                .into_any_element(),
        }
    }

    fn item(
        &mut self,
        item: &Item,
        height: gpui::Pixels,
        measure: &Measure,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let indent =
            measure.space(Space::Roomy) + measure.space(Space::Gutter) * f32::from(item.depth);
        let ink: Hsla = if item.current {
            palette.ink0.into()
        } else {
            palette.ink1.into()
        };
        let mut element = div()
            .id(item.key.clone())
            .role(if item.does != Do::Nothing && self.input_surface.is_some_and(|surface|
                matches!(surface, super::super::root::ShelfNativeSurface::Docked | super::super::root::ShelfNativeSurface::Drawer)) {
                gpui::Role::Button
            } else { gpui::Role::Label })
            .aria_label(item.accessible_name.clone().unwrap_or_else(|| match &item.sub {
                Some(sub) => format!("{}, {}", item.name, sub).into(),
                None => item.name.clone(),
            }))
            .aria_selected(item.current)
            .relative()
            .h(height)
            .w_full()
            .flex()
            .items_center()
            .gap(measure.space(Space::Base))
            .pl(indent)
            .pr(measure.space(Space::Roomy))
            .hover(|style| style.bg(palette.tint))
            .child(self.chevron(item, measure, palette, cx))
            .child(mark(item.mark, measure, palette))
            .child(self.name(item, ink, measure, palette));
        let row_owns_input = self.input_surface.is_some_and(|surface|
            matches!(surface, super::super::root::ShelfNativeSurface::Docked | super::super::root::ShelfNativeSurface::Drawer));
        if row_owns_input && item.does != Do::Nothing {
            let handle = self.targets.native_handle(&item.key, cx);
            if FocusRepresentative::for_leaf(self.targets.is_focused(&item.key), &handle, window) == FocusRepresentative::Descendant {
                element = element.aria_active_descendant();
            }
            element = element.track_focus(&handle);
        }
        if let Some(sub) = item
            .sub
            .as_ref()
            .filter(|_| self.form == SideForm::Full || matches!(item.mark, Mark::Release(_)))
        {
            element = element.child(
                text(ty::MONO_SMALL, measure, palette.ink3)
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(sub.clone()),
            );
        }
        element = match &item.trailing {
            Trailing::Nothing => element,
            // Keyed by its row: one version stands on many rows, and the
            // probe must tell them apart.
            Trailing::Words(words) => element.child(
                text(ty::MONO_SMALL, measure, palette.ink3)
                    .keyed(SharedString::from(format!("shelf-trailing:{}", item.key)))
                    .ml_auto()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(words.clone()),
            ),
            Trailing::State(state) => element.child(glyph::trailing(state, measure, palette)),
        };
        if item.dim {
            element = element.opacity(0.5);
        }
        if item.current {
            element = element.bg(palette.tint).child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.0))
                    .bg(palette.mint.base),
            );
        }
        if row_owns_input && item.does != Do::Nothing {
            let (id, does, hoists) = (item.key.clone(), item.does.clone(), item.hoists.clone());
            // A row that opens a declaration hands its name's box to that
            // declaration's title (W-Page2's `title_key`), so the title grows
            // out of the row that was clicked. It is registered at the click,
            // not painted as a shared element, because the same declaration
            // can also be a door on the page, and one key may have only one
            // owner per frame. The row for the page you are on opens nothing.
            let opens = item.source.clone().filter(|_| !item.current);
            let visit = self.reading_visit;
            let guard = self.action_guard(cx);
            element =
                element.on_click(cx.listener(move |shelf, event: &ClickEvent, window, cx| {
                    if !guard(cx) { return; }
                    if shelf.links.snapshot(cx).session().reading.current.id != visit { return; }
                    shelf.take_keyboard(window, cx);
                    shelf.targets.focus(id.clone());
                    // A double-click scopes into the row: browsing, not going.
                    if event.click_count() >= 2
                        && let Some(scope) = &hoists
                    {
                        shelf.hoist(scope.clone(), cx);
                        return;
                    }
                    if let Some(symbol) = &opens
                        && let Some(name) = shelf.targets.bounds_of(&name_id(&id))
                    {
                        facet::motion::shared::remember(
                            facet::anatomy::page::title_key(symbol.as_str()),
                            name,
                            window,
                            cx,
                        );
                    }
                    shelf.perform(&does, cx);
                }));
        }
        if let Some(key) = item.warm.clone() {
            element = element.on_hover(cx.listener(move |shelf, hovered: &bool, _, cx| {
                let links = shelf.links.clone();
                shelf.hover.hover(key.clone(), *hovered, &links, cx);
            }));
        }
        if row_owns_input {
            self.targets.track(item.key.clone(), element).into_any_element()
        } else {
            self.targets.measure(item.key.clone(), element).into_any_element()
        }
    }

    /// The disclosure chevron (its own click, the whole slot), or the empty
    /// slot that keeps every mark on one line.
    fn chevron(
        &self,
        item: &Item,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let slot = measure.icon(12.0) * 1.5;
        let Some(fold) = item.fold else {
            return div().flex_none().w(slot).into_any_element();
        };
        let mut icon = icons::chevron(IconSize::S12, palette.ink3).size(measure.icon(12.0));
        if fold == Fold::Open {
            icon = icon.with_transformation(Transformation::rotate(radians(FRAC_PI_2)));
        }
        let id = item.id.clone();
        let guard = self.action_guard(cx);
        div()
            .id(SharedString::from(format!("{}#fold", item.key)))
            .role(gpui::Role::Button)
            .aria_label(format!("{} {}", if fold == Fold::Open { "Collapse" } else { "Expand" }, item.name))
            .aria_expanded(fold == Fold::Open)
            .focusable()
            .flex_none()
            .w(slot)
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(icon)
            .on_click(cx.listener(move |shelf, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                if !guard(cx) { return; }
                shelf.take_keyboard(window, cx);
                shelf.flip(id.clone(), cx);
            }))
            .into_any_element()
    }

    /// The row's name: measured on its own (a click hands its box to the
    /// title it opens), with the words that matched underlined.
    fn name(&self, item: &Item, ink: Hsla, measure: &Measure, palette: &Palette) -> AnyElement {
        // `text(...)` (kit's `Said`) already publishes what it is given as
        // its own probe text once `.child` records its words, so the name is
        // keyed as the row instead of being wrapped a second time (which
        // painted one label twice).
        let said = text(ty::MONO_ROW, measure, ink)
            .keyed(gpui::ElementId::Name(
                format!("shelf-row:{}", item.key).into(),
            ))
            .min_w(px(0.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis();
        let said = match &item.hit {
            Some(hit) => said.child(Marked::new(
                item.name.clone(),
                hit.clone(),
                palette.ink0.into(),
                palette.peri.base.into(),
            )),
            None => said.child(item.name.clone()),
        };
        self.targets
            .measure(name_id(&item.key), said)
            .into_any_element()
    }

    /// The 42 px spine: a mark per top-level row, the current one lit.
    pub(super) fn spine_column(
        &mut self,
        measure: &Measure,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let visible_spine: HashSet<SharedString> = self.layout.rows.iter().filter_map(Row::item)
            .filter(|item| item.depth == 0 || item.current).take(24)
            .map(|item| SharedString::from(format!("spine-{}", item.key))).collect();
        let spine_owns_input = self.input_surface == Some(super::super::root::ShelfNativeSurface::Spine);
        if spine_owns_input {
            self.targets.retain_native_handles(|key| visible_spine.contains(key));
        } else if self.input_surface.is_some() {
            self.targets.retain_native_handles(|key| !key.starts_with("spine-"));
        }
        let side = px(28.0 * measure.scale());
        let gap = measure.space(Space::Snug);
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(gap)
            .pt(measure.space(Space::Roomy))
            .overflow_hidden();
        for item in self
            .layout.rows
            .iter()
            .filter_map(Row::item)
            .filter(|item| item.depth == 0 || item.current)
            .take(24)
        {
            let mut cell = div()
                .id(SharedString::from(format!("spine-{}", item.key)))
                .role(if spine_owns_input && item.does != Do::Nothing { gpui::Role::Button } else { gpui::Role::Label })
                .aria_label(item.accessible_name.clone().unwrap_or_else(|| item.name.clone()))
                .relative()
                .flex_none()
                .size(side)
                .flex()
                .items_center()
                .justify_center()
                .opacity(if item.current { 1.0 } else { 0.62 })
                .hover(|style| style.opacity(1.0))
                .child(mark(item.mark, measure, palette));
            let spine_key = SharedString::from(format!("spine-{}", item.key));
            if item.current {
                cell = cell.child(
                    div()
                        .absolute()
                        .left(-measure.space(Space::Snug))
                        .top_0()
                        .bottom_0()
                        .w(px(2.0))
                        .bg(palette.mint.base),
                );
            }
            if spine_owns_input && item.does != Do::Nothing {
                let guard = self.action_guard_for(Some(super::super::root::ShelfNativeSurface::Spine), cx);
                let action = super::super::focus::TargetAction::new(guard.clone(),
                    super::act(&cx.weak_entity(), item.does.clone(), self.reading_visit, guard))
                    .with_payload(item.does.clone());
                self.targets.push(super::super::focus::Target {
                    id: spine_key.clone(), label: item.name.clone(), action: action.clone(),
                    peek: item.warm.clone(), source: item.source.clone(),
                });
                let handle = self.targets.native_handle(&spine_key, cx);
                if FocusRepresentative::for_leaf(self.targets.is_focused(&spine_key), &handle, window) == FocusRepresentative::Descendant {
                    cell = cell.aria_active_descendant();
                }
                cell = cell.track_focus(&handle);
                let targets = self.targets.clone();
                let id = spine_key.clone();
                cell = cell.on_click(move |_: &ClickEvent, window, cx| {
                    if !action.admits(cx) { return; }
                    targets.focus(id.clone());
                    action.run(window, cx);
                });
                column = column.child(self.targets.track(spine_key, cell));
            } else {
                column = column.child(cell);
            }
        }
        column.into_any_element()
    }
}

/// Use the same current-frame StickyGeometry that the List's reveal callback
/// used. The overlay occupies only its covered height, leaving a native row
/// and its hitbox below even when the viewport is not a multiple of a row.
pub(super) fn clipped_sticky_chain(pinned: impl IntoElement, viewport_height: Pixels,
    row_height: Pixels, row_count: usize) -> AnyElement {
    let covered = StickyGeometry::new(viewport_height, row_height, row_count).covered_height();
    div()
        .absolute().top_0().left_0().right_0().h(covered)
        .child(div().id("shelf-sticky-clip").debug_selector(|| "shelf-sticky-clip".into())
            .relative().h_full().overflow_hidden()
            // The full chain may exceed this frame's viewport. Keep the
            // *deepest* useful ancestors in the new clip, with each
            // native hitbox masked to the actual available height.
            .child(div().absolute().bottom_0().left_0().right_0()
                // A short chain fills the clip and begins at its top. An
                // overfull chain keeps its own height and clips from above.
                .h(row_height * row_count as f32).min_h(gpui::relative(1.0)).child(pinned)))
        .into_any_element()
}

/// A row's mark.
fn mark(mark: Mark, measure: &Measure, palette: &Palette) -> AnyElement {
    match mark {
        Mark::Kind(kind) => kind_mark(kind, KindSize::Sm, measure, palette),
        Mark::Icon(icon) => icons::ui(icon, IconSize::S14, palette.ink2)
            .size(measure.icon(14.0))
            .into_any_element(),
        Mark::Release(release) => glyph::release(release, measure, palette),
    }
}

/// The scope's title: its mark, its name, and one quiet line under it.
fn title_block(title: &Title, measure: &Measure, palette: &Palette) -> AnyElement {
    let (kind, name, detail, size) = match title {
        Title::Library { detail } => (
            Kind::Package,
            SharedString::from("Library"),
            detail.clone(),
            KindSize::Lg,
        ),
        Title::Book { name, version } => {
            (Kind::Package, name.clone(), version.clone(), KindSize::Lg)
        }
        Title::Node { kind, name, detail } => (*kind, name.clone(), detail.clone(), KindSize::Md),
    };
    div()
        .id("shelf-title")
        .role(gpui::Role::Heading)
        .aria_level(1)
        .aria_label(format!("{name}, {detail}"))
        .flex()
        .items_center()
        .gap(measure.space(Space::Roomy))
        .px(measure.space(Space::Roomy))
        .pb(measure.space(Space::Roomy))
        .child(kind_mark(kind, size, measure, palette))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .child(
                    text(ty::HEAD, measure, palette.ink0)
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(name),
                )
                .child(
                    text(ty::MONO_SMALL, measure, palette.ink3)
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(detail),
                ),
        )
        .into_any_element()
}
