//! Pure reducer for local-first workspace lifecycle and settings.
//!
//! Keeping this branch separate lets the route reducer stay focused on
//! navigation history while onboarding, the shelf, and settings share one
//! lifecycle transition table.

use super::intent::{Effect, Intent, Reduction};
use super::route::{Overlay, SettingsPage};
use crate::core::{LocalProjectId, ResourceIdentity};
use crate::model::{
    AppSnapshot, AppearancePreference, ConnectionStatus, PrivacyPreference, ProjectPhase,
    WorkspaceProject,
};
use std::sync::Arc;

/// Reduces one workspace-owned intent, returning `None` for route-owned work.
pub(super) fn reduce(snapshot: &AppSnapshot, intent: &Intent) -> Option<Reduction> {
    let mut next = snapshot.clone();
    let mut effects = Vec::new();
    match intent {
        Intent::ToggleShelf => {
            let mut settings = next.settings().clone();
            settings.shelf_open = !settings.shelf_open;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::ToggleContext => {
            let mut settings = next.settings().clone();
            settings.context_open = !settings.context_open;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::ToggleAppearance => {
            let mut settings = next.settings().clone();
            settings.appearance = match settings.appearance {
                AppearancePreference::Abyss => AppearancePreference::Glacier,
                AppearancePreference::Glacier => AppearancePreference::Abyss,
            };
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::SetTextScale { up } => {
            let mut settings = next.settings().clone();
            settings.text_scale = settings.text_scale.step(*up);
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::TogglePrivacy => {
            let mut settings = next.settings().clone();
            settings.privacy = match settings.privacy {
                PrivacyPreference::LocalOnly => PrivacyPreference::RegistryMetadata,
                PrivacyPreference::RegistryMetadata => PrivacyPreference::LocalOnly,
            };
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::ToggleAdvisories => {
            let mut settings = next.settings().clone();
            settings.advisories = !settings.advisories;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::ToggleCache => {
            let mut settings = next.settings().clone();
            settings.cache_enabled = !settings.cache_enabled;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::SetCacheDays { up } => {
            let mut settings = next.settings().clone();
            settings.cache_days = cache_days_step(settings.cache_days, *up);
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::OpenFolderPicker => {}
        Intent::IndexProject {
            project,
            basis,
            request,
        } => {
            let path: Arc<str> = project.as_str().to_owned().into();
            if next
                .workspace()
                .projects
                .iter()
                .any(|item| item.path.as_ref() == path.as_ref())
            {
                next = set_project_phase(&next, path.as_ref(), ProjectPhase::Indexing, None);
                effects.push(Effect::Engine(super::intent::EngineCommand::IndexProject {
                    project: project.clone(),
                    basis: *basis,
                    request: *request,
                }));
                effects.push(Effect::Persist);
            } else {
                let mut workspace = next.workspace().clone();
                workspace.path_error = Some(Arc::from(
                    "That project is no longer on the shelf. Choose it again to reopen it.",
                ));
                next = next.with_workspace(workspace);
                effects.push(Effect::Persist);
            }
        }
        Intent::AddProject { path } => {
            next = admit_project(&next, Arc::clone(path));
            effects.push(Effect::Persist);
        }
        Intent::ActivateProject(project) => {
            let path = project.as_str().to_owned();
            let on_shelf = next
                .workspace()
                .projects
                .iter()
                .any(|item| item.path.as_ref() == path);
            if !on_shelf {
                let mut workspace = next.workspace().clone();
                workspace.path_error = Some(Arc::from(
                    "That project is no longer on the shelf. Choose it again to reopen it.",
                ));
                next = next.with_workspace(workspace);
            } else {
                let mut workspace = next.workspace().clone();
                workspace.active = Some(path.clone().into());
                workspace.path_error = None;
                workspace.projects = workspace
                    .projects
                    .iter()
                    .cloned()
                    .map(|mut item| {
                        item.recent = item.path.as_ref() == path;
                        item
                    })
                    .collect::<Vec<_>>()
                    .into();
                next = next.with_workspace(workspace);
                next = set_project_phase(&next, &path, ProjectPhase::Indexing, None);
                let mut shelf = next.shelf().clone();
                shelf.selected = Some(ResourceIdentity::Local(project.clone()));
                next = next.with_shelf(shelf);
            }
            effects.push(Effect::Persist);
        }
        Intent::RemoveProject(project) => {
            let path = project.as_str();
            let removed_active = next.workspace().active.as_deref() == Some(path);
            let mut workspace = next.workspace().clone();
            workspace.projects = workspace
                .projects
                .iter()
                .filter(|item| item.path.as_ref() != path)
                .cloned()
                .collect::<Vec<_>>()
                .into();
            if workspace.active.as_deref() == Some(path) {
                workspace.active = workspace.projects.first().map(|item| item.path.clone());
            }
            next = next.with_workspace(workspace);
            if removed_active {
                if let Some(fallback) = next.workspace().active.clone() {
                    next =
                        set_project_phase(&next, fallback.as_ref(), ProjectPhase::Indexing, None);
                }
            }
            let mut shelf = next.shelf().clone();
            shelf.items = shelf
                .items
                .iter()
                .filter(|item| item.identity != ResourceIdentity::Local(project.clone()))
                .cloned()
                .collect::<Vec<_>>()
                .into();
            shelf.selected = shelf.items.first().map(|item| item.identity.clone());
            next = next.with_shelf(shelf);
            effects.push(Effect::Persist);
        }
        Intent::RevealProject(_) => {}
        Intent::RetryIndex(project) => {
            next = set_project_phase(&next, project.as_str(), ProjectPhase::Indexing, None);
            effects.push(Effect::Persist);
        }
        Intent::CancelIndex(project) => {
            next = set_project_phase(&next, project.as_str(), ProjectPhase::Cancelled, None);
            effects.push(Effect::Persist);
        }
        Intent::TestConnection => {
            let mut settings = next.settings().clone();
            settings.connection = ConnectionStatus::Testing;
            next = next.with_settings(settings);
            // The root-owned runtime starts the typed probe after reducing
            // this intent. The reducer only records the visible state.
            effects.push(Effect::Persist);
        }
        Intent::ConnectionResult { connected } => {
            let mut settings = next.settings().clone();
            settings.connection = if *connected {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Disconnected
            };
            next = next.with_settings(settings);
        }
        Intent::OpenHelp => {
            let mut session = next.session().clone();
            session.overlay = Some(Overlay::Settings(SettingsPage::Help));
            next = next.with_session(session);
        }
        _ => return None,
    }
    Some(Reduction {
        snapshot: next,
        effects,
    })
}

fn admit_project(snapshot: &AppSnapshot, path: Arc<str>) -> AppSnapshot {
    let path = path.trim();
    if path.is_empty() {
        let mut workspace = snapshot.workspace().clone();
        workspace.path_error = Some(Arc::from("Choose a folder to add to the shelf."));
        return snapshot.with_workspace(workspace);
    }
    let Ok(project) = LocalProjectId::new(path) else {
        let mut workspace = snapshot.workspace().clone();
        workspace.path_error = Some(Arc::from("That folder path is not valid."));
        return snapshot.with_workspace(workspace);
    };
    let mut workspace = snapshot.workspace().clone();
    workspace.path_error = None;
    workspace.active = Some(Arc::from(path));
    let mut projects = workspace.projects.to_vec();
    if let Some(existing) = projects.iter_mut().find(|item| item.path.as_ref() == path) {
        existing.phase = ProjectPhase::Indexing;
        existing.progress = None;
        existing.files_indexed = None;
        existing.error = None;
        existing.recent = true;
    } else {
        projects.push(WorkspaceProject::indexing(Arc::from(path)));
    }
    workspace.projects = projects.into();
    let mut next = snapshot.with_workspace(workspace);
    let mut shelf = next.shelf().clone();
    if !shelf
        .items
        .iter()
        .any(|item| item.identity == ResourceIdentity::Local(project.clone()))
    {
        shelf.items = shelf
            .items
            .iter()
            .cloned()
            .chain([crate::model::ShelfItem {
                identity: ResourceIdentity::Local(project.clone()),
                label: projects_label(path).into(),
                object: crate::model::ObjectId::from_backend(backend_library::object_version(
                    path.as_bytes(),
                )),
            }])
            .collect::<Vec<_>>()
            .into();
    }
    shelf.selected = Some(ResourceIdentity::Local(project));
    next.with_shelf(shelf)
}

fn projects_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Workspace")
        .to_owned()
}

fn cache_days_step(value: u16, up: bool) -> u16 {
    const STEPS: [u16; 5] = [1, 7, 14, 30, 90];
    let index = STEPS
        .iter()
        .position(|step| *step >= value)
        .unwrap_or(STEPS.len().saturating_sub(1));
    if up {
        STEPS[index.saturating_add(1).min(STEPS.len().saturating_sub(1))]
    } else if STEPS[index] >= value {
        STEPS[index.saturating_sub(1)]
    } else {
        STEPS[index]
    }
}

fn set_project_phase(
    snapshot: &AppSnapshot,
    path: &str,
    phase: ProjectPhase,
    error: Option<Arc<str>>,
) -> AppSnapshot {
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = workspace
        .projects
        .iter()
        .cloned()
        .map(|mut item| {
            if item.path.as_ref() == path {
                item.phase = phase;
                item.progress = None;
                item.files_indexed = None;
                item.error = error.clone();
            }
            item
        })
        .collect::<Vec<_>>()
        .into();
    snapshot.with_workspace(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;

    fn snapshot() -> AppSnapshot {
        AppSnapshot::empty(VersionedRoot::new(
            backend_library::view_state_root(&[("root".to_owned(), "one".to_owned())]),
            1,
        ))
    }

    #[test]
    fn adding_the_same_project_reuses_one_typed_shelf_row() {
        let path: Arc<str> = "/tmp/nudox-reducer-project".into();
        let first = reduce(
            &snapshot(),
            &Intent::AddProject {
                path: Arc::clone(&path),
            },
        )
        .expect("workspace intent")
        .snapshot;
        let second = reduce(&first, &Intent::AddProject { path }).expect("workspace intent");

        assert_eq!(second.snapshot.workspace().projects.len(), 1);
        assert_eq!(second.snapshot.shelf().items.len(), 1);
        assert_eq!(
            second.snapshot.workspace().active.as_deref(),
            Some("/tmp/nudox-reducer-project")
        );
    }

    #[test]
    fn index_request_keeps_the_canonical_project_identity_and_root_basis() {
        let path: Arc<str> = "/tmp/nudox-reducer-project".into();
        let admitted = reduce(&snapshot(), &Intent::AddProject { path }).expect("workspace");
        let basis = admitted.snapshot.key();
        let project = LocalProjectId::new("/tmp/nudox-reducer-project").expect("identity");
        let request = super::super::RequestId::new(41);
        let reduction = reduce(
            &admitted.snapshot,
            &Intent::IndexProject {
                project: project.clone(),
                basis,
                request,
            },
        )
        .expect("workspace");

        assert!(matches!(
            reduction.effects.as_slice(),
            [Effect::Engine(super::super::intent::EngineCommand::IndexProject {
                project: actual,
                basis: actual_basis,
                request: actual_request,
            }), Effect::Persist]
                if actual == &project && *actual_basis == basis && *actual_request == request
        ));
    }
}
