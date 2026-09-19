//! The home page: what the reader pinned, where they have been, and the registry.
//! One reading column, three regions, and no card chrome in the first two.
//! The shelf is not repeated here; the projects panel already is the shelf.
//!
//! The order is the order of intent. Pinned first, because a pin is the
//! reader saying "this is what I come here for"; recent second, because the
//! next most likely thing is the last thing; the registry third, as a large
//! heading, because browsing is what a reader does when neither of the first
//! two answers. A first run has none of these, so it gets the welcome instead.
//!
//! The registry region is [`super::browse`]'s; this page only decides where
//! it sits. The helpers at the bottom — the ecosystem tag, the section head,
//! the reserved rows — are shared with the package page so that a section on
//! one page and a section on the other are drawn by the same hand.

use super::workspace::Workspace;
use crate::presentation::project;
use crate::store::document::Target;
use crate::store::marks::Recent;
use crate::store::registry::Spelling;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::bar;
use crate::ui::icon::Icon;
use crate::ui::tip::{Tip, Tipped as _};
use crate::ui::{button, glyph, text};
use backend_library::RegistryEcosystem;
use backend_present::{Identity, Language, Shelf, ShelfEntry};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

/// Width of one pinned tile, in pixels.
const TILE: f32 = 196.0;

/// How many recent subjects the page lists.
const RECENT_SHOWN: usize = 10;

impl Workspace {
    /// Returns the home page.
    pub(super) fn home_page(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let held = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        let pinned = self.pinned_region(theme, &held, cx);
        let recent = self.recent_region(theme, cx);
        let browse = self.browse_region(theme, cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Bay))
            .when(held.is_empty(), |page| page.child(Self::welcome_region(theme, cx)))
            .when_some(pinned, ParentElement::child)
            .when_some(recent, ParentElement::child)
            .child(browse)
            .into_any_element()
    }

    /// Returns the pinned tiles, when anything is pinned.
    fn pinned_region(&mut self, theme: &Theme, held: &Shelf, cx: &mut Context<Self>) -> Option<Div> {
        let pinned = self.shell.read(cx).marks().pinned().to_vec();
        if pinned.is_empty() {
            return None;
        }
        let tiles: Vec<AnyElement> = pinned
            .iter()
            .enumerate()
            .map(|(at, coordinate)| match project::find(held, coordinate) {
                Some(entry) => Self::project_tile(theme, at, entry, cx),
                None => Self::package_tile(theme, at, coordinate, cx),
            })
            .collect();
        Some(head_and_body(
            theme,
            "Pinned",
            div()
                .flex()
                .flex_wrap()
                .gap(space(Space::Snug))
                .children(tiles)
                .into_any_element(),
        ))
    }

    fn project_tile(theme: &Theme, at: usize, entry: &ShelfEntry, cx: &mut Context<Self>) -> AnyElement {
        let coordinate = entry.identity().coordinate().as_str().to_owned();
        let standing = project::standing(entry);
        let opened = coordinate.clone();
        let unpinned = coordinate.clone();
        let group = SharedString::from(format!("pin-tile-{at}"));
        tile_shell(theme, format!("pinned-{at}"))
            .group(group.clone())
            .on_click(cx.listener(move |this, _, _, cx| this.open_project(opened.clone(), cx)))
            .tip(Tip::new("Open project").detail(project::summary(entry)).value(coordinate))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Tight))
                    .min_w(px(0.0))
                    .when(standing != project::Standing::Readable, |row| {
                        row.child(glyph::standing_mark(theme, standing))
                    })
                    .child(
                        text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                            .flex_1()
                            .min_w(px(0.0))
                            .child(entry.identity().name().to_owned()),
                    )
                    .child(unpin_button(theme, at, group, cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_pin(&unpinned, cx);
                    }))),
            )
            .child(bar::language_bar(
                theme,
                format!("pin-bar-{at}"),
                entry.languages(),
                entry.declarations().get(),
                bar::Motion::of(standing),
            ))
            .child(text::single_line(text::faint(theme)).child(project::badge(entry.identity())))
            .into_any_element()
    }

    /// Returns a tile for a pinned package that is not on the shelf.
    fn package_tile(theme: &Theme, at: usize, coordinate: &str, cx: &mut Context<Self>) -> AnyElement {
        let spelling = Spelling::of(coordinate);
        let opened = coordinate.to_owned();
        let unpinned = coordinate.to_owned();
        let group = SharedString::from(format!("pin-tile-{at}"));
        let language = spelling
            .ecosystem()
            .map_or(Language::Unknown, ecosystem_language);
        tile_shell(theme, format!("pinned-{at}"))
            .group(group.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_package(opened.clone(), Target::Child, cx);
            }))
            .tip(Tip::new("Open package").value(coordinate.to_owned()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Tight))
                    .min_w(px(0.0))
                    .child(glyph::language_tag(theme, language))
                    .child(
                        text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                            .flex_1()
                            .min_w(px(0.0))
                            .child(spelling.name().to_owned()),
                    )
                    .child(unpin_button(theme, at, group, cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_pin(&unpinned, cx);
                    }))),
            )
            .child(
                text::single_line(text::faint(theme))
                    .font_family(theme.specimen())
                    .child(spelling.version().to_owned()),
            )
            .child(text::single_line(text::faint(theme)).child("not on the shelf"))
            .into_any_element()
    }

    /// Returns the recently opened subjects, when there are any.
    fn recent_region(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let recent: Vec<Recent> = self
            .shell
            .read(cx)
            .marks()
            .recent()
            .iter()
            .take(RECENT_SHOWN)
            .cloned()
            .collect();
        if recent.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = recent
            .iter()
            .enumerate()
            .map(|(at, entry)| self.recent_row(theme, at, entry, cx))
            .collect();
        Some(head_and_body(
            theme,
            "Recently viewed",
            div().flex().flex_col().gap(px(1.0)).children(rows).into_any_element(),
        ))
    }

    fn recent_row(&self, theme: &Theme, at: usize, entry: &Recent, cx: &mut Context<Self>) -> AnyElement {
        let identity = Identity::parse(entry.coordinate());
        let (mark, title, place) = self.recent_facts(theme, entry, &identity, cx);
        let opened = entry.clone();
        div()
            .id(ElementId::Name(SharedString::from(format!("recent-{at}"))))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Snug))
            .py(px(3.0))
            .rounded(radius(Radius::Small))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .tip(Tip::new(format!("Open {}", entry.noun())).value(entry.coordinate().to_owned()))
            .on_click(cx.listener(move |this, _, _, cx| this.open_recent(&opened, cx)))
            .child(mark)
            .child(
                text::single_line(text::label(theme).font_weight(FontWeight::MEDIUM))
                    .flex_none()
                    .max_w(px(300.0))
                    .child(title),
            )
            .child(
                text::single_line(text::faint(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .child(place),
            )
            .into_any_element()
    }

    /// Returns the mark, the title, and the one-line place for a recent row.
    fn recent_facts(
        &self,
        theme: &Theme,
        entry: &Recent,
        identity: &Identity,
        cx: &Context<Self>,
    ) -> (AnyElement, String, String) {
        match entry {
            Recent::Project { coordinate } => (
                glyph::package_tile(theme, false).into_any_element(),
                identity.name().to_owned(),
                project::badge(identity) + " · " + &crate::presentation::crumb::elide_middle(coordinate, 48),
            ),
            Recent::Package { coordinate } => {
                let spelling = Spelling::of(coordinate);
                let language = spelling.ecosystem().map_or(Language::Unknown, ecosystem_language);
                (
                    glyph::language_tag(theme, language).into_any_element(),
                    spelling.name().to_owned(),
                    format!("{} · registry", spelling.version()),
                )
            }
            Recent::Declaration { coordinate } => {
                let index = self.index.read(cx);
                let kind = index
                    .symbol_for(coordinate)
                    .and_then(|symbol| index.entry(symbol))
                    .and_then(crate::store::index::Entry::kind);
                let within = identity.project().cloned();
                (
                    glyph::kind_tile(theme, kind, false).into_any_element(),
                    identity.name().to_owned(),
                    identity.trail_within(within.as_ref()),
                )
            }
        }
    }

    /// Returns the first-run welcome, folded into the home page itself.
    fn welcome_region(theme: &Theme, cx: &mut Context<Self>) -> Div {
        head_and_body(
            theme,
            "Start here",
            div()
                .flex()
                .flex_col()
                .gap(space(Space::Base))
                .child(text::body(theme).child(
                    "Add a folder or a package and Nudox compiles it, indexes every \
                     declaration in it, and makes all of them readable and linked.",
                ))
                .child(Self::home_actions(theme, cx))
                .child(Self::home_examples(theme, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(Space::Snug))
                        .child(button::key_hint(theme, &super::keys::GO_HOME.label()))
                        .child(text::faint(theme).child("returns to this page")),
                )
                .into_any_element(),
        )
    }

    fn home_actions(theme: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Snug))
            .child(
                button::button(theme, "home-folder", "Add a project…", button::Weight::Primary)
                    .on_click(cx.listener(|this, _, window: &mut Window, cx| {
                        this.begin_add(window, cx);
                    })),
            )
            .child(
                button::button(theme, "home-palette", "Open the palette", button::Weight::Regular)
                    .on_click(cx.listener(|this, _, window: &mut Window, cx| {
                        this.set_field(">".to_owned(), cx);
                        this.focus_field(window, cx);
                    })),
            )
    }

    fn home_examples(theme: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(text::faint(theme).child("Or index one of these:"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(Space::Tight))
                    .children(super::library::EXAMPLES.iter().enumerate().map(
                        |(at, example)| {
                            coordinate_chip(theme, format!("home-example-{at}"), example).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.index_project((*example).to_owned(), cx);
                                }),
                            )
                        },
                    )),
            )
    }
}

/// Returns the small × that unpins a tile, visible while the tile is hovered.
fn unpin_button(
    theme: &Theme,
    at: usize,
    group: SharedString,
    listener: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Div {
    div()
        .flex_none()
        .opacity(0.0)
        .group_hover(group, |style| style.opacity(1.0))
        .child(
            button::icon_button(theme, format!("unpin-{at}"), Icon::Close)
                .tip(Tip::new("Unpin"))
                .on_click(listener),
        )
}


/// Returns the language whose hue one ecosystem is drawn in.
pub(super) const fn ecosystem_language(ecosystem: RegistryEcosystem) -> Language {
    match ecosystem {
        RegistryEcosystem::Cargo => Language::Rust,
        RegistryEcosystem::Npm => Language::TypeScript,
        RegistryEcosystem::Pypi => Language::Python,
        RegistryEcosystem::Maven => Language::Java,
        RegistryEcosystem::Nuget => Language::CSharp,
        RegistryEcosystem::Golang => Language::Go,
        RegistryEcosystem::Cpp => Language::Cxx,
    }
}

/// Returns one hairline section head with its rule, as every page draws it.
pub(super) fn section_head(theme: &Theme, title: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            text::faint(theme)
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_ascii_uppercase()),
        )
        .child(div().flex_1().h(hairline()).bg(theme.paint(Paint::Hairline)))
}

/// Returns one section: a hairline head above its body.
pub(super) fn head_and_body(theme: &Theme, title: &str, body: AnyElement) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(section_head(theme, title))
        .child(body)
}

/// Returns a monospace chip holding one exact coordinate.
pub(super) fn coordinate_chip(
    theme: &Theme,
    id: impl Into<SharedString>,
    text: &str,
) -> gpui::Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .px(space(Space::Snug))
        .py(px(3.0))
        .rounded(radius(Radius::Hair))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .text_color(theme.paint(Paint::TextDim))
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme.paint(Paint::Hover))
                .text_color(theme.paint(Paint::Gilt))
        })
        .child(text.to_owned())
}


/// Returns one reserved bar of an exact size.
pub(super) fn skeleton(theme: &Theme, width: f32, height: f32) -> Div {
    div()
        .w(px(width))
        .max_w(gpui::relative(1.0))
        .h(px(height))
        .rounded(radius(Radius::Hair))
        .bg(theme.paint(Paint::Hover))
}


fn tile_shell(theme: &Theme, id: impl Into<SharedString>) -> gpui::Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .w(px(TILE))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(space(Space::Snug))
        .rounded(radius(Radius::Small))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .cursor_pointer()
        .hover(|style| style.bg(theme.paint(Paint::Hover)))
}


