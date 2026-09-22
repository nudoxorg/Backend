//! Pure route/state reducer.

use super::intent::{Effect, EngineCommand, Intent, Reduction};
use super::route::{OrbitRoute, Overlay, Route, SourceRoute};
use crate::core::ProjectId;

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
        Intent::OpenSource {
            package,
            coordinate,
            line,
            object,
        } => {
            navigate(
                &mut next,
                Route::Source(SourceRoute {
                    project: project_context(snapshot),
                    package,
                    page: coordinate,
                    line,
                    selected: object,
                }),
            );
            effects.push(Effect::Persist);
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
        Intent::Action(action) => {
            if let Some(intent) = action.intent() {
                return reduce(&next, intent);
            }
        }
    }
    Reduction {
        snapshot: next,
        effects,
    }
}

fn navigate(snapshot: &mut crate::model::AppSnapshot, route: Route) {
    if snapshot.route() == &route {
        if snapshot.overlay().is_some() {
            let mut session = snapshot.session().clone();
            session.overlay = None;
            *snapshot = snapshot.with_session(session);
        }
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

fn open_overlay(snapshot: &mut crate::model::AppSnapshot, overlay: Overlay) {
    let mut session = snapshot.session().clone();
    session.overlay = Some(overlay);
    *snapshot = snapshot.with_session(session);
}

fn project_context(snapshot: &crate::model::AppSnapshot) -> Option<ProjectId> {
    route_project(snapshot.route()).or_else(|| {
        snapshot
            .session()
            .back
            .to_vec()
            .into_iter()
            .find_map(|route| route_project(&route))
    })
}

fn route_project(route: &Route) -> Option<ProjectId> {
    match route {
        Route::Orbit(OrbitRoute::Project(project)) => Some(project.clone()),
        Route::Orbit(OrbitRoute::Home) => None,
        Route::Package(route) => route.project.clone(),
        Route::Page(route) => route.project.clone(),
        Route::Source(route) => route.project.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::AppSnapshot;
    use crate::navigation::route::{Coordinate, PackageLane, PackageRoute, PageRoute};

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
        let package = Route::Package(PackageRoute {
            project: None,
            package: crate::core::PackageId::new("pkg").expect("package"),
            lane: PackageLane::Overview,
            selected: None,
        });
        let page = Route::Page(PageRoute {
            project: None,
            package: crate::core::PackageId::new("pkg").expect("package"),
            coordinate: Coordinate::new("pkg::Item").expect("coordinate"),
            selected: None,
        });
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
        let package = Route::Package(PackageRoute {
            project: None,
            package: crate::core::PackageId::new("pkg").expect("package"),
            lane: PackageLane::Overview,
            selected: Some(crate::model::ObjectId::test(7)),
        });
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
    fn source_descent_retains_the_current_project_context() {
        let initial = snapshot();
        let project = crate::core::ProjectId::test(9).expect("project");
        let package = crate::core::PackageId::new("pkg").expect("package");
        let page = Route::Page(crate::navigation::route::PageRoute {
            project: Some(project.clone()),
            package: package.clone(),
            coordinate: crate::navigation::route::Coordinate::new("pkg::Item").expect("coordinate"),
            selected: None,
        });
        let page = reduce(&initial, Intent::Navigate(page)).snapshot;
        let source = reduce(
            &page,
            Intent::OpenSource {
                package,
                coordinate: crate::navigation::route::Coordinate::new("pkg::Item")
                    .expect("coordinate"),
                line: 4,
                object: None,
            },
        )
        .snapshot;
        let Route::Source(source) = source.route() else {
            panic!("source route")
        };
        assert_eq!(source.project.as_ref(), Some(&project));
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
