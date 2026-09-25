//! Pure route/state reducer.

use super::intent::{Effect, EngineCommand, Intent, Reduction};
use super::route::{OrbitRoute, Overlay, Route};

/// Applies one typed intent without touching GPUI, clocks, files, or sockets.
#[must_use]
pub fn reduce(snapshot: &crate::model::AppSnapshot, intent: Intent) -> Reduction {
    if let Some(reduction) = super::workspace_reducer::reduce(snapshot, &intent) {
        return reduction;
    }
    let mut effects = Vec::new();
    let mut next = snapshot.clone();
    match intent {
        Intent::Noop => {}
        Intent::Navigate(route) => {
            navigate(&mut next, route);
            effects.push(Effect::Persist);
        }
        Intent::SetView(view) => {
            if let Some(route) = snapshot.route().with_view(view) {
                replace(&mut next, route);
                effects.push(Effect::Persist);
            }
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
                session.back = back;
                session.forward = forward;
                next = next.with_session(session);
                effects.push(Effect::Persist);
            }
        }
        Intent::Select(object) => {
            let mut session = next.session().clone();
            session.selected = Some(super::route::Selection::Object(object));
            next = next.with_session(session);
        }
        Intent::SelectDocument(document) => {
            let mut session = next.session().clone();
            session.selected = Some(super::route::Selection::Document(document));
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
        | Intent::RetryIndex(_)
        | Intent::CancelIndex(_)
        | Intent::TestConnection
        | Intent::ConnectionResult { .. }
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
    session.overlay = None;
    session.back = back;
    session.forward = Default::default();
    *snapshot = snapshot.with_session(session);
}

/// Replaces the current entry without touching back/forward history.
fn replace(snapshot: &mut crate::model::AppSnapshot, route: Route) {
    let mut session = snapshot.session().clone();
    session.route = route;
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
    use crate::navigation::route::{Coordinate, PackageLane, PackageRoute, ReleaseId, SymbolRoute, View};

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
