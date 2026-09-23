//! Package dossier for a local project, from its own manifests.
//!
//! The registry dossier renders producer facts. A local project has no
//! registry record, so this projection renders [`LocalPackage`] facts that the
//! runtime read offline from the project's `Cargo.toml` and README. Lanes the
//! manifest cannot answer (dependents, releases, advisories) say so plainly
//! instead of requesting registry surfaces for a package no registry holds.

use super::package::lane_tabs;
use super::primitives::{fact, heading};
use crate::core::{LocalProjectId, PackageId};
use crate::model::{
    AppSnapshot, CargoFailure, DependencyKind, LocalPackage, LocalPackageSource, ReadmeBlock,
};
use crate::navigation::{Coordinate, Intent, PackageLane, PackageRoute, PageRoute, Route};
use crate::runtime::UiRootEntity;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::ui::{components, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, div, px,
};

/// Rows rendered per list before the remainder is summarized.
const MAX_ROWS: usize = 200;

/// Returns the served local project that owns `package`, when the live
/// project projection lists it.
pub(super) fn owning_project(
    snapshot: &AppSnapshot,
    package: &PackageId,
) -> Option<LocalProjectId> {
    snapshot
        .project()
        .loaded_value()
        .filter(|project| project.packages.contains(package))
        .map(|project| project.id.clone())
}

pub(super) fn page(
    root: &mut UiRootEntity,
    theme: &Theme,
    snapshot: &AppSnapshot,
    route: &PackageRoute,
    project: &LocalProjectId,
    cx: &mut Context<UiRootEntity>,
) -> AnyElement {
    root.ensure_local_package(project, cx);
    let loaded = snapshot
        .local_package()
        .loaded_value()
        .filter(|package| package.project == *project);
    let Some(package) = loaded else {
        return surface::sunken(theme)
            .p(px(24.0))
            .child(
                text::single_line(text::body(theme))
                    .child("Reading this project's manifest and README…"),
            )
            .into_any_element();
    };
    let tabs = lane_tabs(theme, snapshot, route, route.selected, cx);
    surface::cut(theme, Paint::MintLine)
        .h_full()
        .p(px(28.0))
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(header(theme, package, route, cx))
        .child(div().flex().gap(space(Space::Tight)).children(tabs))
        .child(fact_rail(theme, package))
        .child(
            surface::sunken(theme)
                .id(ElementId::Name("local-package-section".into()))
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .p(px(22.0))
                .flex()
                .flex_col()
                .gap(space(Space::Base))
                .child(section(theme, package, route.lane)),
        )
        .into_any_element()
}

fn header(
    theme: &Theme,
    package: &LocalPackage,
    route: &PackageRoute,
    cx: &mut Context<UiRootEntity>,
) -> impl IntoElement {
    let docs = docs_button(theme, package, route, cx);
    let version = package.version.as_deref().unwrap_or("unversioned");
    let path = text::elide(package.project.as_str(), 72);
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(space(Space::Gutter))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .gap(space(Space::Tight))
                .child(heading(theme, package.name.as_ref()))
                .child(
                    text::single_line(text::faint(theme))
                        .child(format!("{version} · local project · {path}")),
                )
                .child(
                    text::body(theme).child(
                        package
                            .description
                            .as_deref()
                            .unwrap_or("The manifest states no description.")
                            .to_owned(),
                    ),
                )
                .when(!package.keywords.is_empty(), |header| {
                    header.child(
                        text::single_line(text::faint(theme)).child(
                            package
                                .keywords
                                .iter()
                                .map(|keyword| format!("#{keyword}"))
                                .collect::<Vec<_>>()
                                .join("  "),
                        ),
                    )
                }),
        )
        .when_some(docs, ParentElement::child)
}

/// The documentation entry, when the package name is a valid coordinate.
fn docs_button(
    theme: &Theme,
    package: &LocalPackage,
    route: &PackageRoute,
    cx: &mut Context<UiRootEntity>,
) -> Option<impl IntoElement> {
    let coordinate = Coordinate::new(package.name.as_ref()).ok()?;
    let doc_route = Route::Page(PageRoute {
        project: route.project.clone(),
        package: route.package.clone(),
        coordinate,
        selected: route.selected,
    });
    Some(components::measure(
        theme,
        "open-docs",
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
    ))
}

fn fact_rail(theme: &Theme, package: &LocalPackage) -> impl IntoElement {
    let unstated = "Not stated";
    let members = match package.members {
        0 => "No Cargo packages".to_owned(),
        1 => "1 package".to_owned(),
        count => format!("{count} packages"),
    };
    div()
        .flex()
        .flex_wrap()
        .gap(space(Space::Snug))
        .child(fact(
            theme,
            "License",
            package.license.as_deref().unwrap_or(unstated),
        ))
        .child(fact(
            theme,
            "Rust",
            package.rust_version.as_deref().unwrap_or(unstated),
        ))
        .child(fact(theme, "Workspace", &members))
        .child(fact(theme, "Facts from", source_label(package.source)))
}

fn source_label(source: LocalPackageSource) -> &'static str {
    match source {
        LocalPackageSource::Cargo => "cargo metadata (offline)",
        LocalPackageSource::Manifest(CargoFailure::Disabled | CargoFailure::Spawn) => {
            "Cargo.toml (Cargo unavailable)"
        }
        LocalPackageSource::Manifest(CargoFailure::Timeout) => "Cargo.toml (Cargo timed out)",
        LocalPackageSource::Manifest(
            CargoFailure::OutputLimit | CargoFailure::Status | CargoFailure::Decode,
        ) => "Cargo.toml (Cargo refused)",
        LocalPackageSource::NoManifest => "README only",
    }
}

fn section(theme: &Theme, package: &LocalPackage, lane: PackageLane) -> AnyElement {
    match lane {
        PackageLane::Overview => overview(theme, package),
        PackageLane::Dependencies => dependencies(theme, package),
        PackageLane::Dependents => note(
            theme,
            "Dependents",
            "Reverse dependencies are registry facts. This local project is not published, so no registry lists what depends on it.",
        ),
        PackageLane::Releases => note(
            theme,
            "Versions & releases",
            match package.version.as_deref() {
                Some(_) => {
                    "This is the working tree's manifest version. Local projects have no published release history."
                }
                None => {
                    "The manifest states no version, and local projects have no published release history."
                }
            },
        ),
        PackageLane::Security => note(
            theme,
            "Security advisories",
            "Advisory evidence is attached to registry releases and is not fetched for a local project. The absence of findings here is not a clean result.",
        ),
    }
}

fn note(theme: &Theme, title: &str, body: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .child(heading(theme, title))
        .child(text::body(theme).child(body.to_owned()))
        .into_any_element()
}

fn overview(theme: &Theme, package: &LocalPackage) -> AnyElement {
    let links = [
        ("Repository", package.repository.as_deref()),
        ("Homepage", package.homepage.as_deref()),
        ("Documentation", package.documentation.as_deref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.map(|value| (label, value)))
    .collect::<Vec<_>>();
    let categories = package
        .categories
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<&str>>()
        .join(", ");
    let blocks = package
        .readme
        .iter()
        .take(MAX_ROWS)
        .map(|block| readme_block(theme, block));
    div()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .children(links.into_iter().map(|(label, value)| {
            text::single_line(text::label(theme)).child(format!("{label}: {value}"))
        }))
        .when(!categories.is_empty(), |overview| {
            overview.child(
                text::single_line(text::label(theme)).child(format!("Categories: {categories}")),
            )
        })
        .child(heading(theme, "README"))
        .when(package.readme.is_empty(), |overview| {
            overview.child(text::body(theme).child("This project has no README."))
        })
        .children(blocks)
        .when(package.readme.len() > MAX_ROWS, |overview| {
            overview.child(more(
                theme,
                package.readme.len() - MAX_ROWS,
                "README blocks",
            ))
        })
        .into_any_element()
}

fn readme_block(theme: &Theme, block: &ReadmeBlock) -> AnyElement {
    match block {
        ReadmeBlock::Heading { level, text: title } => {
            let scale = match level {
                1 => TypeScale::Title,
                2 => TypeScale::Section,
                _ => TypeScale::Interface,
            };
            text::heading(theme, scale)
                .when(*level > 1, |heading| heading.mt(space(Space::Snug)))
                .child(title.to_string())
                .into_any_element()
        }
        ReadmeBlock::Paragraph(prose) => text::body(theme)
            .child(prose.to_string())
            .into_any_element(),
        ReadmeBlock::Bullet(item) => text::body(theme)
            .pl(space(Space::Room))
            .child(format!("•  {item}"))
            .into_any_element(),
        ReadmeBlock::Code {
            language,
            text: code,
        } => surface::panel(theme)
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(space(Space::Tight))
            .when_some(language.as_ref(), |block, language| {
                block.child(text::single_line(text::faint(theme)).child(language.to_string()))
            })
            .child(
                div()
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Small))
                    .text_color(theme.paint(Paint::Silver1))
                    .child(code.to_string()),
            )
            .into_any_element(),
    }
}

fn dependencies(theme: &Theme, package: &LocalPackage) -> AnyElement {
    let summary = format!(
        "{} unique dependency requirements across {}.",
        package.dependencies.len(),
        match package.members {
            1 => "1 workspace package".to_owned(),
            count => format!("{count} workspace packages"),
        }
    );
    let mut list = div()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .child(heading(theme, "Dependencies"))
        .child(text::single_line(text::faint(theme)).child(summary));
    for kind in [
        DependencyKind::Normal,
        DependencyKind::Build,
        DependencyKind::Development,
    ] {
        let rows = package
            .dependencies
            .iter()
            .filter(|dependency| dependency.kind == kind)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        list = list.child(
            text::single_line(text::heading(theme, TypeScale::Interface))
                .mt(space(Space::Snug))
                .child(format!("{} ({})", kind_title(kind), rows.len())),
        );
        list = list.children(rows.iter().take(MAX_ROWS).map(|dependency| {
            let users = match dependency.users {
                1 => String::new(),
                count => format!(" · used by {count} packages"),
            };
            div()
                .flex()
                .gap(space(Space::Snug))
                .min_w(px(0.0))
                .child(text::single_line(text::label(theme)).child(dependency.name.to_string()))
                .child(
                    text::single_line(text::dim(theme))
                        .child(format!("{}{users}", dependency.requirement)),
                )
        }));
        if rows.len() > MAX_ROWS {
            list = list.child(more(theme, rows.len() - MAX_ROWS, "dependencies"));
        }
    }
    list.child(features(theme, package)).into_any_element()
}

fn kind_title(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Normal => "Runtime",
        DependencyKind::Build => "Build",
        DependencyKind::Development => "Development",
    }
}

fn features(theme: &Theme, package: &LocalPackage) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .mt(space(Space::Loose))
        .child(heading(theme, "Features"))
        .when(package.features.is_empty(), |features| {
            features.child(text::body(theme).child("The manifest declares no Cargo features."))
        })
        .children(package.features.iter().take(MAX_ROWS).map(|feature| {
            let enables = if feature.members.is_empty() {
                "enables nothing".to_owned()
            } else {
                feature
                    .members
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<&str>>()
                    .join(", ")
            };
            let users = match feature.users {
                1 => String::new(),
                count => format!(" · {count} packages"),
            };
            div()
                .flex()
                .gap(space(Space::Snug))
                .min_w(px(0.0))
                .child(text::single_line(text::label(theme)).child(feature.name.to_string()))
                .child(text::single_line(text::dim(theme)).child(format!("{enables}{users}")))
        }))
        .when(package.features.len() > MAX_ROWS, |features| {
            features.child(more(theme, package.features.len() - MAX_ROWS, "features"))
        })
}

fn more(theme: &Theme, hidden: usize, noun: &str) -> impl IntoElement {
    text::single_line(text::faint(theme)).child(format!("{hidden} more {noun} not shown."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::ProjectState;
    use std::sync::Arc;

    #[test]
    fn only_packages_of_the_served_project_take_the_local_dossier() -> Result<(), String> {
        let key = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("local".to_owned(), "dossier".to_owned())]),
            1,
        );
        let project =
            LocalProjectId::new("/tmp/nudox-local-dossier").map_err(|error| error.to_string())?;
        let local = PackageId::new("nudox-local-dossier").map_err(|error| error.to_string())?;
        let registry =
            PackageId::new("pkg:cargo/serde@1.0.0").map_err(|error| error.to_string())?;
        let empty = AppSnapshot::empty(key);
        assert_eq!(owning_project(&empty, &local), None);
        let served = empty.with_project(
            ProjectState {
                id: project.clone(),
                label: Arc::from("nudox-local-dossier"),
                packages: Arc::from([local.clone()]),
            },
            key,
        );
        assert_eq!(owning_project(&served, &local), Some(project));
        assert_eq!(owning_project(&served, &registry), None);
        Ok(())
    }
}
