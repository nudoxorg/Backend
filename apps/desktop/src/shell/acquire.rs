//! The shell's side of adding a release (W-Acquire): facet's add control is
//! handed the state of an addition and two actions, the same on every
//! surface that offers one (Find, a package page's header).

use super::kit::package_route;
use super::region::Links;
use crate::model::pages::PackageRef;
use crate::model::release::{Availability, Offer, Release};
use crate::navigation::Intent;
use crate::runtime::acquire::{self, Removal, Stage};
use crate::runtime::offload::Asker;
use facet::browse::acquire::{AddActions, Adding, Place, Step, add_control};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{AnyElement, App, EntityId, IntoElement, ParentElement, SharedString, Styled, div};
use std::rc::Rc;

/// The actions for the view `view`: it redraws as an addition moves.
pub(crate) fn add_actions(links: &Links, view: EntityId) -> AddActions {
    let (adding, opening) = (links.clone(), links.clone());
    let generation = crate::host::registry::composed().map(|composition| composition.generation);
    AddActions {
        state: Rc::new(move |release, cx| adding_state(release, Asker::View(view), cx)),
        add: Rc::new(move |release, _, cx| {
            if let Some(release) = Release::from_purl(&release) {
                adding.dispatch(Intent::AddRelease(release), cx);
            }
        }),
        open: Rc::new(move |package, _, cx| {
            if let Ok(package) = PackageRef::parse(&package)
                && let Some(route) = package_route(&package)
            {
                opening.dispatch(Intent::Navigate(route), cx);
            }
        }),
        cancel: None,
    }.admitted(Rc::new(move |_, _| generation.is_some()
        && crate::host::registry::composed().map(|composition| composition.generation) == generation))
}

/// Where adding the release at `purl` stands, in facet's words.
fn adding_state(purl: &SharedString, asker: Asker, cx: &mut App) -> Adding {
    let Some(release) = Release::from_purl(purl) else { return Adding::Idle };
    facet_stage(acquire::stage(&release, asker, cx))
}

pub(crate) fn facet_stage(stage: Option<Stage>) -> Adding {
    match stage {
        None => Adding::Idle,
        Some(Stage::Queued) => Adding::Working(Step::Waiting),
        Some(Stage::Resolving) => Adding::Working(Step::Resolving),
        Some(Stage::Unpacking) => Adding::Working(Step::Unpacking),
        Some(Stage::Indexing) => Adding::Working(Step::Indexing),
        Some(Stage::Added(package) | Stage::Partial { page: package, .. }) => Adding::Added { open: package.as_str().to_owned().into() },
        Some(Stage::Failed(reason)) => Adding::Failed(reason.to_string().into()),
    }
}

/// Facet's offer for `offer`.
pub(crate) fn facet_offer(offer: &Offer) -> facet::browse::acquire::Offer {
    facet::browse::acquire::Offer {
        release: offer.release.purl().into(),
        label: offer.release.to_string().into(),
        place: match &offer.availability {
            Availability::Unpacked(_) => Place::Unpacked,
            Availability::Archive(_) => Place::Archive,
            Availability::Download => Place::Download,
            Availability::Ambiguous { indexes } => Place::Ambiguous {
                indexes: indexes.clone(),
            },
            Availability::UnverifiedArchive(path) => {
                Place::UnverifiedArchive(path.clone())
            }
        },
        library: offer.library.as_ref().map(|package| package.as_str().to_owned().into()),
    }
}

/// The release a package page's removal is waiting to be confirmed for, and
/// the reading visit that asked: one at a time, cleared by either answer, and
/// void once the reader leaves that visit.
#[derive(Default)]
struct RemovalAsk(Option<(Release, crate::navigation::presentation::VisitId)>);

impl gpui::Global for RemovalAsk {}

fn visit(links: &Links, cx: &App) -> crate::navigation::presentation::VisitId {
    links.snapshot(cx).session().reading.current.id
}

fn asking(release: &Release, links: &Links, cx: &App) -> bool {
    cx.try_global::<RemovalAsk>().and_then(|ask| ask.0.as_ref())
        .is_some_and(|(asked, at)| asked == release && *at == visit(links, cx))
}

fn ask_removal(release: Option<Release>, links: &Links, cx: &mut App) {
    let at = visit(links, cx);
    cx.set_global(RemovalAsk(release.map(|release| (release, at))));
    cx.refresh_windows();
}

/// What a package page offers for a registry release that is in the
/// library: removing it, after one confirmation, through the owner. Only the
/// pinned page of a tree the registry source owns (never a project of yours,
/// never a past release viewed from another pin).
pub(crate) fn page_removal(dossier: &crate::model::pages::PackageDossier, links: &Links, view: EntityId, measure: &Measure, admit: Rc<dyn Fn(&mut App) -> bool>, cx: &mut App) -> Option<AnyElement> {
    use crate::model::pages::RecordSource;
    use facet::controls::button;
    let record = dossier.record.known().filter(|record| record.source == RecordSource::PublishedSource)?;
    if !dossier.package.is_local() || dossier.package.release_origin().is_some() {
        return None;
    }
    let release = Release::new(record.name.as_ref(), record.version.known()?).ok()?;
    if acquire::stage(&release, Asker::View(view), cx).is_some_and(|stage| stage.working()) {
        return None;
    }
    let removal = acquire::removal(&release, Asker::View(view), cx);
    let palette = cx.palette();
    let words = |text: String, cx: &App| super::kit::text(ty::SMALL, measure, cx.palette().ink2).child(text);
    let dispatch = {
        let (links, release, page) = (links.clone(), release.clone(), dossier.package.clone());
        Rc::new(move |cx: &mut App| {
            ask_removal(None, &links, cx);
            links.dispatch(Intent::RemoveRelease { release: release.clone(), page: page.clone() }, cx);
        })
    };
    let mut row = div().flex().flex_wrap().items_center().gap(measure.space(Space::Base));
    let caption = match &removal {
        Some(Removal::Removed(_)) => {
            let (links, again) = (links.clone(), release.clone());
            row = row.child(words(format!("{release} and its indexes are out of your library. Its source stays on this machine."), cx))
                .child(button("package-readd", "Add it again", measure).size(facet::Control::Small)
                    .when_current(Rc::clone(&admit))
                    .on_click(move |_, cx| links.dispatch(Intent::AddRelease(again.clone()), cx)));
            "REMOVED FROM YOUR LIBRARY"
        }
        Some(removal) if removal.working() => {
            row = row.child(button("package-remove", format!("Removing {release}…"), measure).danger().size(facet::Control::Small).busy(true).disabled(true));
            "REMOVING"
        }
        Some(Removal::Failed(why)) => {
            let retry = Rc::clone(&dispatch);
            row = row.child(words(format!("Not removed: {why}"), cx))
                .child(button("package-remove", "Try removing again", measure).danger().size(facet::Control::Small)
                    .when_current(Rc::clone(&admit))
                    .on_click(move |_, cx| retry(cx)));
            "IN YOUR LIBRARY"
        }
        _ if asking(&release, links, cx) => {
            let confirm = Rc::clone(&dispatch);
            row = row.child(words(format!("Remove {release} and its indexes from your library? Its source stays on this machine, so you can add it again."), cx))
                .child(button("package-remove-confirm", "Remove", measure).danger().size(facet::Control::Small)
                    .aria_label(format!("Remove {release} from your library"))
                    .when_current(Rc::clone(&admit))
                    .on_click(move |_, cx| confirm(cx)))
                .child({
                    let links = links.clone();
                    button("package-remove-keep", "Keep", measure).ghost().size(facet::Control::Small)
                        .on_click(move |_, cx| ask_removal(None, &links, cx))
                });
            "IN YOUR LIBRARY"
        }
        _ => {
            let (ask, links) = (release.clone(), links.clone());
            row = row.child(button("package-remove", "Remove from library", measure).ghost().size(facet::Control::Small)
                .aria_label(format!("Remove {release} from your library"))
                .when_current(Rc::clone(&admit))
                .on_click(move |_, cx| ask_removal(Some(ask.clone()), &links, cx)));
            "IN YOUR LIBRARY"
        }
    };
    Some(
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Base))
            .pb(measure.space(Space::Wide))
            .child(super::kit::text(ty::CAPTION, measure, palette.ink2).child(caption))
            .child(row)
            .into_any_element(),
    )
}

/// The offer a package page makes when the release it names is not in the
/// library: a registry package reached through a dependency or from Find,
/// whose own address the owner has not indexed. `None` for a local project,
/// and for a page that already reads indexed names.
pub(crate) fn page_offer(dossier: &crate::model::pages::PackageDossier, links: &Links, view: EntityId, measure: &Measure, admit: Rc<dyn Fn(&mut App) -> bool>, cx: &mut App) -> Option<AnyElement> {
    if dossier.outline.known().is_some_and(|outline| outline.count() > 0) {
        return None;
    }
    // A purl, or a registry release's own tree (an earlier release of a
    // package the library holds, read from the comb); never a person's own
    // project.
    let release = dossier.package.release()?;
    let source = crate::host::registry::composed()?.source;
    let offer = Offer { availability: source.availability(&release), release, library: None };
    let palette = cx.palette();
    let actions = add_actions(links, view).admitted(Rc::new(move |_, cx| admit(cx)));
    Some(
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Base))
            .pb(measure.space(Space::Wide))
            .child(super::kit::text(ty::CAPTION, measure, palette.ink2).child("NOT IN YOUR LIBRARY"))
            .child(add_control("package-offer", &facet_offer(&offer), &actions, measure, cx))
            .into_any_element(),
    )
}
