//! The package page. The hero says what it is; the crest carries its
//! licence, what it does to your build and machine, its weight and its
//! advisories; the release ticker travels to any release; and the shingles
//! are its territory, one per public name, opening into cards whose badges
//! say what each name is. A future tour needs live typed use evidence;
//! prototype fixture-world rankings never recommend a starting declaration.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::AppSnapshot;
use crate::model::local_package::{
    ActiveProject, ReadmeBlock, ReadmeHeading, ReadmeLink, active_project, readme_fragment_slug,
};
use crate::model::pages::{
    Dependency, DependencyScope, PackageDossier, PackageRef, PageKey, RecordSource,
};
use crate::navigation::{Intent, Route};
use crate::shell::focus::Target;
use crate::shell::kit::{HoverIntent, package_route, quiet, text};
use crate::shell::markdown::FollowMarkdownLink;
use crate::shell::reader::Reader;
use facet::icons::Kind;
use facet::marks::{DepFacts, DepKind, Eco, EcoFacts, dep_line, ecosystem_mark};
use facet::tokens::fluid::PACKAGE_GEM;
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement,
    IntoElement, ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Window, div, point, px,
};
use std::rc::Rc;
use std::sync::Arc;

mod data;
mod fluid;
mod folio;
mod target;
use target::PageTarget;
#[cfg(test)]
mod regions_tests;
#[cfg(test)]
mod tests;

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

    // The pin is the route's package; the dossier is about the release being
    // read, which is the pin unless the route says `at`.
    let (pin, at) = match place {
        Route::Package(route) => (
            PackageRef::parse(route.package.as_str()).ok(),
            route.at.clone(),
        ),
        Route::Symbol(route) => (
            PackageRef::parse(route.package.as_str()).ok(),
            route.at.clone(),
        ),
        _ => (None, None),
    };
    // A registry tree read as a local root is a release too (`toml-0.8.23`).
    let pin_version = pin
        .as_ref()
        .and_then(PackageRef::release_version)
        .map(str::to_owned);
    let record = dossier.record.known();
    let name = record.map_or_else(
        || dossier.package.display_name().to_owned(),
        |record| record.name.to_string(),
    );
    let project_name = active.name.as_deref().unwrap_or("your project");
    let today = today();
    let past = at.as_ref().and_then(|at| {
        pin.as_ref()
            .map(|pin| data::past(pin, at.as_str(), ctx.links.snapshot(cx).key(), cx))
    });
    // What the source on disk says (read off the UI thread; cached).
    let hints: std::collections::HashMap<String, String> = dossier
        .dependencies
        .known()
        .map(|list| {
            list.iter()
                .filter_map(|d| {
                    d.resolved
                        .as_ref()
                        .and_then(|r| r.version())
                        .map(|v| (d.name.to_string(), v.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let project_path = workspace_active.map(|project| project.path());
    let source =
        crate::model::source_facts::reading(&dossier.package, &hints, project_path.as_deref(), cx);
    let ready = match &source {
        crate::model::source_facts::Reading::Ready(facts) => Some(facts.clone()),
        _ => None,
    };
    let modules = dossier
        .outline
        .known()
        .map(|tree| data::modules(tree, &name, ready.as_deref()))
        .unwrap_or_default();
    let (ticker, ticker_note) =
        if let Some(pin) = pin.as_ref().filter(|pin| pin.release().is_some()) {
            match crate::runtime::releases::get(
                pin,
                snapshot.key(),
                at.as_ref().map(|release| release.as_str()),
                cx,
            ) {
                crate::runtime::releases::Read::Reading => (
                    None,
                    Some("Reading the exact local registry release history…".into()),
                ),
                crate::runtime::releases::Read::Waiting => (
                    None,
                    Some("Waiting for an available release-read slot…".into()),
                ),
                crate::runtime::releases::Read::Unavailable(reason) => (
                    None,
                    Some(format!("Release history unavailable: {reason}").into()),
                ),
                crate::runtime::releases::Read::Ready(releases) => (
                    data::ticker(
                        &releases.krate,
                        pin_version.as_deref(),
                        at.as_ref().map(|at| at.as_str()),
                        &today,
                    ),
                    releases.note.as_ref().map(|note| note.to_string().into()),
                ),
            }
        } else {
            (None, None)
        };
    let facts = Rc::new(folio::Facts {
        name: name.clone().into(),
        at: at.as_ref().map(|at| at.as_str().to_owned().into()),
        pin: pin_version.clone().map(Into::into),
        documented: data::documented(&modules),
        structure: data::structure(&modules, ready.as_deref()),
        outline_gap: match &dossier.outline {
            crate::model::pages::Known::Unknown(gap) => Some(crate::shell::kit::gap_words(gap)),
            _ => None,
        },
        modules,
        heads: ready
            .as_deref()
            .map(|source| Rc::new(facet::folio::heads::findings(&data::signals(source)))),
        berg: ready
            .as_deref()
            .map(|source| Rc::new(data::berg(&name, source))),
        features: ready.as_deref().and_then(data::features).map(Rc::new),
        source,
        licence: data::licence(
            record,
            active.license.as_deref(),
            project_name,
            if is_active_project {
                data::Subject::Project
            } else {
                data::Subject::Dependency
            },
        ),
        advisories: data::advisories(record),
        ticker,
        ticker_note,
        past,
    });

    // The page's own width: the folio column is a reading column; the
    // territory wants the room the reader has.
    let measure = page_measure(ctx);
    let byline = ready.as_deref().map(data::byline).unwrap_or_default();
    let hero = hero(&dossier, &active, &name, &byline, &measure, ctx, cx);
    let id = format!(
        "folio-{}",
        pin.as_ref().map_or_else(
            || dossier.package.as_str().to_owned(),
            |pin| pin.as_str().to_owned()
        )
    );
    // A click on a card left this page: that card's module is open again on
    // coming back.
    let reopen = ctx.targets.left_by(place).and_then(|left| {
        let PageTarget::Card(symbol) = PageTarget::parse(&left)? else {
            return None;
        };
        facts
            .modules
            .iter()
            .find(|m| m.items.iter().any(|i| i.symbol == symbol))
            .map(|m| m.name.clone())
    });
    let folio = folio::Folio {
        id: id.into(),
        facts,
        measure,
        hero,
        links: ctx.links.clone(),
        targets: ctx.targets.clone(),
        recall: ctx.targets.recall(),
        reopen,
        active: ctx.active,
        package: dossier.package.clone(),
    };
    // Centred on the reading column it overflows.
    let overshoot = (measure.width() - ctx.measure.width()).max(px(0.0));
    let mut leaves = vec![Leaf::new(div().ml(-(overshoot * 0.5)).child(folio))];
    // A registry release the owner has not indexed offers to be added (W-Acquire).
    if let Some(offer) =
        crate::shell::acquire::page_offer(&dossier, ctx.links, cx.entity_id(), &ctx.measure, cx)
    {
        leaves.insert(0, Leaf::new(offer));
    }
    if let Some(leaf) = readme(&dossier, place, ctx) {
        leaves.push(leaf);
    }
    leaves
}

/// The measure of the page: the room the reader gives it this frame (the
/// window's, never a frame behind), up to a cap, rather than the reading
/// column's width.
fn page_measure(ctx: &Ctx<'_>) -> Measure {
    let scale = ctx.measure.scale();
    let room = ctx.content.width().max(ctx.measure.width());
    ctx.content.within(room.min(px(PAGE_MAX * scale)))
}

/// The widest the folio grows, px at 100 % text.
const PAGE_MAX: f32 = 1800.0;

fn hero(
    dossier: &PackageDossier,
    active: &ActiveProject,
    name: &str,
    byline: &[SharedString],
    measure: &Measure,
    ctx: &mut Ctx<'_>,
    cx: &gpui::App,
) -> AnyElement {
    let palette = ctx.palette;
    let record = dossier.record.known();
    let name = ctx.say(name.to_owned());
    // At large text sizes, a phone-width page has no useful room beside the
    // gem. Decide from this frame's effective content width, so text scale
    // and actual layout room both participate in the breakpoint.
    let gem = PACKAGE_GEM.at(measure.fluid_room());
    let stacked = hero_stacks(measure.width(), measure.scale());
    // The name is never ellipsized: it wraps at identifier boundaries and
    // steps down only when one segment cannot fit its actual room.
    let room = if stacked {
        measure.width()
    } else {
        measure.width() - gem - measure.space(Space::Wide)
    }
    .max(px(0.0));
    let (lines, role) = crate::shell::text_fit::fit_name(&name, ty::HERO, measure, room, cx);
    ctx.hero
        .extend(lines.iter().map(|line| SharedString::from(line.clone())));
    let mut words = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Tight))
        .child(crate::shell::text_fit::name_lines(
            &lines,
            role,
            palette.ink0,
        ));
    if let Some(description) = record.and_then(|record| record.description.known()) {
        // A Cargo description wraps its lines in the manifest; the page wraps its own.
        let lede = ctx.say(description.split_whitespace().collect::<Vec<_>>().join(" "));
        words = words.child(
            text(ty::LEDE, measure, palette.ink2)
                .w_full()
                .min_w_0()
                .max_w_full()
                .child(lede),
        );
    }
    // Who made it, read from its manifest (labelled: not an index fact).
    if !byline.is_empty() {
        let mut line = if stacked {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_y(measure.space(Space::Tight))
                .w_full()
                .min_w_0()
        } else {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(measure.space(Space::Snug))
                .w_full()
                .min_w_0()
        };
        for (index, part) in byline.iter().enumerate() {
            if !stacked && index > 0 {
                line = line.child(text(ty::SMALL, measure, palette.ink3).child("·"));
            }
            let ink = if part.contains('.') && part.contains('/') {
                palette.ink1
            } else {
                palette.ink2
            };
            // `min_w_0` lets a long repository path wrap inside the column
            // instead of forcing the flex row past the reader's right edge.
            let full_value = part.clone();
            line = line.child(
                div()
                    .min_w_0()
                    .max_w_full()
                    .when(stacked, |this| this.w_full())
                    .child(
                        text(ty::SMALL, measure, ink)
                            .min_w_0()
                            .max_w_full()
                            .child(ctx.say(full_value)),
                    ),
            );
        }
        words = words.child(line);
    }
    // Facts are marks, not text (gui-plan §6.2): the registry's stone with
    // its install line, and what it rests on. The licence and the releases
    // have their own places beneath.
    let mut marks = div()
        .id("pkg-hero-marks")
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Wide));
    if let Some(record) = record {
        // A project that was never published names its path, not a registry.
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
            marks = marks.child(ecosystem_mark("mk-eco", facts, measure));
        }
    }
    if let Some(list) = dossier.dependencies.known()
        && !list.is_empty()
    {
        let parent = dossier.package.display_name().to_owned();
        // The library, to link a dependency to the release of it that is
        // indexed here (a project's own dependencies are indexed as the
        // trees its lock pins, not under a registry address).
        let store = ctx.links.store.read(cx);
        let orbit = store.orbit();
        let indexed: &[crate::model::pages::IndexedPackage] = orbit
            .loaded_value()
            .and_then(|model| model.indexed.known())
            .map_or(&[], |list| &list[..]);
        let facts: Vec<DepFacts> = list
            .iter()
            .map(|dependency| dep_facts(dependency, active, indexed))
            .collect();
        // Each dependency that goes somewhere is a door: the keyboard stands
        // on it, Enter opens it as a click does, and Back lands on it again.
        let door_of: Vec<(SharedString, SharedString)> = facts
            .iter()
            .filter_map(|dep| {
                dep.target
                    .clone()
                    .map(|place| (place, PageTarget::Dependency(dep.name.clone()).id()))
            })
            .collect();
        let (targets, on_page, recall) = (ctx.targets.clone(), ctx.active, ctx.targets.recall());
        let (click_links, click_recall, door_links) =
            (ctx.links.clone(), recall.clone(), ctx.links.clone());
        marks = marks.child(
            dep_line("mk-deps", facts, parent, measure)
                .on_open(move |place, _window, cx| {
                    let door = door_of
                        .iter()
                        .find(|(at, _)| at == place)
                        .map(|(_, id)| id.clone());
                    open_dependency(place, door, &click_recall, &click_links, cx);
                })
                .wrap(move |dep, link| {
                    let Some(place) = dep.target.clone() else {
                        return link;
                    };
                    let door = PageTarget::Dependency(dep.name.clone());
                    let (links, recall, id) = (door_links.clone(), recall.clone(), door.id());
                    let act: crate::shell::focus::Act = Rc::new(move |_window, cx| {
                        open_dependency(&place, Some(id.clone()), &recall, &links, cx)
                    });
                    folio::door(&targets, on_page, &door, dep.name.clone(), act, link)
                }),
        );
    }
    let title_and_gem = if stacked {
        div()
            .flex()
            .flex_col()
            .items_stretch()
            .gap(measure.space(Space::Wide))
            .w_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .justify_center()
                    .w_full()
                    .child(facet::paint::gem(Kind::Package).size(f32::from(gem))),
            )
            .child(words.w_full().min_w_0())
    } else {
        div()
            .flex()
            .items_center()
            .gap(measure.space(Space::Wide))
            .w_full()
            .min_w_0()
            .child(
                facet::paint::gem(Kind::Package)
                    .size(f32::from(gem))
                    .flex_none(),
            )
            .child(words.flex_1().min_w_0())
    };
    div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Roomy))
        .w(measure.width())
        .max_w_full()
        .min_w_0()
        .child(title_and_gem)
        .child(marks)
        .into_any_element()
}

/// Stack the package gem above its words once the available width, adjusted
/// for the user's text scale, cannot give both columns a readable measure.
/// The breakpoint is in effective (100%-text) pixels, not nominal window
/// width, so 200% text turns a 390px window into a genuinely narrow page.
fn hero_stacks(available: gpui::Pixels, text_scale: f32) -> bool {
    f32::from(available) / text_scale.max(f32::EPSILON) < HERO_INLINE_MIN_EFFECTIVE
}

const HERO_INLINE_MIN_EFFECTIVE: f32 = 420.0;

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
fn dep_facts(
    dependency: &Dependency,
    active: &ActiveProject,
    indexed: &[crate::model::pages::IndexedPackage],
) -> DepFacts {
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
    let target = in_the_library(dependency, indexed)
        .or(dependency.resolved.as_ref())
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

/// The release of `dependency` the library holds, when it holds one: the
/// release its resolver chose, else the one release of that name, else the
/// one whose version the requirement names exactly (`0.8.23`, `=0.8.23`).
fn in_the_library<'a>(
    dependency: &Dependency,
    indexed: &'a [crate::model::pages::IndexedPackage],
) -> Option<&'a PackageRef> {
    let named: Vec<&PackageRef> = indexed
        .iter()
        .map(|package| &package.package)
        .filter(|package| package.display_name() == dependency.name.as_ref())
        .collect();
    let chosen = dependency
        .resolved
        .as_ref()
        .and_then(PackageRef::release_version);
    let written = dependency
        .requirement
        .trim_start_matches(['=', '^', '~', ' ']);
    named
        .iter()
        .copied()
        .find(|package| chosen.is_some_and(|version| package.release_version() == Some(version)))
        .or_else(|| (named.len() == 1).then(|| named[0]))
        .or_else(|| {
            named
                .iter()
                .copied()
                .find(|package| package.release_version() == Some(written))
        })
}

/// Follows a dependency mark's link to the package it names; the page is
/// left by its door (`door`), so Back lands on it again.
fn open_dependency(
    target: &SharedString,
    door: Option<SharedString>,
    recall: &crate::shell::focus::Recall,
    links: &crate::shell::region::Links,
    cx: &mut gpui::App,
) {
    if let Ok(package) = PackageRef::parse(target.as_ref())
        && let Some(route) = package_route(&package)
    {
        if let Some(door) = door {
            let leaving = links.snapshot(cx).route().clone();
            recall.focus(door.clone());
            recall.remember_leave(leaving, door);
        }
        links.dispatch(Intent::Navigate(route), cx);
    }
}

fn readme(dossier: &PackageDossier, place: &Route, ctx: &mut Ctx<'_>) -> Option<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let blocks = dossier.readme.known();
    let source = dossier
        .readme_markdown
        .known()
        .filter(|source| !source.trim().is_empty());
    if source.is_none() && blocks.is_none_or(|blocks| blocks.is_empty()) {
        return None;
    }
    let heading = ctx.say("Read me");
    let mut column = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Base))
        .max_w(px(680.0 * measure.scale()))
        .child(head(heading, &measure, palette));
    if let Some(source) = source {
        let readme_links = dossier
            .readme_links
            .known()
            .cloned()
            .unwrap_or_else(|| Arc::from([]));
        let headings = dossier
            .readme_headings
            .known()
            .cloned()
            .unwrap_or_else(|| Arc::from([]));
        let outline = dossier.outline.known().cloned();
        let package = dossier.package.clone();
        let restore_target = ctx.targets.left_by(place);
        let shell_links = ctx.links.clone();
        let recall = ctx.targets.recall();
        let reader_scroll = ctx.reader_scroll.clone();
        let action_links = Arc::clone(&readme_links);
        let action_headings = Arc::clone(&headings);
        let action_outline = outline.clone();
        let action_package = package.clone();
        let action_shell_links = shell_links.clone();
        let action_recall = recall.clone();
        let action_scroll = reader_scroll.clone();
        let rich = crate::shell::markdown::view(
            ElementId::Name(SharedString::from("package-readme-markdown")),
            SharedString::from(source.to_string()),
            Arc::clone(&headings),
        )
        .w_full();
        let rich = div()
            .w_full()
            .on_action::<FollowMarkdownLink>(move |action, window, app| {
                activate_readme_link(
                    &action.destination,
                    &action_links,
                    &action_headings,
                    action_outline.as_ref(),
                    &action_package,
                    &action_shell_links,
                    &action_recall,
                    &action_scroll,
                    window,
                    app,
                );
            })
            .child(rich);
        column = column.child(rich);
        if !headings.is_empty() {
            column =
                column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child("On this page"));
            for (index, target) in headings.iter().enumerate() {
                let id: SharedString = format!("readme-heading-link-{index}").into();
                let destination = format!("#{}", target.slug);
                let heading_links = Arc::clone(&readme_links);
                let heading_rows = Arc::clone(&headings);
                let heading_outline = outline.clone();
                let heading_package = package.clone();
                let heading_shell_links = shell_links.clone();
                let heading_recall = recall.clone();
                let heading_scroll = reader_scroll.clone();
                let act_destination = destination.clone();
                let act = Rc::new(move |window: &mut Window, app: &mut App| {
                    activate_readme_link(
                        &act_destination,
                        &heading_links,
                        &heading_rows,
                        heading_outline.as_ref(),
                        &heading_package,
                        &heading_shell_links,
                        &heading_recall,
                        &heading_scroll,
                        window,
                        app,
                    );
                });
                ctx.targets.push(Target {
                    id: id.clone(),
                    label: format!("Go to {}", target.title).into(),
                    act: act.clone(),
                    peek: None,
                    source: None,
                });
                if restore_target.as_ref() == Some(&id) {
                    ctx.targets.focus(id.clone());
                }
                let row = div()
                    .id(id.clone())
                    .w_full()
                    .pl(px(f32::from(target.level.saturating_sub(1)) * 8.0))
                    .cursor_pointer()
                    .text_color(palette.ink2.hsla())
                    .child(text(ty::PROSE, &measure, palette.ink2).child(target.title.to_string()))
                    .on_click(move |_: &ClickEvent, window, app| act(window, app));
                column = column.child(ctx.targets.track(id, row));
            }
        }
        if !readme_links.is_empty() {
            column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child("Links"));
            for (index, link) in readme_links.iter().enumerate() {
                let resolution = readme_link_kind(
                    &link.destination,
                    Some(link),
                    &headings,
                    outline.as_ref(),
                    &package,
                );
                let label = if link.label.is_empty() {
                    link.destination.to_string()
                } else {
                    link.label.to_string()
                };
                let id: SharedString = format!("readme-link-{index}").into();
                let result_note = match &resolution {
                    Some(ReadmeLinkKind::External(_)) => None,
                    Some(ReadmeLinkKind::Anchor(_)) => None,
                    Some(ReadmeLinkKind::File { .. }) => None,
                    Some(ReadmeLinkKind::Route(_)) => None,
                    None => Some("Target isn't available from this package"),
                };
                let destination = link.destination.to_string();
                if resolution.is_some() {
                    let action_links = Arc::clone(&readme_links);
                    let action_headings = Arc::clone(&headings);
                    let action_outline = outline.clone();
                    let action_package = package.clone();
                    let action_shell_links = shell_links.clone();
                    let action_recall = recall.clone();
                    let action_scroll = reader_scroll.clone();
                    let act_destination = destination.clone();
                    let act = Rc::new(move |window: &mut Window, app: &mut App| {
                        activate_readme_link(
                            &act_destination,
                            &action_links,
                            &action_headings,
                            action_outline.as_ref(),
                            &action_package,
                            &action_shell_links,
                            &action_recall,
                            &action_scroll,
                            window,
                            app,
                        );
                    });
                    let peek = match resolution.as_ref().expect("resolved link") {
                        ReadmeLinkKind::Route(route) => route_page_key(route),
                        _ => None,
                    };
                    let source = match resolution.as_ref().expect("resolved link") {
                        ReadmeLinkKind::Route(Route::Symbol(route))
                            if route.view == crate::navigation::View::Code =>
                        {
                            crate::model::pages::SymbolRef::new(route.id.as_str()).ok()
                        }
                        _ => None,
                    };
                    ctx.targets.push(Target {
                        id: id.clone(),
                        label: label.clone().into(),
                        act: act.clone(),
                        peek,
                        source,
                    });
                    if restore_target.as_ref() == Some(&id) {
                        ctx.targets.focus(id.clone());
                    }
                    let row = div()
                        .id(id.clone())
                        .w_full()
                        .cursor_pointer()
                        .text_color(palette.cyan.base.hsla())
                        .child(text(ty::PROSE, &measure, palette.cyan.base).child(label))
                        .on_click(move |_: &ClickEvent, window, app| act(window, app));
                    column = column.child(ctx.targets.track(id, row));
                } else {
                    column = column.child(
                        div()
                            .flex()
                            .flex_col()
                            .child(text(ty::PROSE, &measure, palette.ink2).child(label))
                            .child(quiet(
                                result_note.unwrap_or("Target unavailable"),
                                &measure,
                                palette,
                            )),
                    );
                }
            }
        }
        return Some(Leaf::new(column));
    }
    for block in blocks?.iter().filter(|block| !matches!(block, ReadmeBlock::Paragraph(words) if crate::model::source_facts::docs::nav_row(words))).take(24) {
        column = column.child(readme_block(block, ctx));
    }
    Some(Leaf::new(column))
}

#[derive(Clone, Debug)]
enum ReadmeLinkKind {
    External(String),
    Anchor(ReadmeHeading),
    File { path: String, line: u32 },
    Route(Route),
}

fn readme_link_kind(
    destination: &str,
    indexed: Option<&ReadmeLink>,
    headings: &[ReadmeHeading],
    outline: Option<&crate::model::pages::OutlineTree>,
    package: &PackageRef,
) -> Option<ReadmeLinkKind> {
    let trimmed = destination.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
    {
        return Some(ReadmeLinkKind::External(trimmed.to_owned()));
    }
    if let Some(fragment) = trimmed.strip_prefix('#') {
        let slug = readme_fragment_slug(&decode_fragment(fragment)?);
        return headings
            .iter()
            .find(|heading| heading.slug.as_ref() == slug)
            .cloned()
            .map(ReadmeLinkKind::Anchor);
    }
    if trimmed.starts_with("pkg:") {
        return PackageRef::parse(trimmed)
            .ok()
            .and_then(|package| package_route(&package))
            .map(ReadmeLinkKind::Route);
    }
    if let Some(path) = indexed.and_then(|link| link.local_file.as_deref()) {
        let line = indexed.and_then(|link| link.line).unwrap_or(1);
        if let Some(symbol_route) = exact_file_declaration_route(path, line, outline, package) {
            return Some(ReadmeLinkKind::Route(symbol_route));
        }
        return Some(ReadmeLinkKind::File {
            path: path.to_owned(),
            line,
        });
    }
    rustdoc_symbol_route(trimmed, outline, package).map(ReadmeLinkKind::Route)
}

fn activate_readme_link(
    destination: &str,
    indexed: &[ReadmeLink],
    headings: &[ReadmeHeading],
    outline: Option<&crate::model::pages::OutlineTree>,
    package: &PackageRef,
    shell_links: &crate::shell::region::Links,
    recall: &crate::shell::focus::Recall,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let link = indexed
        .iter()
        .find(|link| link.destination.as_ref() == destination);
    match readme_link_kind(destination, link, headings, outline, package) {
        Some(ReadmeLinkKind::External(url)) => cx.open_url(&url),
        Some(ReadmeLinkKind::Anchor(heading)) => {
            let key = ElementId::Name(SharedString::from(heading.element_id.to_string()));
            if let Some(bounds) = facet::motion::shared::last_bounds(key, window, cx) {
                let offset = scroll.offset();
                let top = scroll.bounds().origin.y + px(24.0);
                scroll.set_offset(point(
                    offset.x,
                    (offset.y + top - bounds.origin.y).min(px(0.0)),
                ));
            }
        }
        Some(ReadmeLinkKind::File { path, line }) => {
            // The path was canonicalized, kept under the local package root,
            // and checked as a file on the package-read worker.
            shell_links.dispatch(
                Intent::OpenSource {
                    path: Arc::from(path),
                    line,
                },
                cx,
            );
        }
        Some(ReadmeLinkKind::Route(route)) => {
            let leaving = shell_links.snapshot(cx).route().clone();
            let id = indexed
                .iter()
                .position(|candidate| candidate.destination.as_ref() == destination)
                .map(|index| SharedString::from(format!("readme-link-{index}")))
                .unwrap_or_else(|| SharedString::from("readme-inline-link"));
            recall.focus(id.clone());
            recall.remember_leave(leaving, id);
            shell_links.dispatch(Intent::Navigate(route), cx);
        }
        None => {}
    }
}

fn decode_fragment(fragment: &str) -> Option<String> {
    let bytes = fragment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let (Some(high), Some(low)) = (bytes.get(index + 1), bytes.get(index + 2)) else {
                return None;
            };
            decoded.push((hex(*high)? << 4) | hex(*low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn rustdoc_symbol_route(
    destination: &str,
    outline: Option<&crate::model::pages::OutlineTree>,
    package: &PackageRef,
) -> Option<Route> {
    let path = destination.split(['#', '?']).next()?;
    if path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|part| part == "..")
        || path.contains("://")
    {
        return None;
    }
    let leaf = path.rsplit('/').next()?;
    let stem = leaf.strip_suffix(".html")?;
    let (name, kind) = rustdoc_item_name(stem);
    let declaration = outline?.complete_names()?.unique_name(name, kind)?;
    crate::shell::kit::symbol_route(package.as_str(), &declaration.decl.coordinate)
}

fn rustdoc_item_name(stem: &str) -> (&str, Option<backend_library::DeclarationKind>) {
    use backend_library::DeclarationKind as Kind;

    [
        ("struct.", Some(Kind::Struct)),
        ("enum.", Some(Kind::Enum)),
        ("trait.", Some(Kind::Trait)),
        ("union.", Some(Kind::Union)),
        ("type.", Some(Kind::Type)),
        ("fn.", Some(Kind::Function)),
        ("method.", Some(Kind::Method)),
        ("associatedtype.", Some(Kind::Type)),
        ("associatedconstant.", Some(Kind::Constant)),
        ("macro.", Some(Kind::Macro)),
        ("constant.", Some(Kind::Constant)),
        ("static.", Some(Kind::Constant)),
        ("mod.", Some(Kind::Module)),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| stem.strip_prefix(prefix).map(|name| (name, kind)))
    .unwrap_or((stem, None))
}

fn exact_file_declaration_route(
    path: &str,
    line: u32,
    outline: Option<&crate::model::pages::OutlineTree>,
    package: &PackageRef,
) -> Option<Route> {
    let root = std::path::Path::new(package.as_str());
    let relative = std::path::Path::new(path)
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let mut declarations = outline?.walk().filter(|node| {
        node.decl.path.as_deref() == Some(relative.as_str()) && node.decl.line == Some(line)
    });
    let declaration = declarations.next()?;
    if declarations.next().is_some() {
        return None;
    }
    let mut route = crate::shell::kit::symbol_view_route(
        package.as_str(),
        &declaration.decl.coordinate,
        crate::navigation::View::Code,
        Some(line),
    )?;
    if let Route::Symbol(ref mut route) = route {
        route.at = None;
    }
    Some(route)
}

fn route_page_key(route: &Route) -> Option<crate::model::pages::PageKey> {
    match route {
        Route::Package(route) => PackageRef::parse(route.package.as_str())
            .ok()
            .map(crate::model::pages::PageKey::Package),
        Route::Symbol(route) => crate::model::pages::SymbolRef::new(route.id.as_str())
            .ok()
            .map(|symbol| {
                if route.view == crate::navigation::View::Code {
                    crate::model::pages::PageKey::Source(symbol)
                } else {
                    crate::model::pages::PageKey::Symbol(symbol)
                }
            }),
        Route::Orbit(_) | Route::World => None,
    }
}

fn readme_block(block: &ReadmeBlock, ctx: &mut Ctx<'_>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    // Markdown reads as words: a link is its text, an image is gone.
    let plain = |words: &str| crate::model::source_facts::docs::clean_inline(words);
    let (role, words, ink) = match block {
        ReadmeBlock::Heading { text: words, .. } => (ty::HEAD, plain(words), palette.ink0),
        ReadmeBlock::Paragraph(words) => (ty::PROSE, plain(words), palette.ink1),
        ReadmeBlock::Code { text: words, .. } => (ty::CODE, words.to_string(), palette.ink1),
        ReadmeBlock::Bullet(words) => (ty::PROSE, format!("· {}", plain(words)), palette.ink1),
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
