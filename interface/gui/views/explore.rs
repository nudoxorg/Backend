//! Defines the explore sheet for `interface-gui`.
//! This module owns the index-search rows, the coverage chip, and the package-profile block.
//! Its narrow surface keeps every catalogue refusal a typed block the reader can act on.
//! Selecting an index row opens the profile block, whose version rows seed the add flow.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, Styled, div, prelude::*};
use interface_library::{
    ExploreCoverage, ExploreError, ExplorePackageName, ExploreQuery, ExploreUnavailable,
    IndexHitRow, PackageProfile, PackageVersionRow, PackageVersionRows, checksum_hex,
};

use crate::app::Workspace;
use crate::store::explore::ExploreSlot;
use crate::theme::{Radius, Role, Space, Status, Theme};
use crate::ui::{self, hsla};

/// The height of one index or version row.
const ROW_HEIGHT: f32 = 36.0;

/// How many rows the loading geometry reserves, so the sheet does not jump when rows land.
const RESERVED_ROWS: u16 = 4;

/// The label the coverage chip carries, in the lane chips' own naming style.
const INDEX_CHIP_LABEL: &str = "~index";

/// The teaching line an empty shelf's idle explore sheet opens with.
const IDLE_TEACHING: &str = "#search the index · add any version with one click";

/// The second teaching line: the one keystroke that carries a coordinate into the add flow.
const PASTE_TEACHING: &str = "cmd-shift-a pastes a coordinate from anywhere";

/// The honest line under a coverage chip that could not search anything.
const SYNC_TEACHING: &str = "the index is empty until a registry sync runs";

/// The dense line the loading slot spells while the engine is on its way.
const LOADING_LINE: &str = "searching the index…";

/// The dense line a package block spells when the catalogue retained no version rows.
const NO_VERSIONS_LINE: &str = "no versions";

/// How many checksum bytes one version row spells, as their lower-case hex.
const CHECKSUM_ABBREVIATION_BYTES: usize = 8;

/// The hex characters that spell [`CHECKSUM_ABBREVIATION_BYTES`] bytes.
const CHECKSUM_ABBREVIATION_HEX: usize = CHECKSUM_ABBREVIATION_BYTES * 2;

/// The word marking a version the registry snapshot no longer offers.
const YANKED_MARK: &str = "yanked";

/// The word the add affordance carries on every version row.
const ADD_MARK: &str = "add";

/// The height the loading geometry reserves, in pixels.
fn reserved_height() -> f32 {
    ROW_HEIGHT * f32::from(RESERVED_ROWS)
}

/// Draws the explore sheet while the omnibar is in index mode: results, loading geometry, a
/// typed refusal, or the empty shelf's teaching.
pub(crate) fn sheet(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = *workspace.theme();
    let Some(slot) = workspace.explore().index_page() else {
        return idle_sheet(&theme, workspace);
    };
    let sheet: AnyElement = match slot {
        ExploreSlot::Loading => loading_sheet(&theme),
        ExploreSlot::Ready(page) => {
            let mut sheet = div()
                .id("explore")
                .flex()
                .flex_col()
                .border_b_1()
                .border_color(hsla(theme.palette().divider()))
                .bg(hsla(theme.palette().panel()))
                .child(coverage_row(&theme, &page.coverage, page.hits.len()))
                .child(rows(workspace, &page.hits, cx));
            if let Some(profile) = profile_block(workspace, cx) {
                sheet = sheet.child(profile);
            }
            sheet
                .child(footer(&theme, page.hits.len()))
                .into_any_element()
        }
        ExploreSlot::Failed(error) => div()
            .id("explore")
            .flex()
            .flex_col()
            .px(gpui::px(theme.pixels(Space::Base.rems())))
            .py(gpui::px(theme.pixels(Space::Tight.rems())))
            .border_b_1()
            .border_color(hsla(theme.palette().divider()))
            .bg(hsla(theme.palette().panel()))
            .child(explore_fault(&theme, error))
            .into_any_element(),
    };
    sheet
}

/// The idle sheet: nothing searched yet, so an empty shelf's sheet teaches instead.
fn idle_sheet(theme: &Theme, workspace: &Workspace) -> AnyElement {
    if !workspace.store().rows().is_empty() {
        return div().into_any_element();
    }
    div()
        .id("explore")
        .flex()
        .flex_col()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .border_b_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(ui::text_low(theme, Role::Body, IDLE_TEACHING))
        .child(ui::text_low(theme, Role::Dense, PASTE_TEACHING))
        .into_any_element()
}

/// The loading sheet: the geometry the rows will occupy, reserved before they land.
fn loading_sheet(theme: &Theme) -> AnyElement {
    div()
        .id("explore")
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(hsla(theme.palette().divider()))
        .bg(hsla(theme.palette().panel()))
        .child(
            ui::with_role(div(), theme, Role::Dense)
                .px(gpui::px(theme.pixels(Space::Base.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .text_color(hsla(theme.palette().text_low()))
                .child(LOADING_LINE.to_owned()),
        )
        .h(gpui::px(reserved_height()))
        .into_any_element()
}

/// The coverage line: one lane-style chip for how much of the index the search consulted.
fn coverage_row(theme: &Theme, coverage: &ExploreCoverage, hits: usize) -> AnyElement {
    let appearance = theme.appearance();
    let (glyph, note, status, ran) = match *coverage {
        ExploreCoverage::Complete => ("✓", format!("{hits} · complete"), Status::Ok, true),
        ExploreCoverage::Partial { searched, total } => (
            "◐",
            format!("searched {searched} of {total}"),
            Status::Warn,
            true,
        ),
        ExploreCoverage::Unavailable { reason } => {
            let note = match reason {
                ExploreUnavailable::EmptyIndex => "empty-index".to_owned(),
                ExploreUnavailable::StoreFault { slug } => slug.to_owned(),
                ExploreUnavailable::CatalogAbsent => "catalog-absent".to_owned(),
            };
            ("✗", note, Status::Danger, false)
        }
    };
    let mut row = div()
        .id("coverage")
        .flex()
        .flex_wrap()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .child(
            ui::with_role(div(), theme, Role::Dense)
                .flex()
                .gap(gpui::px(theme.pixels(Space::Hair.rems())))
                .px(gpui::px(theme.pixels(Space::Tight.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
                .bg(hsla(status.ground(appearance)))
                .text_color(hsla(status.color(appearance)))
                .child(format!("{INDEX_CHIP_LABEL} {glyph} {note}")),
        );
    if !ran {
        row = row.child(ui::text_low(theme, Role::Dense, SYNC_TEACHING));
    }
    row.into_any_element()
}

/// The index rows: document emphasized, matched snippet, and a dense dim score.
fn rows(
    workspace: &Workspace,
    hits: &[IndexHitRow],
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *workspace.theme();
    let selected = workspace
        .search()
        .index_cursor()
        .min(hits.len().saturating_sub(1));
    let mut list = div().id("index-rows").flex().flex_col();
    for (index, row) in hits.iter().enumerate() {
        let ground = if index == selected {
            theme.palette().element_active()
        } else {
            theme.palette().panel()
        };
        let score = row.score.0.to_string();
        list = list.child(
            div()
                .id(("index-row", index))
                .h(gpui::px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .px(gpui::px(theme.pixels(Space::Base.rems())))
                .bg(hsla(ground))
                .cursor_pointer()
                .on_click(cx.listener(move |workspace, _: &gpui::ClickEvent, _window, cx| {
                    workspace.search_mut().select_index(index);
                    if let Some(name) = selected_package(workspace) {
                        workspace.package_profile(name);
                    }
                    cx.notify();
                }))
                .child(
                    ui::with_role(div(), &theme, Role::Ui)
                        .flex_shrink(0.0)
                        .text_color(hsla(theme.palette().text()))
                        .child(row.document.as_str().to_owned()),
                )
                .child(
                    ui::text_low(&theme, Role::Dense, &row.matched)
                        .flex_1()
                        .min_w_0()
                        .truncate(),
                )
                .child(
                    ui::with_role(div(), &theme, Role::Dense)
                        .flex_shrink(0.0)
                        .text_color(hsla(theme.palette().text_inert()))
                        .child(score),
                ),
        );
    }
    list.into_any_element()
}

/// What the profile block has to draw for the selected package.
#[derive(Debug)]
enum ProfileView<'a> {
    /// The command is on its way.
    Loading,
    /// The catalogue refused, with its own cause retained.
    Failed(&'a ExploreError),
    /// The profile answered: latest version and full history.
    Profile(&'a PackageProfile),
    /// Only the version rows answered, so the block draws from those.
    Versions(&'a PackageVersionRows),
}

/// Resolves what the profile block draws, preferring the profile slot and falling back to the
/// version rows the package-versions command folded.
fn profile_view<'a>(
    workspace: &'a Workspace,
    name: &ExplorePackageName,
) -> Option<ProfileView<'a>> {
    match (workspace.explore().profile(name), workspace.explore().versions(name)) {
        (Some(ExploreSlot::Ready(profile)), _) => Some(ProfileView::Profile(profile)),
        (_, Some(ExploreSlot::Ready(rows))) => Some(ProfileView::Versions(rows)),
        (Some(ExploreSlot::Failed(error)), _)
        | (_, Some(ExploreSlot::Failed(error))) => Some(ProfileView::Failed(error)),
        (Some(ExploreSlot::Loading), _) | (_, Some(ExploreSlot::Loading)) => {
            Some(ProfileView::Loading)
        }
        (None, None) => None,
    }
}

/// The profile block beneath the rows, when the selected row has a slot to show.
fn profile_block(workspace: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let theme = *workspace.theme();
    let name = selected_package(workspace)?;
    let view = profile_view(workspace, &name)?;
    let mut block = div()
        .id("profile")
        .flex()
        .flex_col()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .border_t_1()
        .border_color(hsla(theme.palette().divider()))
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .child(
            ui::with_role(div(), &theme, Role::Title)
                .text_color(hsla(theme.palette().text()))
                .child(name.as_str().to_owned()),
        );
    match view {
        ProfileView::Profile(profile) => {
            if let Some(latest) = &profile.latest {
                block = block.child(latest_row(&theme, latest));
            }
            block = version_rows(&theme, &name, profile.versions.rows.as_ref(), block, cx);
        }
        ProfileView::Versions(loaded) => {
            block = version_rows(&theme, &name, loaded.rows.as_ref(), block, cx);
        }
        ProfileView::Failed(error) => block = block.child(explore_fault(&theme, error)),
        ProfileView::Loading => {
            block = block.child(
                ui::with_role(div(), &theme, Role::Dense)
                    .h(gpui::px(reserved_height()))
                    .text_color(hsla(theme.palette().text_inert()))
                    .child(LOADING_LINE.to_owned()),
            );
        }
    }
    Some(block.into_any_element())
}

/// Appends every version row, or the honest line a package without rows spells.
fn version_rows(
    theme: &Theme,
    name: &ExplorePackageName,
    rows: &[PackageVersionRow],
    mut block: gpui::Stateful<gpui::Div>,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<gpui::Div> {
    if rows.is_empty() {
        return block.child(ui::text_low(theme, Role::Dense, NO_VERSIONS_LINE));
    }
    let appearance = theme.appearance();
    for (index, row) in rows.iter().enumerate() {
        let yanked = !row.active.is_active();
        let cycle = row.cycle.get().to_string();
        let seed = format!("cargo:{}@{}", name.as_str(), row.version.as_str());
        let mut line = div()
            .id(("version-row", index))
            .h(gpui::px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(gpui::px(theme.pixels(Space::Tight.rems())));
        let spelling = ui::with_role(div(), theme, Role::Ui).child(row.version.as_str().to_owned());
        line = if yanked {
            line.child(
                spelling
                    .line_through()
                    .text_color(hsla(theme.palette().text_inert())),
            )
        } else {
            line.child(spelling.text_color(hsla(theme.palette().text())))
        };
        line = line
            .child(checksum_text(theme, row))
            .child(
                ui::with_role(div(), theme, Role::Dense)
                    .text_color(hsla(theme.palette().text_inert()))
                    .child(format!("cycle {cycle}")),
            )
            .children(yanked.then(|| {
                ui::with_role(div(), theme, Role::Dense)
                    .px(gpui::px(theme.pixels(Space::Tight.rems())))
                    .py(gpui::px(theme.pixels(Space::Hair.rems())))
                    .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
                    .bg(hsla(Status::Warn.ground(appearance)))
                    .text_color(hsla(Status::Warn.color(appearance)))
                    .child(YANKED_MARK.to_owned())
            }))
            .child(
                ui::button(
                    theme,
                    ADD_MARK,
                    false,
                    cx.listener(move |workspace, _: &gpui::ClickEvent, window, cx| {
                        let seed = seed.clone();
                        workspace.quick_add(&seed, window, cx);
                        cx.notify();
                    }),
                )
                .flex_shrink(0.0),
            );
        block = block.child(line);
    }
    block
}

/// The latest-version line: the spelling at full emphasis, its short checksum beside it.
fn latest_row(theme: &Theme, latest: &PackageVersionRow) -> AnyElement {
    div()
        .id("latest")
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Tight.rems())))
        .child(
            ui::with_role(div(), theme, Role::Ui)
                .text_color(hsla(theme.palette().text()))
                .child(format!("latest {}", latest.version.as_str())),
        )
        .child(checksum_text(theme, latest))
        .into_any_element()
}

/// The checksum as the row's short hex: the first bytes, spelled lower-case.
fn checksum_text(theme: &Theme, row: &PackageVersionRow) -> gpui::Div {
    let hex = checksum_hex(row.checksum);
    let short = hex.get(..CHECKSUM_ABBREVIATION_HEX).unwrap_or_default();
    ui::text_low(theme, Role::Mono, short)
}

/// One explore refusal drawn as a typed inline block, in the fault block's own grammar.
fn explore_fault(theme: &Theme, error: &ExploreError) -> AnyElement {
    ui::with_role(div(), theme, Role::Mono)
        .text_color(hsla(Status::Danger.color(theme.appearance())))
        .px(gpui::px(theme.pixels(Space::Snug.rems())))
        .py(gpui::px(theme.pixels(Space::Tight.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Control.rems())))
        .bg(hsla(Status::Danger.ground(theme.appearance())))
        .child(fault_line(error))
        .into_any_element()
}

/// The lines one explore refusal spells, in the words the catalogue itself uses.
fn fault_line(error: &ExploreError) -> String {
    match error {
        ExploreError::QueryTooLong { observed, maximum } => {
            format!("query-too-long · {observed} bytes · {maximum} admitted")
        }
        ExploreError::IndexStore { detail } => format!("index-store · {detail}"),
        ExploreError::CatalogAbsent => format!("catalog-absent · {SYNC_TEACHING}"),
        ExploreError::Catalog { detail } => format!("catalog · {detail}"),
        ExploreError::NotFound { package } => format!("not-found · {}", package.as_str()),
        ExploreError::Ambiguous { package, observed } => {
            format!("ambiguous · {} · {observed} rows", package.as_str())
        }
    }
}

/// The sheet footer: the row count, in the results sheet's own words.
fn footer(theme: &Theme, hits: usize) -> AnyElement {
    ui::with_role(div(), theme, Role::Dense)
        .px(gpui::px(theme.pixels(Space::Base.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .text_color(hsla(theme.palette().text_low()))
        .child(format!("{hits} rows"))
        .into_any_element()
}

/// The loaded index page's rows, empty while nothing is loaded or the slot is not ready.
#[must_use]
pub(crate) fn index_rows(workspace: &Workspace) -> &[IndexHitRow] {
    match workspace.explore().index_page() {
        Some(ExploreSlot::Ready(page)) => &page.hits,
        _ => &[],
    }
}

/// The package the index sheet's cursor points at, when a loaded row names one.
///
/// The durable projection carries no coordinate, so the row's document text is the exact name
/// the profile command is asked for, admitted through the same grammar every surface uses.
#[must_use]
pub(crate) fn selected_package(workspace: &Workspace) -> Option<ExplorePackageName> {
    let row = index_rows(workspace).get(workspace.search().index_cursor())?;
    ExplorePackageName::new(row.document.as_str()).ok()
}

/// The index mode's own Submit: a loaded page opens its selected row's profile, an unrun query
/// runs, and nothing typed does nothing.
pub(crate) fn submit_index(workspace: &mut Workspace) {
    if index_rows(workspace).is_empty() {
        let query = workspace.search().mode().query().to_owned();
        if let Ok(query) = ExploreQuery::new(&query) {
            workspace.search_index(query);
        }
        return;
    }
    if let Some(name) = selected_package(workspace) {
        workspace.package_profile(name);
    }
}
