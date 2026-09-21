//! Crates.io-style package dossier and registry fact lanes.

use super::primitives::{fact, heading, loading_card};
use crate::model::{AppSnapshot, PackageSummary};
use crate::navigation::{Intent, PackageLane, PackageRoute, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, space};
use crate::ui::{components, surface, text};
use backend_library::{PackageReference, SurfaceCommand};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::Selectable as _;

pub(super) fn package_page(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    route: &PackageRoute,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let package = snapshot.catalog().loaded_value().and_then(|catalog| {
        catalog
            .packages
            .iter()
            .find(|row| row.coordinate == route.package)
    });
    let Some(package) = package else {
        return loading_card(theme, snapshot.catalog().terminal()).into_any_element();
    };
    let package_id = package.coordinate.clone();
    let tabs = [
        ("Overview", PackageLane::Overview),
        ("Dependencies", PackageLane::Dependencies),
        ("Dependents", PackageLane::Dependents),
        ("Releases", PackageLane::Releases),
        ("Security", PackageLane::Security),
    ]
    .into_iter()
    .map(|(label, lane)| {
        let route = Route::Package(PackageRoute {
            project: route.project.clone(),
            package: package_id.clone(),
            lane,
            selected: Some(package.object),
        });
        components::button_with_state(
            theme,
            format!("package-tab-{label}"),
            label,
            components::Weight::Quiet,
            false,
            true,
        )
        .selected(route == *snapshot.route())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.queue(Intent::Navigate(route.clone()), cx);
        }))
        .into_any_element()
    })
    .collect::<Vec<_>>();
    let doc_route = Route::Page(crate::navigation::PageRoute {
        project: route.project.clone(),
        package: package.coordinate.clone(),
        coordinate: crate::navigation::Coordinate::new(package.name.as_ref())
            .unwrap_or_else(|_| crate::navigation::Coordinate::new("package").expect("literal")),
        selected: Some(package.object),
    });
    surface::cut(theme, Paint::PeriwinkleLine)
        .p(px(28.0))
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space(Space::Tight))
                        .child(heading(theme, package.name.as_ref()))
                        .child(text::single_line(text::faint(theme)).child(format!(
                            "{} · {} · {} bytes",
                            package.coordinate, package.version, package.bytes
                        )))
                        .child(text::single_line(text::body(theme)).child(
                            "Version-pinned package dossier from the live registry projection.",
                        )),
                )
                .child(
                    components::button_with_state(
                        theme,
                        "open-docs",
                        "Read documentation",
                        components::Weight::Primary,
                        false,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue(Intent::Navigate(doc_route.clone()), cx);
                    })),
                ),
        )
        .child(div().flex().gap(space(Space::Tight)).children(tabs))
        .child(package_fact_rail(theme, package))
        .child(package_sections(root, theme, package, route, cx))
        .into_any_element()
}

fn package_fact_rail(theme: &Theme, package: &PackageSummary) -> impl IntoElement {
    div()
        .flex()
        .gap(space(Space::Snug))
        .child(fact(theme, "Downloads", package.downloads.as_ref()))
        .child(fact(theme, "Release", package.standing.as_ref()))
        .child(fact(theme, "Security", package.advisory.as_ref()))
        .child(fact(theme, "Ecosystem", package.ecosystem.as_ref()))
}

fn package_sections(
    root: &mut UiRootEntity,
    theme: &Theme,
    package: &PackageSummary,
    route: &PackageRoute,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    let (title, body) = match route.lane {
        PackageLane::Overview => (
            "README",
            "The package overview is pinned to the exact release admitted by the local index. Open docs or source from the same object identity.",
        ),
        PackageLane::Dependencies => (
            "Dependencies",
            "Outgoing dependency facts are requested from the producer and retain their coverage state at this version.",
        ),
        PackageLane::Dependents => (
            "Dependents",
            "Reverse dependency facts remain separate from an empty result, so unavailable registry coverage is never shown as zero.",
        ),
        PackageLane::Releases => (
            "Versions & releases",
            "Release history is ordered by the registry frontier and keeps yanked, deprecated, and removed standing visible.",
        ),
        PackageLane::Security => (
            "Security advisories",
            "Advisory evidence and acquisition policy are displayed exactly as admitted for this release.",
        ),
    };
    let Some(reference) = PackageReference::parse(package.coordinate.as_str()).ok() else {
        return surface::sunken(theme)
            .p(px(22.0))
            .child(text::single_line(text::body(theme)).child(
                "The producer returned an invalid package coordinate; no registry request was issued.",
            ))
            .into_any_element();
    };
    let command = match route.lane {
        PackageLane::Dependencies => SurfaceCommand::Dependencies { package: reference },
        PackageLane::Dependents => SurfaceCommand::Dependents { package: reference },
        PackageLane::Releases => SurfaceCommand::PackageVersions { package: reference },
        PackageLane::Security => SurfaceCommand::Advisory {
            package: reference,
            override_evidence: None,
        },
        PackageLane::Overview => SurfaceCommand::Package { package: reference },
    };
    root.ensure_surface(command, cx);
    surface::sunken(theme)
        .p(px(22.0))
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .child(heading(theme, title))
        .child(text::single_line(text::body(theme)).child(body))
        .child(
            text::single_line(text::faint(theme))
                .child(format!("{} · live facts", package.coordinate)),
        )
        .into_any_element()
}
