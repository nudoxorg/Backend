//! The explore region: a real title, a real search field, and a card grid.
//! A card says what a package *is*, which a dense row cannot say in one line.
//! Every card mixes recorded facts with labelled samples, never silently.
//!
//! The design decision this region embodies is that browsing and scanning are
//! different acts and want different shapes. A reader who already knows the
//! name wants the dense list the omnibar gives them. A reader who is here to
//! *discover* has to compare candidates on things a one-line row has no room
//! for — what the package claims to be, whether anyone uses it, whether it is
//! still moving, what licence it carries — so this region draws a card and
//! spends the pixels. The grid is laid out by flex basis rather than by a
//! measured column count, so the same code gives three columns in a wide
//! reading column, two when a panel is open, and one on a narrow window,
//! without asking the window how big it is.
//!
//! Two honesty rules hold the region together. Cards are built for the loaded
//! page only and "Show more" widens the engine's own window, so the grid can
//! never claim more rows than the reply held. And the sort controls say when
//! they rank by a sampled number: an ordering the registry never published is
//! a useful convenience and a dishonest fact, and the difference is a word.

use super::home::ecosystem_language;
use super::workspace::Workspace;
use crate::motion::{Beat, entering_opacity, once};
use crate::store::document::Target;
use crate::store::dossier::{self, Card};
use crate::store::registry::{
    self, Listing, Loadable, Ordering, PackageRow, Query, RegistryStore, Standing,
};
use crate::theme::Theme;
use crate::theme::language::hue as language_hue;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::icon::{Icon, Logo};
use crate::ui::{button, chart, fault as fault_ui, icon, text};
use backend_library::RegistryEcosystem;
use backend_present::Shelf;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, ClickEvent, Context, Div, ElementId, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, Window, div, px,
};
use gpui_elements::editable_text::{EditableTextState, text_input};

/// Every ecosystem the engine's closed registry namespace admits.
const ECOSYSTEMS: [RegistryEcosystem; 7] = [
    RegistryEcosystem::Cargo,
    RegistryEcosystem::Npm,
    RegistryEcosystem::Pypi,
    RegistryEcosystem::Maven,
    RegistryEcosystem::Nuget,
    RegistryEcosystem::Golang,
    RegistryEcosystem::Cpp,
];

/// Preferred width of one card, which is what sets the column count.
const CARD: f32 = 268.0;

/// Narrowest a card is allowed to be squeezed before the grid drops a column.
const NARROW: f32 = 232.0;

/// Height of a card's description: two lines of secondary text, and no more.
///
/// The box is fixed rather than fitted so that a row of cards has one
/// baseline: a grid whose cards each chose their own height reads as a pile.
const BLURB: f32 = 36.0;

/// Height of the sparkline on a card.
const SPARK: u16 = 20;

/// How many skeleton cards a loading grid reserves.
const RESERVED: u8 = 6;

impl Workspace {
    /// Returns the explore region: heading, search, filters, and the grid.
    pub(super) fn browse_region(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        self.registry.update(cx, RegistryStore::ensure_page);
        let field = self.registry.update(cx, RegistryStore::field);
        let held = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        let (query, state, listings, order, more) = {
            let registry = self.registry.read(cx);
            (
                registry.query().clone(),
                registry.page().clone(),
                registry.listing(),
                registry.order(),
                registry.can_advance(),
            )
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Base))
            .child(Self::browse_title(theme))
            .child(Self::browse_field(theme, &field))
            .child(Self::scope_row(theme, query.ecosystem(), cx))
            .child(Self::order_row(theme, order, cx))
            .child(Self::browse_note(
                theme,
                &query,
                &state,
                listings.len(),
                order,
            ))
            .child(Self::browse_body(
                theme, &query, &state, &listings, &held, cx,
            ))
            .when(more, |region| region.child(Self::more_row(theme, cx)))
            .into_any_element()
    }

    /// Opens the home page with the explore region's first page already read.
    ///
    /// A preview scene has to stand where a keystroke would leave the window,
    /// and the catalog read is what the first frame of the explore region
    /// starts; asking for it here means the screenshot is of an answered grid
    /// rather than of six skeletons.
    #[cfg(feature = "preview")]
    pub(crate) fn preview_browse(&mut self, cx: &mut Context<Self>) {
        self.open_home(cx);
        self.registry.update(cx, RegistryStore::ensure_page);
    }

    /// Returns the region's own title, set as a title rather than as a label.
    fn browse_title(theme: &Theme) -> Div {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(text::heading(theme, TypeScale::Title).child("Explore the registry"))
            .child(text::text_at(theme, TypeScale::Body, Paint::TextDim).child(
                "Every package the local index holds records for. Search by name, narrow \
                     to one ecosystem, then put anything on the shelf to read its source.",
            ))
    }

    /// Returns the large search field, which teaches by example.
    fn browse_field(theme: &Theme, field: &Entity<EditableTextState>) -> Div {
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Room))
            .py(space(Space::Base))
            .rounded(radius(Radius::Medium))
            .bg(theme.paint(Paint::Panel))
            .border(hairline())
            .border_color(theme.paint(Paint::HairlineStrong))
            .child(icon::sized(theme, Icon::Search, 15.0, Paint::TextFaint))
            .child(
                div().flex_1().min_w(px(0.0)).child(
                    text_input("browse-search")
                        .state(field.downgrade())
                        .placeholder("serde, requests, @types/node, zod…")
                        .placeholder_color(theme.paint(Paint::TextFaint))
                        .selection_color(theme.paint(Paint::GiltWash))
                        .caret_color(theme.paint(Paint::Gilt))
                        .text_size(type_size(TypeScale::Body))
                        .text_color(theme.paint(Paint::TextStrong)),
                ),
            )
    }
}

/// The filter and sort controls.
impl Workspace {
    fn scope_row(theme: &Theme, current: Option<RegistryEcosystem>, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Tight))
            .child(Self::scope_toggle(theme, None, current, cx))
            .children(
                ECOSYSTEMS
                    .iter()
                    .map(|ecosystem| Self::scope_toggle(theme, Some(*ecosystem), current, cx)),
            )
    }

    /// Returns one ecosystem toggle, carrying that ecosystem's own logo.
    fn scope_toggle(
        theme: &Theme,
        ecosystem: Option<RegistryEcosystem>,
        current: Option<RegistryEcosystem>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chosen = ecosystem == current;
        let label = ecosystem.map_or("all", RegistryEcosystem::as_str);
        toggle(theme, format!("browse-scope-{label}"), chosen)
            .when_some(ecosystem, |chip, ecosystem| {
                chip.children(ecosystem_logo(theme, ecosystem, 12.0))
            })
            .child(label.to_owned())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.registry
                    .update(cx, |registry, cx| registry.scope_to(ecosystem, cx));
            }))
            .into_any_element()
    }

    fn order_row(theme: &Theme, current: Ordering, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space(Space::Tight))
            .child(text::faint(theme).flex_none().child("Sort by"))
            .children(Ordering::ALL.into_iter().map(|order| {
                toggle(
                    theme,
                    format!("browse-order-{}", order.label()),
                    order == current,
                )
                .child(order.label())
                .when(order.is_sampled(), |chip| {
                    chip.child(chart::sample_tag(theme))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.registry
                        .update(cx, |registry, cx| registry.order_by(order, cx));
                }))
                .into_any_element()
            }))
    }

    /// Returns the one line that says what the visible grid actually is.
    fn browse_note(
        theme: &Theme,
        query: &Query,
        state: &Loadable<Vec<PackageRow>>,
        shown: usize,
        order: Ordering,
    ) -> Div {
        let scope = query.ecosystem().map_or_else(
            || "every ecosystem".to_owned(),
            |ecosystem| ecosystem.as_str().to_owned(),
        );
        let needle = query.text().trim();
        let sentence = match state.ready() {
            None if state.is_pending() => format!("Reading the local index across {scope}…"),
            None => format!("The local index did not answer for {scope}."),
            Some(_) if needle.is_empty() => format!(
                "{shown} package(s) from the local index across {scope}, {}.",
                order.sentence()
            ),
            Some(_) => format!(
                "{shown} package(s) matching “{needle}” across {scope}, {}.",
                order.sentence()
            ),
        };
        text::faint(theme).child(sentence)
    }

    fn more_row(theme: &Theme, cx: &mut Context<Self>) -> Div {
        div().flex().child(
            button::button(theme, "browse-more", "Show more", button::Weight::Regular).on_click(
                cx.listener(|this, _, _, cx| {
                    this.registry.update(cx, RegistryStore::advance);
                }),
            ),
        )
    }
}

/// The grid and its four states.
impl Workspace {
    fn browse_body(
        theme: &Theme,
        query: &Query,
        state: &Loadable<Vec<PackageRow>>,
        listings: &[Listing],
        held: &Shelf,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match state {
            Loadable::Idle | Loadable::Loading => reserved_grid(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                let actions = Self::affordances(theme, "browse", fault, query.text(), cx);
                fault_ui::block(theme, fault, actions).into_any_element()
            }
            Loadable::Ready(_) if listings.is_empty() => {
                empty_state(theme, query).into_any_element()
            }
            Loadable::Ready(_) => Self::browse_grid(theme, listings, held, cx),
        }
    }

    /// Returns the card grid for exactly the rows the reply held.
    fn browse_grid(
        theme: &Theme,
        listings: &[Listing],
        held: &Shelf,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let reduced = theme.reduced_motion();
        let count = listings.len();
        div()
            .w_full()
            .flex()
            .flex_wrap()
            .gap(space(Space::Base))
            .children(listings.iter().enumerate().map(|(at, listing)| {
                let standing = registry::add_standing(held, listing.row().coordinate());
                card(theme, &entity, at, listing, standing)
            }))
            .with_animation(
                ElementId::Name(SharedString::from(format!("browse-grid-{count}"))),
                once(Beat::Reveal, reduced),
                |grid, delta| grid.opacity(entering_opacity(delta)),
            )
            .into_any_element()
    }
}

/// Returns one chip-shaped toggle in its chosen or unchosen state.
fn toggle(theme: &Theme, id: impl Into<SharedString>, chosen: bool) -> gpui::Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .px(space(Space::Snug))
        .py(px(3.0))
        .rounded(radius(Radius::Capsule))
        .border(hairline())
        .border_color(theme.paint(if chosen {
            Paint::GiltDim
        } else {
            Paint::Hairline
        }))
        .when(chosen, |chip| chip.bg(theme.paint(Paint::GiltWash)))
        .text_size(type_size(TypeScale::Tiny))
        .text_color(theme.paint(if chosen { Paint::Gilt } else { Paint::TextDim }))
        .cursor_pointer()
        .hover(|style| style.bg(theme.paint(Paint::Hover)))
}

/// Returns one ecosystem's logo, drawn in that ecosystem's own hue.
pub(super) fn ecosystem_logo(
    theme: &Theme,
    ecosystem: RegistryEcosystem,
    side: f32,
) -> Option<gpui::Svg> {
    let language = ecosystem_language(ecosystem);
    let mark = Logo::of(language)?;
    Some(icon::logo(
        mark,
        side,
        theme.on_plane(language_hue(language)),
    ))
}

/// Returns one browse card: what it is, whether it is used, and one action.
fn card(
    theme: &Theme,
    entity: &Entity<Workspace>,
    at: usize,
    listing: &Listing,
    standing: Standing,
) -> AnyElement {
    div()
        .flex_basis(px(CARD))
        .flex_grow(1.0)
        .min_w(px(NARROW))
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .p(space(Space::Base))
        .rounded(radius(Radius::Medium))
        .bg(theme.paint(Paint::Panel))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .child(card_face(theme, entity, at, listing))
        .child(card_foot(theme, entity, at, listing, standing))
        .into_any_element()
}

/// Returns the part of a card that opens the package it names.
fn card_face(
    theme: &Theme,
    entity: &Entity<Workspace>,
    at: usize,
    listing: &Listing,
) -> gpui::Stateful<Div> {
    let row = listing.row();
    let card = listing.card();
    let entity = entity.clone();
    let coordinate = row.coordinate().to_owned();
    div()
        .id(ElementId::Name(SharedString::from(format!(
            "browse-card-{at}"
        ))))
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .cursor_pointer()
        .on_click(move |event: &ClickEvent, _window: &mut Window, cx| {
            let target = if event.modifiers().alt {
                Target::Background
            } else {
                Target::Here
            };
            let coordinate = coordinate.clone();
            entity.update(cx, |workspace, cx| {
                workspace.open_package(coordinate, target, cx);
            });
        })
        .child(card_head(theme, row))
        .child(
            text::dim(theme)
                .w_full()
                .h(px(BLURB))
                .overflow_hidden()
                .child(card.blurb().to_owned()),
        )
        .child(keyword_row(theme, card))
        .child(usage_row(theme, row.ecosystem(), card))
}

/// Returns a card's first line: logo, name, and pinned version.
fn card_head(theme: &Theme, row: &PackageRow) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .children(ecosystem_logo(theme, row.ecosystem(), 14.0))
        .child(
            text::single_line(
                text::heading(theme, TypeScale::Interface)
                    .flex_1()
                    .min_w(px(0.0)),
            )
            .child(row.name().to_owned()),
        )
        .child(
            text::faint(theme)
                .flex_none()
                .font_family(theme.specimen())
                .child(row.version().to_owned()),
        )
}

/// Returns the keyword chips, or the one line that stands in for them.
fn keyword_row(theme: &Theme, card: &Card) -> Div {
    let keywords = card.keywords();
    if keywords.is_empty() {
        return text::faint(theme).child("no keywords published");
    }
    div()
        .flex()
        .flex_wrap()
        .gap(space(Space::Tight))
        .children(keywords.iter().map(|keyword| {
            div()
                .flex_none()
                .px(px(5.0))
                .py(px(1.0))
                .rounded(radius(Radius::Hair))
                .bg(theme.paint(Paint::Hover))
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(Paint::TextDim))
                .child(keyword.clone())
        }))
}

/// Returns the published download count. A registry row may omit telemetry;
/// in that case the card states that absence instead of drawing a zero.
fn usage_row(theme: &Theme, ecosystem: RegistryEcosystem, card: &Card) -> Div {
    let hue = language_hue(ecosystem_language(ecosystem));
    let downloads = card.downloads();
    let counts = downloads.counts();
    let count_label = downloads
        .total()
        .map_or_else(|| "downloads not reported".to_owned(), dossier::tally_label);
    div()
        .flex()
        .items_end()
        .gap(space(Space::Snug))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(chart::sparkline(theme, hue, &counts, SPARK)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(1.0))
                .child(
                    text::text_at(theme, TypeScale::Tiny, Paint::Text)
                        .font_weight(FontWeight::MEDIUM)
                        .child(count_label),
                )
                .when(card.is_sampled(), |column| {
                    column.child(chart::sample_tag(theme))
                }),
        )
}

/// Returns a card's last line: licence and size, then the one action.
fn card_foot(
    theme: &Theme,
    entity: &Entity<Workspace>,
    at: usize,
    listing: &Listing,
    standing: Standing,
) -> Div {
    let row = listing.row();
    let license = card_license(theme, listing);
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .pt(space(Space::Tight))
        .border_t(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .child(license)
        .child(div().flex_1())
        .child(add_control(theme, entity, at, row, standing))
}

/// Returns the licence and size line, stating the licence's absence honestly.
fn card_license(theme: &Theme, listing: &Listing) -> Div {
    let license = listing
        .card()
        .license()
        .map_or_else(|| "no licence published".to_owned(), str::to_owned);
    let released = listing.card().released().map_or_else(
        || "date not published".to_owned(),
        |stamp| format!("updated {}", stamp.iso()),
    );
    text::single_line(text::faint(theme))
        .flex_1()
        .min_w(px(0.0))
        .child(format!("{license} · {} · {released}", listing.row().size()))
}

/// Returns the add affordance, or the statement that replaces it.
fn add_control(
    theme: &Theme,
    entity: &Entity<Workspace>,
    at: usize,
    row: &PackageRow,
    standing: Standing,
) -> AnyElement {
    if !standing.is_actionable() {
        let role = if standing == Standing::OnShelf {
            Paint::Ok
        } else {
            Paint::Caution
        };
        return div()
            .flex_none()
            .text_size(type_size(TypeScale::Micro))
            .text_color(theme.paint(role))
            .child(standing.label())
            .into_any_element();
    }
    let entity = entity.clone();
    let coordinate = row.coordinate().to_owned();
    button::button(
        theme,
        format!("browse-add-{at}"),
        standing.label(),
        button::Weight::Quiet,
    )
    .on_click(move |_, _window: &mut Window, cx| {
        let coordinate = coordinate.clone();
        entity.update(cx, |workspace, cx| {
            workspace.index_project(coordinate, cx);
        });
    })
    .into_any_element()
}

/// Returns the grid a reader sees while the first page is on the wire.
///
/// The skeletons are card-shaped rather than line-shaped so the page does not
/// reflow when the reply lands: the geometry a loading grid reserves is the
/// geometry the answered grid occupies.
fn reserved_grid(theme: &Theme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .gap(space(Space::Base))
        .children((0..RESERVED).map(|at| {
            div()
                .flex_basis(px(CARD))
                .flex_grow(1.0)
                .min_w(px(NARROW))
                .flex()
                .flex_col()
                .gap(space(Space::Snug))
                .p(space(Space::Base))
                .rounded(radius(Radius::Medium))
                .border(hairline())
                .border_color(theme.paint(Paint::Hairline))
                .child(bone(theme, 0.55, 14.0))
                .child(bone(theme, 0.95, 10.0))
                .child(bone(theme, 0.70, 10.0))
                .child(bone(theme, 1.00, f32::from(SPARK)))
                .child(bone(theme, 0.40, 10.0))
                .with_animation(
                    ElementId::Name(SharedString::from(format!("browse-bone-{at}"))),
                    once(Beat::Reveal, theme.reduced_motion()),
                    |card, delta| card.opacity(entering_opacity(delta)),
                )
        }))
}

/// Returns one reserved bar at a fraction of the card's width.
fn bone(theme: &Theme, part: f32, height: f32) -> Div {
    div()
        .w(gpui::relative(part))
        .h(px(height))
        .rounded(radius(Radius::Hair))
        .bg(theme.paint(Paint::Hover))
}

/// Returns the empty state, which says exactly what was looked for.
fn empty_state(theme: &Theme, query: &Query) -> Div {
    let needle = query.text().trim();
    let scope = query.ecosystem().map_or_else(
        || "any ecosystem".to_owned(),
        |ecosystem| format!("the {} registry", ecosystem.as_str()),
    );
    let sentence = if needle.is_empty() {
        format!("The local index holds no package from {scope}.")
    } else {
        format!("Nothing in {scope} matches \u{201c}{needle}\u{201d}.")
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .p(space(Space::Room))
        .rounded(radius(Radius::Medium))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .child(text::label(theme).child(sentence))
        .child(text::faint(theme).child(
            "Narrow or widen the ecosystem filter, or add the package by its exact \
             coordinate — a pinned package URL such as pkg:npm/zod@3.23.8.",
        ))
}
