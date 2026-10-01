//! Pure reducer for local-first workspace lifecycle and settings.
//!
//! Keeping this branch separate lets the route reducer stay focused on
//! navigation history while onboarding, the shelf, and settings share one
//! lifecycle transition table.

use super::intent::{Effect, FolderPickerOutcome, Intent, Reduction, RequestId};
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
                AppearancePreference::Glacier | AppearancePreference::System => {
                    AppearancePreference::Abyss
                }
            };
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::SetAppearance(appearance) => {
            let mut settings = next.settings().clone();
            settings.appearance = *appearance;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::Zoom { display, step } => {
            let mut settings = next.settings().clone();
            settings.zoom = settings.zoom.step(display, *step);
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::ZoomTo { display, percent } => {
            let mut settings = next.settings().clone();
            settings.zoom = settings.zoom.to_percent(display, *percent);
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::SetDensity(density) => {
            let mut settings = next.settings().clone();
            settings.density = *density;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::SetContrast(contrast) => {
            let mut settings = next.settings().clone();
            settings.contrast = *contrast;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::SetMotion(motion) => {
            let mut settings = next.settings().clone();
            settings.motion = *motion;
            settings.reduced_motion = *motion == crate::model::MotionPreference::Reduced;
            next = next.with_settings(settings);
            effects.push(Effect::Persist);
        }
        Intent::OpenInbox => {
            let mut session = next.session().clone();
            session.overlay = Some(Overlay::Inbox);
            next = next.with_session(session);
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
        Intent::OpenAddProject => {
            let mut session = next.session().clone();
            session.overlay = Some(Overlay::AddProject);
            next = next.with_session(session);
            let mut workspace = next.workspace().clone();
            workspace.path_error = None;
            next = next.with_workspace(workspace);
        }
        Intent::OpenFolderPicker => {}
        Intent::FolderPickerResult { outcome } => match outcome {
            FolderPickerOutcome::Selected(paths) => {
                for path in paths.iter() {
                    next = admit_project_path(&next, path);
                }
                // The dialog the picker was opened from has done its work.
                if next.overlay() == Some(Overlay::AddProject) {
                    let mut session = next.session().clone();
                    session.overlay = None;
                    next = next.with_session(session);
                }
                effects.push(Effect::Persist);
            }
            FolderPickerOutcome::Cancelled => {}
            FolderPickerOutcome::Unavailable(message) | FolderPickerOutcome::Failed(message) => {
                let mut workspace = next.workspace().clone();
                workspace.path_error = Some(Arc::clone(message));
                next = next.with_workspace(workspace);
                effects.push(Effect::Persist);
            }
        },
        Intent::IndexProject {
            project,
            basis,
            request,
        } => {
            if next
                .workspace()
                .projects
                .iter()
                .any(|item| item.id == *project)
            {
                next =
                    set_project_phase(&next, project, ProjectPhase::Indexing, None, Some(*request));
                // This compatibility command reaches Session::index at the
                // service boundary. The package-owned ProjectIngest receipt
                // transport will replace this effect once it lands; until
                // then terminal state still comes from the live service
                // response rather than from a local timer or scanner.
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
        Intent::AddProject { project } => {
            next = admit_project(&next, project.clone());
            effects.push(Effect::Persist);
        }
        Intent::RejectProjectPath { message } => {
            let mut workspace = next.workspace().clone();
            workspace.path_error = Some(Arc::clone(message));
            next = next.with_workspace(workspace);
        }
        Intent::ActivateProject(project) => {
            let on_shelf = next
                .workspace()
                .projects
                .iter()
                .any(|item| item.id == *project);
            if !on_shelf {
                let mut workspace = next.workspace().clone();
                workspace.path_error = Some(Arc::from(
                    "That project is no longer on the shelf. Choose it again to reopen it.",
                ));
                next = next.with_workspace(workspace);
            } else {
                // Choosing a project among several is choosing which one is
                // yours right now. Every project shares the owner's one
                // index, so nothing is indexed again: a ready project stays
                // ready, one that is running keeps running, one that stopped
                // stays stopped until someone presses "Try again".
                let mut workspace = next.workspace().clone();
                workspace.active = Some(project.clone());
                workspace.path_error = None;
                workspace.projects = workspace
                    .projects
                    .iter()
                    .cloned()
                    .map(|mut item| {
                        item.recent = item.id == *project;
                        item
                    })
                    .collect::<Vec<_>>()
                    .into();
                next = next.with_workspace(workspace);
                let mut shelf = next.shelf().clone();
                shelf.selected = Some(ResourceIdentity::Local(project.clone()));
                next = next.with_shelf(shelf);
            }
            effects.push(Effect::Persist);
        }
        Intent::RemoveProject(project) => {
            let removed_request = next
                .workspace()
                .projects
                .iter()
                .find(|item| item.id == *project)
                .and_then(|item| item.request);
            let mut workspace = next.workspace().clone();
            workspace.projects = workspace
                .projects
                .iter()
                .filter(|item| item.id != *project)
                .cloned()
                .collect::<Vec<_>>()
                .into();
            if workspace.active.as_ref() == Some(project) {
                workspace.active = workspace.projects.first().map(|item| item.id.clone());
            }
            next = next.with_workspace(workspace);
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
            if let Some(request) = removed_request {
                effects.push(Effect::Cancel(request));
            }
            effects.push(Effect::Persist);
        }
        Intent::RevealProject(_) | Intent::OpenSource { .. } => {}
        Intent::RetryIndex(project) => {
            let previous_request = next
                .workspace()
                .projects
                .iter()
                .find(|item| item.id == *project)
                .and_then(|item| item.request);
            next = set_project_phase(&next, project, ProjectPhase::Indexing, None, None);
            if let Some(request) = previous_request {
                effects.push(Effect::Cancel(request));
            }
            effects.push(Effect::Persist);
        }
        Intent::CancelIndex(project) => {
            // The owner has not yet supplied a terminal receipt. Keep the
            // row in Cancelling until its ProjectIngest cancellation reply
            // is admitted; painting Cancelled here would fabricate a
            // producer outcome.
            let request = next
                .workspace()
                .projects
                .iter()
                .find(|item| item.id == *project)
                .and_then(|item| item.request);
            next = set_project_phase(&next, project, ProjectPhase::Cancelling, None, request);
            if let Some(request) = request {
                effects.push(Effect::Cancel(request));
            }
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
        Intent::OwnerReady { key, mode } => {
            // The owner's own root, read by the owner thread the moment it
            // answered; no read was ever asked at the unserved one.
            next = next.with_key(*key, None);
            let mut settings = next.settings().clone();
            settings.service_mode = *mode;
            settings.connection = ConnectionStatus::Connected;
            next = next.with_settings(settings);
        }
        Intent::DismissNote(note) => {
            let mut workspace = next.workspace().clone();
            workspace.notes = workspace
                .notes
                .iter()
                .filter(|held| *held != note)
                .cloned()
                .collect::<Vec<_>>()
                .into();
            next = next.with_workspace(workspace);
        }
        Intent::LibraryRebuilding { kept_at } => {
            // The index every ready project stood on is gone from the owner's
            // side (set aside), so "ready" on the shelf is no longer true: each
            // project that still has a folder is indexed again, from nothing.
            let mut workspace = next.workspace().clone();
            workspace.projects = workspace
                .projects
                .iter()
                .cloned()
                .map(|mut project| {
                    if project.phase != ProjectPhase::Missing {
                        project.phase = ProjectPhase::Indexing;
                        project.progress = None;
                        project.files_indexed = None;
                        project.request = None;
                        project.error = None;
                    }
                    project
                })
                .collect::<Vec<_>>()
                .into();
            let note = crate::model::Note::LibraryRebuilding {
                kept_at: Arc::clone(kept_at),
            };
            if !workspace.notes.contains(&note) {
                workspace.notes = workspace
                    .notes
                    .iter()
                    .cloned()
                    .chain([note])
                    .collect::<Vec<_>>()
                    .into();
            }
            next = next.with_workspace(workspace);
            effects.push(Effect::Persist);
        }
        Intent::WindowResized { width, height } => {
            let size = crate::model::WindowSize {
                width: *width,
                height: *height,
            };
            if next.settings().window != Some(size) {
                let mut settings = next.settings().clone();
                settings.window = Some(size);
                next = next.with_settings(settings);
                effects.push(Effect::Persist);
            }
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

fn admit_project(snapshot: &AppSnapshot, project: LocalProjectId) -> AppSnapshot {
    let display = project.display_lossy();
    admit_project_identity(snapshot, project, display)
}

fn admit_project_path(snapshot: &AppSnapshot, path: &std::path::Path) -> AppSnapshot {
    let Ok(project) = LocalProjectId::from_path(path) else {
        let mut workspace = snapshot.workspace().clone();
        workspace.path_error = Some(Arc::from("That folder path is not valid."));
        return snapshot.with_workspace(workspace);
    };
    admit_project_identity(snapshot, project, path.display().to_string())
}

fn admit_project_identity(
    snapshot: &AppSnapshot,
    project: LocalProjectId,
    selected_path: String,
) -> AppSnapshot {
    let canonical_path = project.as_str();
    if canonical_path.is_empty() {
        let mut workspace = snapshot.workspace().clone();
        workspace.path_error = Some(Arc::from("Choose a folder to add to the shelf."));
        return snapshot.with_workspace(workspace);
    }
    let mut workspace = snapshot.workspace().clone();
    workspace.path_error = None;
    workspace.active = Some(project.clone());
    let mut projects = workspace.projects.to_vec();
    if let Some(existing) = projects.iter_mut().find(|item| item.id == project) {
        existing.phase = ProjectPhase::Indexing;
        existing.progress = None;
        existing.files_indexed = None;
        existing.error = None;
        existing.request = None;
        existing.recent = true;
    } else {
        projects.push(WorkspaceProject::indexing_with_display(
            project.clone(),
            selected_path,
        ));
    }
    workspace.projects = projects.into();
    let next = snapshot.with_workspace(workspace);
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
                label: projects_label(canonical_path).into(),
                object: crate::model::ObjectId::from_backend(project.key()),
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
    project: &LocalProjectId,
    phase: ProjectPhase,
    error: Option<Arc<str>>,
    request: Option<RequestId>,
) -> AppSnapshot {
    let mut workspace = snapshot.workspace().clone();
    workspace.projects = workspace
        .projects
        .iter()
        .cloned()
        .map(|mut item| {
            if item.id == *project {
                item.phase = phase;
                item.progress = None;
                item.files_indexed = None;
                item.request = request;
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
        AppSnapshot::empty(VersionedRoot::synthetic(
            backend_library::view_state_root(&[("root".to_owned(), "one".to_owned())]),
            1,
        ))
    }

    #[test]
    fn adding_the_same_project_reuses_one_typed_shelf_row() {
        let project = LocalProjectId::new("/tmp/nudox-reducer-project").expect("identity");
        let first = reduce(
            &snapshot(),
            &Intent::AddProject {
                project: project.clone(),
            },
        )
        .expect("workspace intent")
        .snapshot;
        let second = reduce(&first, &Intent::AddProject { project }).expect("workspace intent");

        assert_eq!(second.snapshot.workspace().projects.len(), 1);
        assert_eq!(second.snapshot.shelf().items.len(), 1);
        assert_eq!(
            second
                .snapshot
                .workspace()
                .active
                .as_ref()
                .map(LocalProjectId::as_str),
            Some("/tmp/nudox-reducer-project")
        );
    }

    #[test]
    fn add_project_overlay_clears_stale_validation_without_persisting_a_draft() {
        let mut workspace = snapshot().workspace().clone();
        workspace.path_error = Some("stale path failure".into());
        let snapshot = snapshot().with_workspace(workspace);
        let opened = reduce(&snapshot, &Intent::OpenAddProject).expect("workspace intent");

        assert_eq!(opened.snapshot.overlay(), Some(Overlay::AddProject));
        assert_eq!(opened.snapshot.workspace().path_error, None);
        assert!(opened.effects.is_empty());

        let rejected = reduce(
            &opened.snapshot,
            &Intent::RejectProjectPath {
                message: "Enter a project folder path.".into(),
            },
        )
        .expect("workspace intent");
        assert_eq!(rejected.snapshot.overlay(), Some(Overlay::AddProject));
        assert_eq!(
            rejected.snapshot.workspace().path_error.as_deref(),
            Some("Enter a project folder path.")
        );
        assert!(rejected.effects.is_empty());
    }

    #[test]
    fn index_intent_submits_the_canonical_project_to_the_service_adapter() {
        let project = LocalProjectId::new("/tmp/nudox-reducer-project").expect("identity");
        let admitted = reduce(
            &snapshot(),
            &Intent::AddProject {
                project: project.clone(),
            },
        )
        .expect("workspace");
        let basis = admitted.snapshot.key();
        let request = RequestId::new(41);
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
            [
                Effect::Engine(super::super::intent::EngineCommand::IndexProject {
                    project: actual,
                    basis: actual_basis,
                    request: actual_request,
                }),
                Effect::Persist
            ] if actual == &project && *actual_basis == basis && *actual_request == request
        ));
        assert_eq!(reduction.snapshot.workspace().projects[0].id, project);
        assert_eq!(
            reduction.snapshot.workspace().projects[0].request,
            Some(request)
        );
        assert_eq!(reduction.snapshot.key(), basis);
    }

    #[test]
    fn native_picker_admits_multiple_paths_as_one_deduplicated_shelf() {
        let paths: Arc<[std::path::PathBuf]> = vec![
            "/tmp/nudox-first-project".into(),
            "/tmp/nudox-second-project".into(),
            "/tmp/nudox-first-project".into(),
        ]
        .into();
        let reduction = reduce(
            &snapshot(),
            &Intent::FolderPickerResult {
                outcome: FolderPickerOutcome::Selected(paths),
            },
        )
        .expect("workspace intent");

        assert_eq!(reduction.snapshot.workspace().projects.len(), 2);
        assert_eq!(reduction.snapshot.shelf().items.len(), 2);
        assert!(
            reduction
                .snapshot
                .workspace()
                .projects
                .iter()
                .all(|project| project.phase == ProjectPhase::Indexing)
        );
    }

    fn two_ready_projects() -> (AppSnapshot, LocalProjectId, LocalProjectId) {
        let (first, second) = (
            LocalProjectId::new("/tmp/nudox-switch-first").expect("identity"),
            LocalProjectId::new("/tmp/nudox-switch-second").expect("identity"),
        );
        let mut snapshot = snapshot();
        for project in [&first, &second] {
            snapshot = reduce(
                &snapshot,
                &Intent::AddProject {
                    project: project.clone(),
                },
            )
            .expect("workspace")
            .snapshot;
        }
        let mut workspace = snapshot.workspace().clone();
        workspace.projects = workspace
            .projects
            .iter()
            .cloned()
            .map(|mut project| {
                project.phase = ProjectPhase::Ready;
                project
            })
            .collect::<Vec<_>>()
            .into();
        (snapshot.with_workspace(workspace), first, second)
    }

    #[test]
    fn switching_between_ready_projects_indexes_nothing_again() {
        let (snapshot, first, second) = two_ready_projects();
        let switched =
            reduce(&snapshot, &Intent::ActivateProject(first.clone())).expect("workspace");
        assert_eq!(
            switched.snapshot.workspace().active.as_ref(),
            Some(&first),
            "the chosen project is the active one"
        );
        assert!(
            switched
                .snapshot
                .workspace()
                .projects
                .iter()
                .all(|project| project.phase == ProjectPhase::Ready),
            "a ready project stays ready: switching is instant, never a fresh compile"
        );
        assert!(
            !switched
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Engine(_) | Effect::Cancel(_))),
            "and no engine work is asked for: {:?}",
            switched.effects
        );
        assert_eq!(
            switched.effects,
            [Effect::Persist],
            "the choice is remembered"
        );
        let back = reduce(&switched.snapshot, &Intent::ActivateProject(second.clone()))
            .expect("workspace");
        assert_eq!(back.snapshot.workspace().active.as_ref(), Some(&second));
        assert_eq!(
            back.snapshot.shelf().selected,
            Some(ResourceIdentity::Local(second)),
            "the shelf follows"
        );
    }

    #[test]
    fn a_stopped_project_stays_stopped_until_someone_presses_try_again() {
        let (snapshot, first, _) = two_ready_projects();
        let failed = set_project_phase(
            &snapshot,
            &first,
            ProjectPhase::Failed,
            Some(Arc::from("the compiler stopped")),
            None,
        );
        let activated =
            reduce(&failed, &Intent::ActivateProject(first.clone())).expect("workspace");
        let row = &activated.snapshot.workspace().projects[0];
        assert_eq!(
            (row.phase, row.error.as_deref()),
            (ProjectPhase::Failed, Some("the compiler stopped")),
            "choosing it does not hide what happened"
        );
        let retried = reduce(&activated.snapshot, &Intent::RetryIndex(first)).expect("workspace");
        let row = &retried.snapshot.workspace().projects[0];
        assert_eq!(
            (row.phase, row.error.as_deref()),
            (ProjectPhase::Indexing, None),
            "Try again starts it again and forgets the old reason"
        );
    }

    #[test]
    fn removing_the_active_project_makes_another_active_and_forgets_the_row() {
        let (snapshot, first, second) = two_ready_projects();
        let active = snapshot
            .workspace()
            .active
            .clone()
            .expect("the last added is active");
        assert_eq!(active, second);
        let removed = reduce(&snapshot, &Intent::RemoveProject(second.clone())).expect("workspace");
        assert_eq!(removed.snapshot.workspace().projects.len(), 1);
        assert_eq!(
            removed.snapshot.workspace().active.as_ref(),
            Some(&first),
            "the shelf never has no active project while it has projects"
        );
        assert_eq!(removed.snapshot.shelf().items.len(), 1);
        assert_eq!(
            removed.snapshot.workspace().projects[0].phase,
            ProjectPhase::Ready,
            "the one that stays is not indexed again"
        );
        assert!(removed.effects.contains(&Effect::Persist));
    }

    #[test]
    fn the_folder_the_picker_chose_closes_the_dialog_it_was_opened_from() {
        let opened = reduce(&snapshot(), &Intent::OpenAddProject)
            .expect("workspace")
            .snapshot;
        assert_eq!(opened.overlay(), Some(Overlay::AddProject));
        let chosen: Arc<[std::path::PathBuf]> = vec!["/tmp/nudox-picker-chosen".into()].into();
        let picked = reduce(
            &opened,
            &Intent::FolderPickerResult {
                outcome: FolderPickerOutcome::Selected(chosen),
            },
        )
        .expect("workspace");
        assert_eq!(picked.snapshot.overlay(), None);
        assert_eq!(picked.snapshot.workspace().projects.len(), 1);
        let cancelled = reduce(
            &opened,
            &Intent::FolderPickerResult {
                outcome: FolderPickerOutcome::Cancelled,
            },
        )
        .expect("workspace");
        assert_eq!(
            cancelled.snapshot.overlay(),
            Some(Overlay::AddProject),
            "a cancelled picker leaves the dialog where it was"
        );
    }

    #[test]
    fn a_library_set_aside_is_told_once_and_every_project_is_indexed_again() {
        let (snapshot, first, _) = two_ready_projects();
        let gone = set_project_phase(&snapshot, &first, ProjectPhase::Missing, None, None);
        let told = reduce(
            &gone,
            &Intent::LibraryRebuilding {
                kept_at: Arc::from("/data/from-another-build"),
            },
        )
        .expect("workspace");
        let phases: Vec<_> = told
            .snapshot
            .workspace()
            .projects
            .iter()
            .map(|project| project.phase)
            .collect();
        assert_eq!(
            phases,
            [ProjectPhase::Missing, ProjectPhase::Indexing],
            "a folder that is gone stays gone; the rest are indexed again"
        );
        assert_eq!(
            told.snapshot.workspace().notes.as_ref(),
            [crate::model::Note::LibraryRebuilding {
                kept_at: Arc::from("/data/from-another-build")
            }]
        );
        let again = reduce(
            &told.snapshot,
            &Intent::LibraryRebuilding {
                kept_at: Arc::from("/data/from-another-build"),
            },
        )
        .expect("workspace");
        assert_eq!(again.snapshot.workspace().notes.len(), 1, "said once");
        let dismissed = reduce(
            &again.snapshot,
            &Intent::DismissNote(crate::model::Note::LibraryRebuilding {
                kept_at: Arc::from("/data/from-another-build"),
            }),
        )
        .expect("workspace");
        assert!(
            dismissed.snapshot.workspace().notes.is_empty(),
            "and let go when dismissed"
        );
    }

    #[test]
    fn the_window_size_is_remembered_only_when_it_changed() {
        let resized = reduce(
            &snapshot(),
            &Intent::WindowResized {
                width: 1100,
                height: 800,
            },
        )
        .expect("workspace");
        assert_eq!(
            resized.snapshot.settings().window,
            Some(crate::model::WindowSize {
                width: 1100,
                height: 800
            })
        );
        assert_eq!(resized.effects, [Effect::Persist]);
        let same = reduce(
            &resized.snapshot,
            &Intent::WindowResized {
                width: 1100,
                height: 800,
            },
        )
        .expect("workspace");
        assert!(same.effects.is_empty(), "the same size writes nothing");
    }

    #[test]
    fn cancellation_keeps_a_project_in_flight_until_the_owner_replies() {
        let project = LocalProjectId::new("/tmp/nudox-cancellable-project").expect("identity");
        let admitted = reduce(
            &snapshot(),
            &Intent::AddProject {
                project: project.clone(),
            },
        )
        .expect("workspace");
        let indexing = reduce(
            &admitted.snapshot,
            &Intent::IndexProject {
                project: project.clone(),
                basis: admitted.snapshot.key(),
                request: RequestId::new(52),
            },
        )
        .expect("index request");
        let cancelled =
            reduce(&indexing.snapshot, &Intent::CancelIndex(project)).expect("cancel request");
        assert!(
            cancelled
                .effects
                .contains(&Effect::Cancel(RequestId::new(52)))
        );
        assert!(cancelled.effects.contains(&Effect::Persist));
        assert_eq!(
            cancelled.snapshot.workspace().projects[0].phase,
            ProjectPhase::Cancelling
        );
    }
}
