//! The shell's side of adding a release (W-Acquire): facet's add control is
//! handed the state of an addition and two actions, the same on every
//! surface that offers one (Find, a package page's header).

use super::kit::package_route;
use super::region::Links;
use crate::model::pages::PackageRef;
use crate::model::release::{Availability, Offer, Release};
use crate::navigation::Intent;
use crate::runtime::acquire::{self, Stage};
use crate::runtime::offload::Asker;
use facet::browse::acquire::{AddActions, Adding, Place, Step, add_control};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{AnyElement, App, EntityId, IntoElement, ParentElement, SharedString, Styled, div};
use std::rc::Rc;

/// The actions for the view `view`: it redraws as an addition moves.
pub(crate) fn add_actions(links: &Links, view: EntityId) -> AddActions {
    let (adding, opening) = (links.clone(), links.clone());
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
    }
}

/// Where adding the release at `purl` stands, in facet's words.
fn adding_state(purl: &SharedString, asker: Asker, cx: &mut App) -> Adding {
    let Some(release) = Release::from_purl(purl) else { return Adding::Idle };
    match acquire::stage(&release, asker, cx) {
        None => Adding::Idle,
        Some(Stage::Queued) => Adding::Working(Step::Waiting),
        Some(Stage::Resolving) => Adding::Working(Step::Resolving),
        Some(Stage::Unpacking) => Adding::Working(Step::Unpacking),
        Some(Stage::Indexing) => Adding::Working(Step::Indexing),
        Some(Stage::Added(package)) => Adding::Added { open: package.as_str().to_owned().into() },
        Some(Stage::Failed(reason)) => Adding::Failed(reason.to_string().into()),
    }
}

/// Facet's offer for `offer`.
pub(crate) fn facet_offer(offer: &Offer) -> facet::browse::acquire::Offer {
    facet::browse::acquire::Offer {
        release: offer.release.purl().into(),
        label: offer.release.to_string().into(),
        place: match offer.availability {
            Availability::Unpacked(_) => Place::Unpacked,
            Availability::Archive(_) => Place::Archive,
            Availability::Download => Place::Download,
        },
        library: offer.library.as_ref().map(|package| package.as_str().to_owned().into()),
    }
}

/// The offer a package page makes when the release it names is not in the
/// library: a registry package reached through a dependency or from Find,
/// whose own address the owner has not indexed. `None` for a local project,
/// and for a page that already reads indexed names.
pub(crate) fn page_offer(dossier: &crate::model::pages::PackageDossier, links: &Links, view: EntityId, measure: &Measure, cx: &mut App) -> Option<AnyElement> {
    if dossier.package.is_local() || dossier.outline.known().is_some_and(|outline| outline.count() > 0) {
        return None;
    }
    let release = Release::from_purl(dossier.package.as_str())?;
    let source = crate::host::registry::composed()?.source;
    let offer = Offer { availability: source.availability(&release), release, library: None };
    let palette = cx.palette();
    let actions = add_actions(links, view);
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
