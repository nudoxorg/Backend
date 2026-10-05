//! Folder selection and Retry share exact terminal-operation evidence.
use super::*;

#[test]
fn terminal_failure_reselection_clears_only_the_finished_claim() -> Result<(), Box<dyn std::error::Error>> {
    let project = LocalProjectId::new("/fixture/reselect-terminal-project")?;
    let initial = reduce(&AppSnapshot::empty(crate::core::VersionedRoot::unserved()),
        &Intent::AddProject { project: project.clone() }).ok_or("workspace admission")?.snapshot;
    let mut operation = crate::model::index_operation::tests::claim(&project, 0x81);
    operation.observation = Some(crate::model::index_operation::tests::observation(&operation,
        backend_library::IndexOperationState::Failed {
            reason: backend_library::IndexOperationFailureReason::Refused,
            detail: backend_library::ProductText::from_static("compiler refused the exact attempt"),
        }));
    for phase in [ProjectPhase::Failed, ProjectPhase::Cancelled, ProjectPhase::Missing] {
        let mut workspace = initial.workspace().clone();
        let mut rows = workspace.projects.to_vec();
        rows[0].phase = phase;
        rows[0].operation = Some(operation.clone());
        workspace.projects = rows.into();
        let stopped = initial.with_workspace(workspace);
        for intent in [Intent::AddProject { project: project.clone() }, Intent::RetryIndex(project.clone())] {
            let renewed = reduce(&stopped, &intent).ok_or("renewal reducer")?;
            let row = &renewed.snapshot.workspace().projects[0];
            assert_eq!(row.phase, ProjectPhase::Indexing);
            assert_eq!(row.operation, None);
            assert_eq!(row.request, None);
            assert!(renewed.effects.contains(&Effect::Persist));
            let saved = crate::model::PersistentState::project(&renewed.snapshot);
            assert_eq!(saved.shelf[0].phase, crate::model::PersistedProjectPhase::Queued);
            assert_eq!(saved.shelf[0].operation, None);
        }
    }
    Ok(())
}

#[test]
fn missing_folder_reselection_and_retry_cannot_replace_an_uncertain_sent_claim() -> Result<(), Box<dyn std::error::Error>> {
    let project = LocalProjectId::new("/fixture/reselect-uncertain-project")?;
    let initial = reduce(&AppSnapshot::empty(crate::core::VersionedRoot::unserved()),
        &Intent::AddProject { project: project.clone() }).ok_or("workspace admission")?.snapshot;
    let mut operation = crate::model::index_operation::tests::claim(&project, 0x82);
    operation.observation = Some(backend_library::IndexOperationObservation::Unknown { operation_key: operation.key });
    let mut workspace = initial.workspace().clone();
    let mut rows = workspace.projects.to_vec();
    rows[0].phase = ProjectPhase::Missing;
    rows[0].operation = Some(operation.clone());
    workspace.projects = rows.into();
    let missing = initial.with_workspace(workspace);
    let selected = reduce(&missing, &Intent::AddProject { project: project.clone() }).ok_or("reselection")?;
    assert_eq!(selected.snapshot.workspace().projects[0].phase, ProjectPhase::Unconfirmed);
    assert_eq!(selected.snapshot.workspace().projects[0].operation.as_ref(), Some(&operation));
    let retried = reduce(&selected.snapshot, &Intent::RetryIndex(project)).ok_or("retry reducer")?;
    assert_eq!(retried.snapshot.workspace().projects[0].phase, ProjectPhase::Unconfirmed);
    assert_eq!(retried.snapshot.workspace().projects[0].operation.as_ref(), Some(&operation));
    assert!(!retried.effects.iter().any(|effect| matches!(effect, Effect::Engine(_))));
    Ok(())
}
