//! The package page: recorded identity, a bounded outline, dependencies and
//! authored README blocks. A future tour needs live typed use evidence;
//! prototype fixture-world rankings never recommend a starting declaration.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::AppSnapshot;
use crate::model::local_package::{ActiveProject, ReadmeBlock, active_project};
use crate::model::pages::{
    Dependency, DependencyScope, OutlineNode, PackageDossier, PackageRef, PageKey, RecordSource,
    Standing,
};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::runtime::fixture_releases;
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{
    HoverIntent, gap_words, kind_of, package_route, quiet, symbol_route, text,
};
use crate::shell::reader::Reader;
use facet::icons::{Icon, IconSize, Kind, KindSize, ui};
use facet::marks::semver::ReleaseFact;
use facet::marks::{
    DepFacts, DepKind, Eco, EcoFacts, LicenseFacts, VersionFacts, dep_line, dep_link,
    ecosystem_mark, license_mark, version_mark,
};
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use std::rc::Rc;

pub(super) fn body(
    place: &Route,
    snapshot: &AppSnapshot,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    _hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let Some(package) = crate::runtime::store::route_package(place) else {
        return vec![Leaf::new(quiet(
            "This page's address is not a package.",
            &ctx.measure,
            ctx.palette,
        ))];
    };
    let resource = store.package(&package);
    let dossier = match shown(&resource) {
        Shown::Ready(dossier) => dossier.clone(),
        other => {
            return not_ready(
                &other,
                &PageKey::Package(package.clone()),
                package.display_name(),
                ctx,
                cx,
            );
        }
    };
    // The reader's own workspace project, not the package whose page is
    // open: what a fit line is measured against, and what "yours" means
    // for a dependency. Read the same cheap, cargo-free way as the local
    // package loader's README-only projection (no subprocess).
    let workspace_active = snapshot.workspace().active.as_ref();
    let active = workspace_active
        .map(|project| active_project(&project.path()))
        .unwrap_or_default();
    // The package whose page is open *is* that active project: its own
    // hero reads its own tree rather than comparing itself with itself.
    let is_active_project =
        workspace_active.is_some_and(|project| dossier.package.as_str() == project.as_str());
    let mut leaves = vec![hero(&dossier, &active, is_active_project, ctx, cx)];
    leaves.push(outline(&dossier, ctx, cx));
    if let Some(leaf) = dependencies(&dossier, &active, ctx) {
        leaves.push(leaf);
    }
    if let Some(leaf) = readme(&dossier, ctx) {
        leaves.push(leaf);
    }
    leaves
}

fn hero(
    dossier: &PackageDossier,
    active: &ActiveProject,
    is_active_project: bool,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    eprintln!(
        "MARKS_DEBUG hero package={:?} versions={:?} record_source={:?}",
        dossier.package.as_str(),
        dossier.versions,
        dossier.record.known().map(|r| r.source)
    );
    let record = dossier.record.known();
    let name = ctx.say(record.map_or_else(
        || dossier.package.display_name().to_owned(),
        |record| record.name.to_string(),
    ));
    let mut words = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Tight))
        .child(text(ty::HERO, &measure, palette.ink0).child(name));
    if let Some(description) = record.and_then(|record| record.description.known()) {
        let lede = ctx.say(description.to_string());
        words = words.child(text(ty::LEDE, &measure, palette.ink2).child(lede));
    }

    // The rejected pattern was a table of italic labels (VERSION /
    // ECOSYSTEM / OUTLINE READ). Each fact is now its own mark
    // (gui-plan.md §6.2: "facts are components, not text"), built straight
    // from the dossier the marks module already knows how to render, never
    // duplicating a producer. A fact this dossier does not carry (release
    // dates beyond the pinned upgrade-lens fixture, lockfile duplicates,
    // tree licenses, dependency usage) renders through the mark's own
    // honest-unknown path rather than being invented here.
    let mut marks = div()
        .id("pkg-hero-marks")
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Wide));
    if let Some(record) = record {
        // No committed release exists for this coordinate at all (a
        // workspace project that was never published, e.g. `publish =
        // false`): the ecosystem mark names the path instead of a
        // registry, and the version mark's comb shows the unpublished
        // trail instead of a release history.
        let unpublished = matches!(record.source, RecordSource::LocalManifest);
        if let Some(eco) = record
            .ecosystem
            .known()
            .and_then(|ecosystem| Eco::of(ecosystem))
        {
            let version = record.version.known().map(|version| version.as_ref());
            let install = eco.install(record.name.as_ref(), version);
            let mut facts = EcoFacts::new(eco, Some(install.as_str()));
            if unpublished {
                facts.local = Some(SharedString::from(
                    dossier.package.display_name().to_owned(),
                ));
            }
            marks = marks.child(ecosystem_mark("mk-eco", facts, &measure));
        }
        let spdx = record.license.known().map(ToString::to_string);
        // Comparing a package's license with itself says nothing (the
        // lead's ruling): the fit is measured against the *active*
        // project's own license, read from its manifest, never from this
        // dossier. When that project's own license is not known, the card
        // omits the fit line rather than guess.
        let project_name = active.name.as_deref().unwrap_or("your project");
        let mut license_facts =
            LicenseFacts::new(spdx.as_deref(), active.license.as_deref(), project_name);
        license_facts.own = is_active_project;
        marks = marks.child(license_mark("mk-license", license_facts, &measure));

        if let Some(versions) = dossier.versions.known() {
            // Release publish dates: the pinned upgrade-lens fixture
            // (`runtime::fixture_releases`, toml and smallvec today). Every
            // other release renders with `at: None`, an honest unknown age
            // rather than a guess.
            let dated = fixture_releases::release_data(&dossier.package, cx);
            let releases = versions
                .iter()
                .map(|entry| {
                    let at = dated.and_then(|release| {
                        release
                            .versions
                            .iter()
                            .find(|known| known.v.as_ref() == entry.version.as_ref())
                            .map(|known| known.at.to_string())
                    });
                    ReleaseFact::new(
                        entry.version.to_string(),
                        at.as_deref(),
                        entry.standing == Standing::Yanked,
                    )
                })
                .collect::<Vec<_>>();
            let facts = VersionFacts {
                name: record.name.to_string().into(),
                releases,
                pin: record.version.known().map(ToString::to_string),
                also: Vec::new(),
                yours: Vec::new(),
                pin_via: Vec::new(),
                measured: None,
                local: unpublished
                    .then(|| SharedString::from(dossier.package.display_name().to_owned())),
                now: today().into(),
            };
            marks = marks.child(version_mark("mk-version", facts, &measure));
        }
    }
    if let Some(list) = dossier.dependencies.known()
        && !list.is_empty()
    {
        let parent = dossier.package.display_name().to_owned();
        let facts: Vec<DepFacts> = list
            .iter()
            .map(|dependency| dep_facts(dependency, active))
            .collect();
        let links = ctx.links.clone();
        marks = marks.child(dep_line("mk-deps", facts, parent, &measure).on_open(
            move |target, _window, cx| {
                open_dependency(target, &links, cx);
            },
        ));
    }

    let graph_links = ctx.links.clone();
    let graph_package = dossier.package.clone();
    let graph_button =
        facet::controls::button("browse-package-graph", "Browse dependency graph", &measure)
            .ghost()
            .on_click(move |_, cx| {
                graph_links.dispatch(
                    Intent::Navigate(Route::Orbit(OrbitRoute::Browse(
                        BrowseRoute::PackageGraph {
                            package: graph_package.clone(),
                            direction: backend_library::PackageGraphDirection::Dependencies,
                            authority: None,
                            cursor: None,
                        },
                    ))),
                    cx,
                )
            });
    Leaf::new(
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Roomy))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Wide))
                    .child(
                        facet::paint::gem(Kind::Package).size(f32::from(measure.fluid(48.0, 64.0))),
                    )
                    .child(words),
            )
            .child(marks)
            .child(graph_button),
    )
}

/// Today, as `semver::ago` reads it (`YYYY-MM-DD`, UTC): the inverse of the
/// days-from-civil arithmetic `facet::marks::semver::days` already uses, so
/// no date-formatting dependency is added for one field.
fn today() -> String {
    let days = i64::try_from(crate::shell::root::now_ms() / 86_400_000).unwrap_or(0) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// One dependency's facts for its mark: what the dossier already carries.
/// Usage counts, feature gating and tree standing have no producer reaching
/// this page yet, so they stay unknown rather than invented (the mark's own
/// honest-unknown line, proved in `marks::tests`). "Yours" means a member
/// of the reader's own cargo workspace (`active.members`, from its
/// `[workspace] members`) — not merely a coordinate that resolves to a
/// path on this machine, which vendored and registry sources do too.
fn dep_facts(dependency: &Dependency, active: &ActiveProject) -> DepFacts {
    let kind = match dependency.scope {
        DependencyScope::Development => DepKind::Dev,
        DependencyScope::Build => DepKind::Build,
        DependencyScope::Runtime | DependencyScope::Optional | DependencyScope::Peer => {
            DepKind::Normal
        }
    };
    let resolved = dependency
        .resolved
        .as_ref()
        .and_then(|package| package.version().map(str::to_owned));
    let local = active.members.contains(dependency.name.as_ref());
    // Dead end #15 (`shell/kit.rs`): a link without a place is drawn as
    // text, never as a control that looks live but goes nowhere. An
    // unresolved dependency has no target at all, rather than a name that
    // cannot actually open.
    let target = dependency
        .resolved
        .as_ref()
        .map(|package| SharedString::from(package.as_str().to_owned()));
    DepFacts {
        name: dependency.name.to_string().into(),
        kind,
        req: Some(dependency.requirement.to_string()),
        resolved,
        newest: None,
        optional: dependency.optional || dependency.scope == DependencyScope::Optional,
        features: Vec::new(),
        on_by_default: None,
        on_in_tree: None,
        uses: None,
        items: Vec::new(),
        purpose: None,
        local,
        in_tree: None,
        tree_note: Some("your tree is not read yet".to_owned()),
        target,
    }
}

/// Follows a dependency mark's link to the package it names.
fn open_dependency(target: &SharedString, links: &crate::shell::region::Links, cx: &mut gpui::App) {
    if let Ok(package) = PackageRef::parse(target.as_ref())
        && let Some(route) = package_route(&package)
    {
        links.dispatch(Intent::Navigate(route), cx);
    }
}

fn outline(dossier: &PackageDossier, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let heading = ctx.say("Recorded outline");
    let note = ctx.say("Choose a row to inspect the declarations recorded beneath it.");
    let mut column = div()
        .flex()
        .flex_col()
        .child(head(heading, &measure, palette))
        .child(text(ty::SMALL, &measure, palette.ink2).child(note));
    match dossier.outline.known() {
        Some(tree) => {
            let expanded = ctx.package_outline_expanded;
            let shown = tree.roots.len().min(if expanded { 48 } else { 8 });
            for node in tree.roots.iter().take(shown) {
                column = column.child(node_row(node, &dossier.package, ctx, cx));
            }
            if tree.roots.len() > 8 {
                let id: SharedString = "pkg-outline-toggle".into();
                let label = ctx.say(if expanded {
                    "Show fewer entries"
                } else {
                    "Show more entries"
                });
                let weak = cx.weak_entity();
                let act: Act = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |reader, cx| reader.toggle_package_outline(cx));
                });
                if ctx.active {
                    ctx.targets.push(Target {
                        id: id.clone(),
                        label: label.clone(),
                        act: Rc::clone(&act),
                        peek: None,
                        source: None,
                    });
                }
                let mut toggle = div()
                    .id(id.clone())
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Base))
                    .py(measure.space(Space::Base))
                    .px(measure.space(Space::Base))
                    .cursor_pointer()
                    .hover(|style| style.bg(palette.tint))
                    .child(ui(Icon::Layers, IconSize::S16, palette.peri.base))
                    .child(text(ty::SMALL, &measure, palette.ink0).child(label));
                if !expanded {
                    toggle =
                        toggle.child(text(ty::CAPTION, &measure, palette.ink2).child(ctx.say(
                            format!("{} further top-level entries", tree.roots.len() - shown),
                        )));
                }
                column = column.child(ctx.targets.track(
                    id.clone(),
                    toggle.on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
                ));
            }
            if tree.roots.len() > shown && expanded {
                column = column.child(quiet(
                    ctx.say(format!(
                        "{} further top-level entries are in the library sidebar",
                        tree.roots.len() - shown
                    )),
                    &measure,
                    palette,
                ));
            }
            if !tree.complete {
                let more = ctx.say("The outline is still being read.");
                column = column.child(quiet(more, &measure, palette));
            }
        }
        None => {
            if let Some(gap) = dossier.outline.gap() {
                let words = ctx.say(gap_words(gap));
                column = column.child(quiet(words, &measure, palette));
            }
        }
    }
    Leaf::new(column)
}

fn node_row(
    node: &OutlineNode,
    package: &PackageRef,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let symbol = node.decl.coordinate.clone();
    let id: SharedString = format!("pkg-{}", symbol.as_str()).into();
    let name = ctx.say(node.decl.name.to_string());
    let route = symbol_route(package.as_str(), &symbol);
    let links = ctx.links.clone();
    let act: Act = Rc::new(move |_, cx| {
        if let Some(route) = route.clone() {
            links.dispatch(Intent::Navigate(route), cx);
        }
    });
    ctx.targets.push(Target {
        id: id.clone(),
        label: name.clone(),
        act: Rc::clone(&act),
        peek: Some(PageKey::Symbol(symbol.clone())),
        source: None,
    });
    let count = node.count().saturating_sub(1);
    let warm = PageKey::Symbol(symbol);
    let row = div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(measure.space(Space::Roomy))
        .h(measure.row() + measure.space(Space::Base))
        .px(measure.space(Space::Base))
        .hover(|style| style.bg(palette.tint))
        .child(crate::shell::kit::kind_mark(
            kind_of(node.decl.kind),
            KindSize::Sm,
            &measure,
            palette,
        ))
        .child(text(ty::MONO_ROW, &measure, palette.ink1).child(name))
        .children(
            (count > 0)
                .then(|| text(ty::SMALL, &measure, palette.ink2).child(format!("{count} nested"))),
        )
        .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
        .on_hover(cx.listener(move |reader, hovered: &bool, _, cx| {
            reader.hover_link(warm.clone(), *hovered, cx)
        }));
    ctx.targets.track(id, row).into_any_element()
}

fn dependencies(
    dossier: &PackageDossier,
    active: &ActiveProject,
    ctx: &mut Ctx<'_>,
) -> Option<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let list = dossier.dependencies.known()?;
    if list.is_empty() {
        return None;
    }
    let heading = ctx.say("Depends on");
    let mut column = div()
        .flex()
        .flex_col()
        .child(head(heading, &measure, palette));
    let parent = dossier.package.display_name().to_owned();
    let links = ctx.links.clone();
    for dependency in list.iter() {
        let id: SharedString = format!("pkg-dep-{}", dependency.name.as_ref()).into();
        let facts = dep_facts(dependency, active);
        let links = links.clone();
        let link = dep_link(id.clone(), facts, parent.clone(), &measure)
            .on_open(move |target, _window, cx| open_dependency(target, &links, cx));
        column = column.child(
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(measure.space(Space::Roomy))
                .h(measure.row() + measure.space(Space::Tight))
                .px(measure.space(Space::Base))
                .child(link),
        );
    }
    Some(Leaf::new(column))
}

fn readme(dossier: &PackageDossier, ctx: &mut Ctx<'_>) -> Option<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let blocks = dossier.readme.known()?;
    if blocks.is_empty() {
        return None;
    }
    let heading = ctx.say("Read me");
    let mut column = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Base))
        .max_w(px(680.0 * measure.scale()))
        .child(head(heading, &measure, palette));
    for block in blocks.iter().take(24) {
        column = column.child(readme_block(block, ctx));
    }
    Some(Leaf::new(column))
}

fn readme_block(block: &ReadmeBlock, ctx: &mut Ctx<'_>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let (role, words, ink) = match block {
        ReadmeBlock::Heading { text: words, .. } => (ty::HEAD, words.to_string(), palette.ink0),
        ReadmeBlock::Paragraph(words) => (ty::PROSE, words.to_string(), palette.ink1),
        ReadmeBlock::Code { text: words, .. } => (ty::CODE, words.to_string(), palette.ink1),
        ReadmeBlock::Bullet(words) => (ty::PROSE, format!("· {words}"), palette.ink1),
    };
    let said = ctx.say(words);
    text(role, &measure, ink).child(said).into_any_element()
}

fn head(title: SharedString, measure: &Measure, palette: &Palette) -> AnyElement {
    text(ty::TITLE, measure, palette.ink0)
        .pt(measure.space(Space::Wide))
        .pb(measure.space(Space::Base))
        .child(title)
        .into_any_element()
}
