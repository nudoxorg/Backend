//! The package page. The hero says what it is; the crest carries its
//! licence, what it does to your build and machine, its weight and its
//! advisories; the release ticker travels to any release; and the shingles
//! are its territory, one per public name, opening into cards whose badges
//! say what each name is. A future tour needs live typed use evidence;
//! prototype fixture-world rankings never recommend a starting declaration.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::AppSnapshot;
use crate::model::local_package::{ActiveProject, ReadmeBlock, active_project};
use crate::model::pages::{
    Dependency, DependencyScope, PackageDossier, PackageRef, PageKey, RecordSource,
};
use crate::navigation::{Intent, Route};
use crate::shell::kit::{HoverIntent, package_route, quiet, text};
use crate::shell::reader::Reader;
use facet::icons::Kind;
use facet::marks::{DepFacts, DepKind, Eco, EcoFacts, dep_line, ecosystem_mark};
use facet::tokens::fluid::PACKAGE_GEM;
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
    px,
};
use std::rc::Rc;

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
        ticker: data::ticker(
            &dossier,
            ready.as_deref(),
            pin_version.as_deref(),
            at.as_ref().map(|at| at.as_str()),
            &today,
        ),
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
    if let Some(leaf) = readme(&dossier, ctx) {
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
    // The name is never ellipsized: it wraps at identifier boundaries and
    // steps down only when one segment cannot fit the room beside the gem.
    let gem = PACKAGE_GEM.at(measure.fluid_room());
    let room = (measure.width() - gem - measure.space(Space::Wide)).max(px(120.0));
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
                .max_w(measure.width() * 0.9)
                .child(lede),
        );
    }
    // Who made it, read from its manifest (labelled: not an index fact).
    if !byline.is_empty() {
        let mut line = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(measure.space(Space::Snug));
        for (index, part) in byline.iter().enumerate() {
            if index > 0 {
                line = line.child(text(ty::SMALL, measure, palette.ink3).child("·"));
            }
            let ink = if part.contains('.') && part.contains('/') {
                palette.ink1
            } else {
                palette.ink2
            };
            line = line.child(text(ty::SMALL, measure, ink).child(ctx.say(part.clone())));
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
    div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Roomy))
        .child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Wide))
                .min_w_0()
                .child(
                    facet::paint::gem(Kind::Package)
                        .size(f32::from(gem))
                        .flex_none(),
                )
                // The words take what the gem leaves, and wrap inside it.
                .child(words.flex_1().min_w_0()),
        )
        .child(marks)
        .into_any_element()
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
    for block in blocks.iter().filter(|block| !matches!(block, ReadmeBlock::Paragraph(words) if crate::model::source_facts::docs::nav_row(words))).take(24) {
        column = column.child(readme_block(block, ctx));
    }
    Some(Leaf::new(column))
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
