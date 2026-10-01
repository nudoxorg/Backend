//! Painting the sidebar: the column (the way out, the scope's title, the
//! comb, the lens strip, the narrowing line, the rows) and the spine it
//! collapses to. Everything drawn was decided in the model; nothing here
//! chooses what a row says or does.

use super::Shelf;
use super::glyph::{self, Marked};
use super::hold::{self, Chip, Step};
use super::lens::{Counts, Lens};
use super::listing::{Head, Releases, StepDoes, StepOut, Title};
use super::row::{Do, Fold, Heading, Item, Mark, Row, Trailing};
use crate::navigation::Intent;
use crate::shell::focus::Zone;
use crate::shell::kit::{kind_mark, text};
use facet::icons::{self, IconSize, Kind, KindSize};
use facet::tokens::fluid::SideForm;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, Hsla, InteractiveElement, IntoElement, ParentElement,
    ScrollStrategy, SharedString, StatefulInteractiveElement, Styled, Transformation, div, px,
    radians, uniform_list,
};
use std::f32::consts::{FRAC_PI_2, PI};
use std::ops::Range;
use std::rc::Rc;

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
    fn take_keyboard(&self, cx: &mut Context<Self>) {
        let links = self.links.clone();
        cx.defer(move |cx| links.shell(cx, |shell, cx| shell.set_zone(Zone::Shelf, cx)));
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
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .pt(measure.space(Space::Roomy));
        if let Some(step) = &head.step_out {
            column = column.child(self.step_out_row(step, measure, palette, cx));
        }
        #[cfg(test)]
        self.upgrade.replace(None);
        if let Some(title) = &head.title {
            column = column.child(title_block(title, measure, palette));
            if let Some(releases) = &head.releases
                && !releases.list.is_empty()
            {
                for element in self.comb_block(releases, measure, palette) {
                    column = column.child(
                        div()
                            .px(gutter)
                            .pb(measure.space(Space::Roomy))
                            .child(element),
                    );
                }
            }
        }
        if !head.held.is_empty() {
            column = column.child(self.held_chips(&head.held, measure, palette, cx));
        }
        if let Some(counts) = head.counts {
            column = column
                .child(self.lens_strip(counts, measure, palette, cx))
                .child(self.narrow_line(measure, palette));
        }
        let count = self.rows.len();
        let row_measure = *measure;
        let list = uniform_list(
            "shelf-rows",
            count,
            cx.processor(move |shelf: &mut Self, range: Range<usize>, _window, cx| {
                shelf.render_rows(range, &row_measure, cx)
            }),
        )
        .size_full()
        .track_scroll(&self.scroll);
        column
            .child(
                facet::probe::scroll_scope(
                    "shelf-rows",
                    div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(list)
                        .children(self.sticky(measure, palette, cx)),
                ),
            )
            .child(facet::probe::scroll_probe(
                "shelf-rows",
                self.scroll.0.borrow().base_handle.clone(),
            ))
            .children(
                (!head.trail.is_empty())
                    .then(|| self.trail_block(&head.trail, measure, palette, cx)),
            )
            .into_any_element()
    }

    /// Sticky ancestors (VS Code's Explorer): in a long list, the chain of
    /// parents of the first visible row stays pinned to the top, so you always
    /// see which module and type you are inside.
    fn sticky(
        &self,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let height = measure.row() + measure.space(Space::Tight);
        let scrolled = -self.scroll.0.borrow().base_handle.offset().y;
        let mut first = 0usize;
        let mut top = px(0.0);
        while top + height <= scrolled && first < self.rows.len() {
            top += height;
            first += 1;
        }
        let Some(Row::Item(row)) = self.rows.get(first) else {
            return None;
        };
        let mut wanted = row.depth;
        let mut chain: Vec<(usize, &Item)> = Vec::new();
        for (index, line) in self.rows[..first].iter().enumerate().rev() {
            if wanted == 0 {
                break;
            }
            if let Row::Item(item) = line
                && item.depth < wanted
            {
                chain.push((index, item));
                wanted = item.depth;
            }
        }
        if chain.is_empty() {
            return None;
        }
        chain.reverse();
        let mut pinned = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bg(palette.g1)
            .border_b_1()
            .border_color(palette.line1.hsla());
        for (index, item) in chain {
            let indent =
                measure.space(Space::Roomy) + measure.space(Space::Gutter) * f32::from(item.depth);
            pinned = pinned.child(
                div()
                    .id(SharedString::from(format!("{}#sticky", item.key)))
                    .h(height)
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
                        shelf.scroll.scroll_to_item(index, ScrollStrategy::Top);
                        cx.notify();
                    })),
            );
        }
        Some(pinned.into_any_element())
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
                    .on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                        shelf.take_keyboard(cx);
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
            .flex_wrap()
            .items_center()
            .gap_x(measure.space(Space::Snug));
        for (index, step) in steps.iter().enumerate() {
            if index > 0 {
                places = places.child(text(ty::MONO_SMALL, measure, palette.ink3).child("‹"));
            }
            let route = step.route.clone();
            places = places.child(
                div()
                    .id(SharedString::from(format!("shelf-trail-{index}")))
                    .role(gpui::Role::Link)
                    .aria_label(step.label.clone())
                    .focusable()
                    .cursor_pointer()
                    .child(text(ty::MONO_SMALL, measure, palette.ink3).child(step.label.clone()))
                    .on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
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
            .on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                shelf.take_keyboard(cx);
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
            let tab = tab.on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                shelf.take_keyboard(cx);
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
    fn narrow_line(&self, measure: &Measure, palette: &Palette) -> AnyElement {
        let height = measure.row() + measure.space(Space::Tight);
        let line = div()
            .id("shelf-narrow")
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

    fn render_rows(
        &mut self,
        range: Range<usize>,
        measure: &Measure,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let palette = cx.facet().palette();
        let rows = Rc::clone(&self.rows);
        range
            .filter_map(|index| rows.get(index).map(|row| (index, row)))
            .map(|(index, row)| self.line(index, row, measure, palette, cx))
            .collect()
    }

    fn line(
        &mut self,
        index: usize,
        row: &Row,
        measure: &Measure,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = measure.row() + measure.space(Space::Tight);
        match row {
            Row::Item(item) => self.item(item, height, measure, palette, cx),
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
                .h(height)
                .w_full()
                .flex()
                .items_center()
                .px(measure.space(Space::Roomy))
                .child(
                    text(ty::CAPTION, measure, palette.ink3)
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
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
            .role(if item.does == Do::Nothing { gpui::Role::Label } else { gpui::Role::Button })
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
        if self.targets.is_focused(&item.key) {
            element = element.aria_active_descendant();
        }
        if item.does != Do::Nothing {
            element = element.focusable();
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
        if item.does != Do::Nothing {
            let (id, does, hoists) = (item.key.clone(), item.does.clone(), item.hoists.clone());
            // A row that opens a declaration hands its name's box to that
            // declaration's title (W-Page2's `title_key`), so the title grows
            // out of the row that was clicked. It is registered at the click,
            // not painted as a shared element, because the same declaration
            // can also be a door on the page, and one key may have only one
            // owner per frame. The row for the page you are on opens nothing.
            let opens = item.source.clone().filter(|_| !item.current);
            element =
                element.on_click(cx.listener(move |shelf, event: &ClickEvent, window, cx| {
                    shelf.take_keyboard(cx);
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
        self.targets
            .track(item.key.clone(), element)
            .into_any_element()
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
            .on_click(cx.listener(move |shelf, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                shelf.take_keyboard(cx);
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
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            .rows
            .iter()
            .filter_map(Row::item)
            .filter(|item| item.depth == 0 || item.current)
            .take(24)
        {
            let mut cell = div()
                .id(SharedString::from(format!("spine-{}", item.key)))
                .role(if item.does == Do::Nothing { gpui::Role::Label } else { gpui::Role::Button })
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
            if self.targets.is_focused(&item.key) {
                cell = cell.aria_active_descendant();
            }
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
            if item.does != Do::Nothing {
                cell = cell.focusable();
                let does = item.does.clone();
                let shelf = cx.weak_entity();
                cell = cell.on_click(move |_: &ClickEvent, _, cx| {
                    let _ = shelf.update(cx, |shelf, cx| shelf.perform(&does, cx));
                });
            }
            column = column.child(cell);
        }
        column.into_any_element()
    }
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
