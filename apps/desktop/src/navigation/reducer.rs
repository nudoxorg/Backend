//! Pure route/state reducer.

use super::intent::{Effect, EngineCommand, Intent, Reduction};
use super::route::{Overlay, Route};

/// Applies one typed intent without touching GPUI, clocks, files, or sockets.
#[must_use]
pub fn reduce(snapshot: &crate::model::AppSnapshot, intent: Intent) -> Reduction {
    if let Some(reduction) = super::workspace_reducer::reduce(snapshot, &intent) {
        return reduction;
    }
    let mut effects = Vec::new();
    let mut next = snapshot.clone();
    // Going anywhere keeps the place the query is showing; Back and Esc
    // put the place you were on back instead.
    match &intent {
        Intent::Navigate(_) | Intent::ZoomOut | Intent::Tour(_) | Intent::Forward | Intent::SetView(_) | Intent::SetRelease(_) => {
            commit_preview(&mut next);
        }
        Intent::Back | Intent::DismissOverlay if next.session().preview.is_some() => {
            end_preview(&mut next, true);
            return Reduction { snapshot: next, effects };
        }
        _ => {}
    }
    match intent {
        // The window root adds a release (`runtime::acquire`): no state here.
        Intent::Noop | Intent::AddRelease(_) => {}
        Intent::Navigate(route) => {
            navigate(&mut next, route);
            effects.push(Effect::Persist);
        }
        Intent::Preview(route) => {
            let mut session = next.session().clone();
            if session.preview.is_none() {
                session.preview = Some(session.route.clone());
            }
            session.route = route;
            next = next.with_session(session);
        }
        Intent::CommitPreview => {
            if commit_preview(&mut next) {
                effects.push(Effect::Persist);
            }
        }
        Intent::EndPreview => end_preview(&mut next, false),
        Intent::RefineFind { expected, query } => {
            let owns_field = match snapshot.route() {
                Route::Orbit(super::OrbitRoute::Browse(super::BrowseRoute::Find(current))) => expected.as_ref() == Some(current),
                Route::Orbit(super::OrbitRoute::Browse(super::BrowseRoute::FindHome)) => expected.is_none(),
                _ => false,
            };
            if owns_field && expected != query {
                let route = query.map_or(super::BrowseRoute::FindHome, super::BrowseRoute::Find);
                replace(&mut next, Route::Orbit(super::OrbitRoute::Browse(route)));
                effects.push(Effect::Persist);
            }
        }
        Intent::SetView(view) => {
            if let Some(route) = snapshot.route().with_view(view) {
                replace(&mut next, route);
                effects.push(Effect::Persist);
            }
        }
        Intent::Tour(_) => {
            navigate(&mut next, Route::World);
            effects.push(Effect::Persist);
        }
        Intent::Hold(held) => {
            let mut session = next.session().clone();
            // The first card ever held is whispered once per install.
            if !session.whispered {
                session.whispered = true;
                session.whisper = Some(held.clone());
            }
            session.hand = session.hand.hold(held);
            next = next.with_session(session);
            effects.push(Effect::Persist);
        }
        Intent::LetGo(held) => {
            let mut session = next.session().clone();
            session.hand = session.hand.let_go(&held);
            next = next.with_session(session);
            effects.push(Effect::Persist);
        }
        Intent::TouchHeld(held, at) => {
            let mut session = next.session().clone();
            session.hand = session.hand.touch(&held, at);
            next = next.with_session(session);
            effects.push(Effect::Persist);
        }
        Intent::SetRelease(at) => {
            let route = snapshot.route().with_release(at);
            if &route != snapshot.route() {
                replace(&mut next, route);
                effects.push(Effect::Persist);
            }
        }
        Intent::ZoomOut => {
            if let Some(route) = snapshot.route().zoom_out() {
                navigate(&mut next, route);
                effects.push(Effect::Persist);
                if let Some(selected) = snapshot.route().selected() {
                    let mut session = next.session().clone();
                    session.selected = Some(super::route::Selection::Object(selected));
                    next = next.with_session(session);
                }
            }
        }
        Intent::Back => {
            let mut session = next.session().clone();
            session.overlay = None;
            if let Some((previous, back)) = session.back.pop() {
                let current = session.route.clone();
                let forward = session.forward.push(current);
                session.route = previous;
                session.pending_selection = None;
                session.back = back;
                session.forward = forward;
                next = next.with_session(session);
                effects.push(Effect::Persist);
            }
        }
        Intent::Forward => {
            let mut session = next.session().clone();
            session.overlay = None;
            if let Some((forward_route, forward)) = session.forward.pop() {
                let current = session.route.clone();
                let back = session.back.push(current);
                session.route = forward_route;
                session.pending_selection = None;
                session.back = back;
                session.forward = forward;
                next = next.with_session(session);
                effects.push(Effect::Persist);
            }
        }
        Intent::Select(object) => {
            let mut session = next.session().clone();
            session.selected = Some(super::route::Selection::Object(object));
            session.pending_selection = None;
            next = next.with_session(session);
        }
        Intent::SelectDocument(document) => {
            let mut session = next.session().clone();
            session.selected = Some(super::route::Selection::Document(document));
            session.pending_selection = None;
            next = next.with_session(session);
        }
        Intent::OpenCommandPalette => {
            open_overlay(&mut next, Overlay::CommandPalette);
        }
        Intent::DismissOverlay => {
            if snapshot.overlay().is_some() {
                let mut session = next.session().clone();
                let persist = matches!(session.overlay, Some(Overlay::Settings(_)));
                session.overlay = None;
                next = next.with_session(session);
                if persist {
                    effects.push(Effect::Persist);
                }
            }
        }
        Intent::OpenSettings(page) => {
            open_overlay(&mut next, Overlay::Settings(page));
            effects.push(Effect::Persist);
        }
        Intent::ToggleReducedMotion => {
            let mut settings = next.settings().clone();
            settings.reduced_motion = !settings.reduced_motion;
            settings.motion = if settings.reduced_motion {
                crate::model::MotionPreference::Reduced
            } else {
                crate::model::MotionPreference::System
            };
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::Stop => effects.push(Effect::CancelAll),
        Intent::RefreshRoot { basis, request } => {
            effects.push(Effect::Engine(EngineCommand::ReadRoot { basis, request }));
        }
        Intent::RefreshObject {
            object,
            delta,
            basis,
            request,
        } => {
            effects.push(Effect::Engine(EngineCommand::ReadObject {
                object,
                delta,
                basis,
                request,
            }));
        }
        Intent::RefreshSurface {
            command,
            basis,
            request,
        } => {
            effects.push(Effect::Engine(EngineCommand::RefreshSurface {
                command,
                basis,
                request,
            }));
        }
        Intent::RefreshLocalPackage {
            project,
            basis,
            request,
        } => {
            effects.push(Effect::Engine(EngineCommand::ReadLocalPackage {
                project,
                basis,
                request,
            }));
        }
        Intent::Action(action) => {
            if let Some(intent) = action.intent() {
                return reduce(&next, intent);
            }
        }
        Intent::ToggleShelf
        | Intent::ToggleContext
        | Intent::ToggleAppearance
        | Intent::SetAppearance(_)
        | Intent::Zoom { .. }
        | Intent::ZoomTo { .. }
        | Intent::SetDensity(_)
        | Intent::SetContrast(_)
        | Intent::SetMotion(_)
        | Intent::OpenInbox
        | Intent::TogglePrivacy
        | Intent::ToggleAdvisories
        | Intent::ToggleCache
        | Intent::SetCacheDays { .. }
        | Intent::OpenAddProject
        | Intent::OpenFolderPicker
        | Intent::FolderPickerResult { .. }
        | Intent::IndexProject { .. }
        | Intent::AddProject { .. }
        | Intent::RejectProjectPath { .. }
        | Intent::ActivateProject(_)
        | Intent::RemoveProject(_)
        | Intent::RevealProject(_)
        | Intent::OpenSource { .. }
        | Intent::RetryIndex(_)
        | Intent::CancelIndex(_)
        | Intent::TestConnection
        | Intent::ConnectionResult { .. }
        | Intent::OwnerReady { .. }
        | Intent::DismissNote(_)
        | Intent::LibraryRebuilding { .. }
        | Intent::WindowResized { .. }
        | Intent::OpenHelp => {
            unreachable!("workspace reducer owns workspace intents")
        }
    }
    Reduction {
        snapshot: next,
        effects,
    }
}

fn navigate(snapshot: &mut crate::model::AppSnapshot, route: Route) {
    // Arriving at the place you are (another view, another line) is not
    // navigation: it replaces the entry, so Back still leaves the place.
    if snapshot.route().same_place(&route) {
        replace(snapshot, route);
        return;
    }
    let mut session = snapshot.session().clone();
    let back = session.back.push(session.route.clone());
    session.route = route;
    session.pending_selection = None;
    session.overlay = None;
    session.back = back;
    session.forward = Default::default();
    *snapshot = snapshot.with_session(session);
}

/// Keeps the previewed place: the place you were on becomes Back (unless
/// the preview is that same place). The overlay stays as it is.
fn commit_preview(snapshot: &mut crate::model::AppSnapshot) -> bool {
    let mut session = snapshot.session().clone();
    let Some(origin) = session.preview.take() else { return false };
    if !origin.same_place(&session.route) {
        session.back = session.back.push(origin);
        session.forward = Default::default();
    }
    *snapshot = snapshot.with_session(session);
    true
}

/// Puts the place you were on back; `close` also closes the query.
fn end_preview(snapshot: &mut crate::model::AppSnapshot, close: bool) {
    let mut session = snapshot.session().clone();
    if let Some(origin) = session.preview.take() {
        session.route = origin;
    }
    if close {
        session.overlay = None;
    }
    *snapshot = snapshot.with_session(session);
}

/// Replaces the current entry without touching back/forward history.
fn replace(snapshot: &mut crate::model::AppSnapshot, route: Route) {
    let mut session = snapshot.session().clone();
    session.route = route;
    session.pending_selection = None;
    session.overlay = None;
    *snapshot = snapshot.with_session(session);
}

fn open_overlay(snapshot: &mut crate::model::AppSnapshot, overlay: Overlay) {
    let mut session = snapshot.session().clone();
    session.overlay = Some(overlay);
    *snapshot = snapshot.with_session(session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::AppSnapshot;
    use crate::navigation::route::{
        Coordinate, OrbitRoute, PackageLane, PackageRoute, ReleaseId, SymbolRoute, View,
    };

    fn package_route(selected: Option<crate::model::ObjectId>) -> Route {
        Route::Package(PackageRoute {
            project: None,
            package: crate::core::PackageId::new("pkg").expect("package"),
            lane: PackageLane::Overview,
            selected,
            at: None,
        })
    }

    fn symbol_route(name: &str, view: View) -> Route {
        Route::Symbol(SymbolRoute {
            project: None,
            package: crate::core::PackageId::new("pkg").expect("package"),
            id: Coordinate::new(&format!("pkg::{name}")).expect("coordinate"),
            at: None,
            view,
            line: None,
            selected: None,
        })
    }

    fn snapshot() -> AppSnapshot {
        AppSnapshot::empty(VersionedRoot::synthetic(
            backend_library::view_state_root(&[("root".to_owned(), "one".to_owned())]),
            1,
        ))
    }

    #[test]
    fn noop_is_an_identity_law() {
        let value = snapshot();
        let reduced = reduce(&value, Intent::Noop);
        assert_eq!(reduced.snapshot, value);
        assert!(reduced.effects.is_empty());
    }

    #[test]
    fn find_typing_keeps_one_history_stop_and_stale_callbacks_cannot_steal_navigation() {
        use crate::model::pages::SearchQuery;
        use crate::navigation::{BrowseRoute, OrbitRoute};
        let initial = snapshot();
        let first = SearchQuery::new("tom", 50).unwrap();
        let final_query = SearchQuery::new("toml", 50).unwrap();
        let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(first.clone())));
        let opened = reduce(&initial, Intent::Navigate(route)).snapshot;
        let refined = reduce(&opened, Intent::RefineFind { expected: Some(first.clone()), query: Some(final_query.clone()) }).snapshot;
        assert_eq!(refined.route(), &Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(final_query.clone()))));
        assert_eq!(reduce(&refined, Intent::Back).snapshot.route(), initial.route());
        let stale = reduce(&refined, Intent::RefineFind { expected: Some(first.clone()), query: Some(SearchQuery::new("old", 50).unwrap()) });
        assert_eq!(stale.snapshot, refined);
        assert!(stale.effects.is_empty());
        let departed = reduce(&refined, Intent::Navigate(package_route(None))).snapshot;
        let late = reduce(&departed, Intent::RefineFind { expected: Some(final_query.clone()), query: Some(first) });
        assert_eq!(late.snapshot, departed);
        assert!(late.effects.is_empty());
        let cleared = reduce(&refined, Intent::RefineFind { expected: Some(final_query), query: None }).snapshot;
        assert_eq!(cleared.route(), &Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome)));
        assert_eq!(reduce(&cleared, Intent::Back).snapshot.route(), initial.route());
    }

    #[test]
    fn route_navigation_is_typed_and_back_forward_is_deterministic() {
        let initial = snapshot();
        let package = package_route(None);
        let page = symbol_route("Item", View::Page);
        let a = reduce(&initial, Intent::Navigate(package)).snapshot;
        let b = reduce(&a, Intent::Navigate(page)).snapshot;
        let c = reduce(&b, Intent::Back).snapshot;
        assert_eq!(c.route(), a.route());
        let d = reduce(&c, Intent::Forward).snapshot;
        assert_eq!(d.route(), b.route());
    }

    #[test]
    fn zoom_out_retains_selection_and_overlay_does_not_change_content_history() {
        let initial = snapshot();
        let package = package_route(Some(crate::model::ObjectId::test(7)));
        let page = reduce(&initial, Intent::Navigate(package)).snapshot;
        let zoomed = reduce(&page, Intent::ZoomOut).snapshot;
        assert!(matches!(zoomed.route(), Route::Orbit(OrbitRoute::Home)));
        assert_eq!(
            zoomed.session().selected,
            Some(super::super::route::Selection::Object(
                crate::model::ObjectId::test(7)
            ))
        );

        let opened = reduce(&page, Intent::OpenCommandPalette).snapshot;
        assert_eq!(opened.route(), page.route());
        assert_eq!(opened.overlay(), Some(Overlay::CommandPalette));
        assert_eq!(opened.session().back, page.session().back);
        let restored = reduce(&opened, Intent::DismissOverlay).snapshot;
        assert!(matches!(restored.route(), Route::Package(_)));
        assert_eq!(restored.overlay(), None);
        assert_eq!(restored.session().back, page.session().back);
    }

    #[test]
    fn switching_view_replaces_the_entry_and_back_leaves_the_declaration() {
        let package = reduce(&snapshot(), Intent::Navigate(package_route(None))).snapshot;
        let page = reduce(&package, Intent::Navigate(symbol_route("Item", View::Page))).snapshot;
        let depth = page.session().back.len();
        let code = reduce(&page, Intent::SetView(View::Code)).snapshot;
        assert_eq!(code.route(), &symbol_route("Item", View::Code));
        let graph = reduce(&code, Intent::SetView(View::Graph)).snapshot;
        assert_eq!(graph.session().back.len(), depth, "no view switch pushed history");
        // Navigating to the place you are (another view) is also a replace.
        let again = reduce(&graph, Intent::Navigate(symbol_route("Item", View::Page))).snapshot;
        assert_eq!(again.session().back.len(), depth);
        // Back leaves the declaration for the package, whatever the view.
        let back = reduce(&again, Intent::Back).snapshot;
        assert_eq!(back.route(), package.route());
        // Forward returns to the declaration in the view it was left in.
        let forward = reduce(&back, Intent::Forward).snapshot;
        assert_eq!(forward.route(), &symbol_route("Item", View::Page));
        // A view switch on a place with no views changes nothing.
        let unchanged = reduce(&package, Intent::SetView(View::Code));
        assert_eq!(unchanged.snapshot.route(), package.route());
        assert!(unchanged.effects.is_empty());
    }

    #[test]
    fn a_release_is_viewed_in_place_and_returns_to_the_pin() {
        let page = reduce(&snapshot(), Intent::Navigate(symbol_route("Item", View::Page))).snapshot;
        let depth = page.session().back.len();
        let release = ReleaseId::new("1.0.190").expect("release");
        let viewing = reduce(&page, Intent::SetRelease(Some(release.clone()))).snapshot;
        assert_eq!(viewing.route().at(), Some(&release));
        assert_eq!(viewing.session().back.len(), depth, "re-scoping is not navigation");
        let pinned = reduce(&viewing, Intent::SetRelease(None)).snapshot;
        assert_eq!(pinned.route(), page.route());
    }

    #[test]
    fn local_package_refresh_is_one_local_read_effect_without_a_snapshot_change()
    -> Result<(), crate::core::IdentityError> {
        let value = snapshot();
        let basis = value.key().observed_at(5);
        let project = crate::core::LocalProjectId::new("/tmp/nudox-local")?;
        let request = super::super::RequestId::new(6);
        let reduced = reduce(
            &value,
            Intent::RefreshLocalPackage {
                project: project.clone(),
                basis,
                request,
            },
        );
        assert_eq!(reduced.snapshot, value);
        assert_eq!(
            reduced.effects,
            [Effect::Engine(EngineCommand::ReadLocalPackage {
                project,
                basis,
                request,
            })]
        );
        Ok(())
    }

    /// The query walks results without writing history; Esc puts you back
    /// where you were, with Back exactly as it was before you typed.
    #[test]
    fn a_walked_preview_is_not_history_and_esc_puts_you_back() {
        let at = reduce(&snapshot(), Intent::Navigate(package_route(None))).snapshot;
        let back_before = at.session().back.clone();
        let querying = reduce(&at, Intent::OpenCommandPalette).snapshot;
        let first = reduce(&querying, Intent::Preview(symbol_route("A", View::Page))).snapshot;
        let second = reduce(&first, Intent::Preview(symbol_route("B", View::Page))).snapshot;
        assert_eq!(second.route(), &symbol_route("B", View::Page), "the reader shows the walked result");
        assert_eq!(second.committed_route(), &package_route(None), "the place you were on is what is kept");
        assert_eq!(second.session().back, back_before, "walking results writes no history");
        assert_eq!(second.overlay(), Some(Overlay::CommandPalette), "a preview keeps the query open");
        let returned = reduce(&second, Intent::DismissOverlay).snapshot;
        assert_eq!(returned.route(), &package_route(None), "Esc puts the place you were on back");
        assert_eq!(returned.session().preview, None);
        assert_eq!(returned.overlay(), None);
        assert_eq!(returned.session().back, back_before);
    }

    /// ↵ keeps the previewed place, and Back then leads to where you were.
    #[test]
    fn committing_a_preview_makes_where_you_were_the_way_back() {
        let at = reduce(&snapshot(), Intent::Navigate(package_route(None))).snapshot;
        let first = reduce(&at, Intent::Preview(symbol_route("A", View::Page))).snapshot;
        let second = reduce(&first, Intent::Preview(symbol_route("B", View::Page))).snapshot;
        let kept = reduce(&second, Intent::CommitPreview);
        assert!(kept.effects.contains(&Effect::Persist));
        let kept = kept.snapshot;
        assert_eq!(kept.route(), &symbol_route("B", View::Page));
        assert_eq!(kept.session().preview, None);
        assert_eq!(reduce(&kept, Intent::Back).snapshot.route(), &package_route(None), "Back skips every walked result");
    }

    /// Following a link inside a previewed page keeps both places: Back
    /// returns to the previewed page, then to where the query started.
    #[test]
    fn a_link_followed_from_a_preview_keeps_the_preview_and_its_origin() {
        let at = reduce(&snapshot(), Intent::Navigate(package_route(None))).snapshot;
        let previewing = reduce(&at, Intent::Preview(symbol_route("A", View::Page))).snapshot;
        let followed = reduce(&previewing, Intent::Navigate(symbol_route("C", View::Page))).snapshot;
        assert_eq!(followed.session().preview, None);
        let back_one = reduce(&followed, Intent::Back).snapshot;
        assert_eq!(back_one.route(), &symbol_route("A", View::Page));
        assert_eq!(reduce(&back_one, Intent::Back).snapshot.route(), &package_route(None));
    }

    /// An emptied query shows where you were again but stays open.
    #[test]
    fn ending_a_preview_without_closing_keeps_the_query_open() {
        let at = reduce(&snapshot(), Intent::Navigate(package_route(None))).snapshot;
        let querying = reduce(&at, Intent::OpenCommandPalette).snapshot;
        let previewing = reduce(&querying, Intent::Preview(symbol_route("A", View::Page))).snapshot;
        let emptied = reduce(&previewing, Intent::EndPreview).snapshot;
        assert_eq!(emptied.route(), &package_route(None));
        assert_eq!(emptied.overlay(), Some(Overlay::CommandPalette));
        assert_eq!(emptied.session().preview, None);
    }

    #[test]
    fn object_refresh_preserves_the_exact_caller_basis() {
        let value = snapshot();
        let basis = value.key().observed_at(41);
        let object = crate::model::ObjectId::test(3);
        let delta = crate::model::DeltaId::test(8);
        let reduced = reduce(
            &value,
            Intent::RefreshObject {
                object,
                delta,
                basis,
                request: super::super::RequestId::new(2),
            },
        );
        assert!(matches!(
            reduced.effects.as_slice(),
            [Effect::Engine(EngineCommand::ReadObject {
                object: actual_object,
                delta: actual_delta,
                basis: actual_basis,
                ..
            })] if *actual_object == object && *actual_delta == delta && *actual_basis == basis
        ));
    }
}
