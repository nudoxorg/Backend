//! One workspace binding shared by startup, the actor, and page workers.
//! Discovery may be retried until installation. Afterwards every restart
//! uses the exact same paths; an outage never selects another workspace.

use crate::core::{ErrorValue, FaultCode, LocalProjectId};
use crate::model::pages::{PageValue, ReadFailure};
use crate::model::{AppSnapshot, PersistentState};
use crate::runtime::owner::OwnerGate;
use crate::runtime::reads::{PageReader, ReadContext, ReadRequest, SessionReader};
use backend_runtime::WorkspacePaths;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Default)]
pub(crate) struct Binding(Arc<OnceLock<BoundWorkspace>>);

pub(crate) struct BoundWorkspace {
    pub(crate) paths: WorkspacePaths,
    pub(crate) project: LocalProjectId,
    pub(crate) snapshot: AppSnapshot,
    pub(crate) persistence: Option<PersistentState>,
}

impl Binding {
    pub(crate) fn get(&self) -> Option<&BoundWorkspace> {
        self.0.get()
    }

    /// Only the host discovery producer installs the binding, before Ready.
    pub(crate) fn install(&self, paths: WorkspacePaths) -> Result<&BoundWorkspace, String> {
        if let Some(bound) = self.get() {
            return if bound.paths == paths {
                Ok(bound)
            } else {
                Err("this window is already bound to a different local workspace".to_owned())
            };
        }
        let project = LocalProjectId::from_path(paths.project()).map_err(|error| {
            format!("the discovered workspace path cannot be represented safely: {error}")
        })?;
        let bound = super::launch::restore_binding(paths, project);
        // A losing installation never replaces any path or authority.
        if let Err(candidate) = self.0.set(bound) {
            if self
                .get()
                .is_none_or(|bound| bound.paths != candidate.paths)
            {
                return Err(
                    "this window is already bound to a different local workspace".to_owned(),
                );
            }
        }
        self.get()
            .ok_or_else(|| "the local workspace binding was not installed".to_owned())
    }
}

impl BoundWorkspace {
    /// Admit cold state once, retaining local decisions made while discovery
    /// was unavailable. The current route/overlay and settings win when changed.
    pub(crate) fn admit_snapshot(
        &self,
        current: &AppSnapshot,
        origin: &AppSnapshot,
    ) -> AppSnapshot {
        let restored = &self.snapshot;
        let mut next = current.clone();
        if current.session() == origin.session() {
            next = next.with_session(restored.session().clone());
        }
        if current.settings() == origin.settings() {
            next = next.with_settings(restored.settings().clone());
        }
        let mut shelf = restored.shelf().clone();
        let mut items = shelf.items.to_vec();
        for local in current.shelf().items.iter() {
            if let Some(old) = items.iter_mut().find(|old| old.identity == local.identity) {
                *old = local.clone();
            } else {
                items.push(local.clone());
            }
        }
        shelf.items = items.into();
        if current.shelf().selected != origin.shelf().selected {
            shelf.selected = current.shelf().selected.clone();
        }
        next = next.with_shelf(shelf);
        let mut workspace = restored.workspace().clone();
        let mut projects = workspace.projects.to_vec();
        for local in current.workspace().projects.iter() {
            if let Some(old) = projects.iter_mut().find(|old| old.id == local.id) {
                *old = local.clone();
            } else {
                projects.push(local.clone());
            }
        }
        workspace.projects = projects.into();
        if current.workspace().active != origin.workspace().active {
            workspace.active = current.workspace().active.clone();
        }
        if current.workspace().path_error != origin.workspace().path_error {
            workspace.path_error = current.workspace().path_error.clone();
        }
        let mut notes = workspace.notes.to_vec();
        for note in current.workspace().notes.iter() {
            if !notes.contains(note) {
                notes.push(note.clone());
            }
        }
        workspace.notes = notes.into();
        workspace.host = Some(self.project.clone());
        next.with_workspace(workspace)
    }
}

/// Page lanes exist even before paths can be discovered. Their sessions bind
/// lazily on the worker, using the very same one-time installation as the actor.
pub(crate) struct BindingReader {
    binding: Binding,
    gate: OwnerGate,
    reader: Option<SessionReader>,
}

impl BindingReader {
    pub(crate) fn new(binding: Binding, gate: OwnerGate) -> Self {
        Self {
            binding,
            gate,
            reader: None,
        }
    }
}

impl PageReader for BindingReader {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        if self.reader.is_none() {
            self.gate.wait_cancelled(context.cancel).map_err(|fault| {
                if context.cancel.is_cancelled() {
                    ReadFailure::Cancelled
                } else {
                    ReadFailure::Fault(ErrorValue::new(FaultCode::Transport, fault.to_string()))
                }
            })?;
            let bound = self.binding.get().ok_or_else(|| {
                ReadFailure::Fault(ErrorValue::new(
                    FaultCode::Transport,
                    "the owner answered without a local workspace binding",
                ))
            })?;
            self.reader = Some(SessionReader::gated(
                bound.paths.endpoint(),
                self.gate.clone(),
            ));
        }
        self.reader
            .as_mut()
            .expect("reader installed after workspace admission")
            .read(request, context)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::pages::{Generation, PageKey};
    use crate::model::{ProjectPhase, ServiceMode};
    use crate::navigation::RequestId;
    use crate::navigation::{Intent, OrbitRoute, Route};
    use crate::runtime::actor::{CancellationToken, EngineActor, EngineDto, EngineRequest};
    use crate::runtime::mailbox::PushResult;
    use crate::runtime::owner::{OwnerFault, OwnerState};
    use crate::runtime::reads::{Priority, ReadJob, ReadPool};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(tag: &str) -> (std::path::PathBuf, WorkspacePaths) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::path::PathBuf::from("/tmp")
            .join(format!("nx-bind-{tag}-{}-{nonce}", std::process::id()));
        let project = root.join("project");
        crate::host::private_dir(&project.join("src")).expect("project");
        crate::host::private_dir(&root.join("data")).expect("data");
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='binding-proof'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("manifest");
        std::fs::write(project.join("src/lib.rs"), "pub struct BindingProof;\n").expect("source");
        let paths = WorkspacePaths::discover(
            Some(project),
            Some(root.join("data")),
            Some(root.join("owner.sock")),
        )
        .expect("paths");
        (root, paths)
    }

    #[test]
    fn discovery_failure_retries_a_real_producer_and_pins_every_later_restart() {
        let (root, paths) = fixture("retry");
        let binding = Binding::default();
        let gate = OwnerGate::starting();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        let discovered = paths.clone();
        let thread =
            crate::host::owner::spawn_discovering(binding.clone(), gate.clone(), move || {
                match counted.fetch_add(1, Ordering::SeqCst) {
                    0 => Err("the application folder could not be admitted".to_owned()),
                    1 => Ok(discovered.clone()),
                    _ => panic!("an installed binding must never discover another workspace"),
                }
            })
            .expect("retry producer");
        crate::runtime::wait::until("discovery failed", || {
            attempts.load(Ordering::SeqCst) == 1
                && matches!(gate.state(), OwnerState::Failed(OwnerFault::Host(_)))
        });
        assert!(binding.get().is_none());
        let actor = EngineActor::start(
            crate::host::launch::BootClient::new(binding.clone(), gate.clone()),
            4,
        )
        .expect("actor exists before installation");
        let readers = binding.clone();
        let sessions = gate.clone();
        let pool = ReadPool::start(1, move |_| {
            BindingReader::new(readers.clone(), sessions.clone())
        })
        .expect("page lane exists before installation");
        assert!(gate.restart(), "Retry has a live producer");
        crate::runtime::wait::until("real owner ready after rediscovery", || {
            matches!(gate.state(), OwnerState::Ready { .. })
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let bound = binding.get().expect("installed before Ready");
        assert_eq!(bound.paths.endpoint(), paths.endpoint());
        let mut session =
            backend_client::Session::connect(bound.paths.endpoint()).expect("actual endpoint");
        session.revision().expect("actual owner revision");
        drop(session);
        assert!(matches!(
            actor.try_submit(EngineRequest::Root {
                request: RequestId::new(1),
                basis: VersionedRoot::unserved(),
                cancel: CancellationToken::new()
            }),
            PushResult::Enqueued
        ));
        let outcome = crate::runtime::wait::until_some(
            "the pre-existing actor binds to the recovered workspace",
            || {
                actor
                    .drain_events()
                    .into_iter()
                    .find(|event| event.request == RequestId::new(1))
            },
        );
        assert!(matches!(outcome.result, Ok(EngineDto::Root { .. })));
        assert!(pool.submit(ReadJob {
            key: PageKey::Health,
            request: ReadRequest::Health,
            generation: Generation::new(1).expect("nonzero fixture generation"),
            priority: Priority::Normal,
            cancel: CancellationToken::new(),
            affinity: None
        }));
        let page = crate::runtime::wait::until_some(
            "the pre-existing page lane binds to the recovered workspace",
            || pool.drain().into_iter().find(|page| page.complete),
        );
        assert!(matches!(page.result, Ok(PageValue::Health(_))));
        gate.publish(OwnerState::Failed(OwnerFault::Host("retry proof".into())));
        assert!(gate.restart());
        crate::runtime::wait::until("same workspace restarted", || {
            matches!(gate.state(), OwnerState::Ready { .. })
        });
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the resolver cannot retarget an admitted window"
        );
        assert_eq!(binding.get().expect("pinned").paths.data(), paths.data());
        drop(actor);
        drop(pool);
        drop(thread);
        std::fs::remove_dir_all(root).expect("fixture removed");
    }

    #[test]
    fn a_late_binding_preserves_local_shelf_settings_and_modal_through_cold_restart() {
        let (root, paths) = fixture("local");
        let origin = AppSnapshot::empty(VersionedRoot::unserved());
        let local = LocalProjectId::from_path(&root.join("project")).expect("folder");
        let current = crate::navigation::reduce(
            &origin,
            Intent::AddProject {
                project: local.clone(),
            },
        )
        .snapshot;
        let current = crate::navigation::reduce(&current, Intent::OpenAddProject).snapshot;
        let mut settings = current.settings().clone();
        settings.window = Some(crate::model::WindowSize {
            width: 777,
            height: 666,
        });
        let current = current.with_settings(settings.clone());
        let binding = Binding::default();
        let bound = binding.install(paths.clone()).expect("late binding");
        let merged = bound.admit_snapshot(&current, &origin);
        assert_eq!(
            merged.session(),
            current.session(),
            "the active Add modal stays over the current route"
        );
        assert_eq!(merged.settings(), &settings);
        assert_eq!(merged.workspace().host, Some(local.clone()));
        let row = merged
            .workspace()
            .projects
            .iter()
            .find(|row| row.id == local)
            .expect("local admission retained");
        assert_eq!(row.phase, ProjectPhase::Indexing);
        assert!(
            row.request.is_none(),
            "cold state cannot claim the owner submitted it"
        );
        let persistence = bound.persistence.as_ref().expect("saving available");
        persistence
            .save(&PersistentState::project(&merged))
            .expect("save local session");
        let cold = Binding::default();
        let restarted = cold.install(paths).expect("cold restart");
        assert_eq!(restarted.snapshot.settings().window, settings.window);
        let row = restarted
            .snapshot
            .workspace()
            .projects
            .iter()
            .find(|row| row.id == local)
            .expect("durable local admission");
        assert_eq!(row.phase, ProjectPhase::Indexing);
        assert!(row.request.is_none());
        std::fs::remove_dir_all(root).expect("fixture removed");
    }

    #[test]
    fn retry_capability_is_shared_and_cannot_create_a_phantom_start() {
        let gate = OwnerGate::starting();
        let clone = gate.clone();
        gate.disable_restart();
        gate.publish(OwnerState::Failed(OwnerFault::Host(
            "thread creation refused".into(),
        )));
        assert!(!clone.restart());
        assert!(matches!(gate.state(), OwnerState::Failed(_)));
        let binding = Binding::default();
        assert!(binding.get().is_none());
        // A serving gate alone can never manufacture a workspace binding.
        let ready = OwnerGate::ready(VersionedRoot::unserved(), ServiceMode::Embedded);
        assert!(ready.wait().is_ok());
        assert!(binding.get().is_none());
        assert_eq!(
            AppSnapshot::empty(VersionedRoot::unserved()).route(),
            &Route::Orbit(OrbitRoute::Home)
        );
    }
    #[test]
    fn an_installed_binding_explicitly_rejects_distinct_workspace_paths() {
        let (root, paths) = fixture("pinned");
        let binding = Binding::default();
        binding.install(paths.clone()).expect("first installation");
        binding
            .install(paths.clone())
            .expect("same binding remains admissible");
        let distinct = WorkspacePaths::discover(
            Some(paths.project().to_path_buf()),
            Some(paths.data().to_path_buf()),
            Some(root.join("another.sock")),
        )
        .expect("distinct endpoint");
        assert!(
            binding.install(distinct).is_err(),
            "a caller must see that its paths were rejected"
        );
        assert_eq!(binding.get().expect("unchanged binding").paths, paths);
        std::fs::remove_dir_all(root).expect("fixture removed");
    }

    #[test]
    fn a_local_preflight_refusal_cannot_overwrite_a_submitted_or_newer_root_request() {
        struct NoIo;
        impl crate::runtime::EngineClient for NoIo {
            fn execute(
                &mut self,
                _: &EngineRequest,
            ) -> Result<EngineDto, crate::runtime::EngineFault> {
                Err(crate::runtime::EngineFault::Cancelled)
            }
        }
        let key = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("preflight".into(), "one".into())]),
            1,
        );
        let project = LocalProjectId::new("/tmp/preflight-project").expect("identity");
        let unsent = crate::navigation::reduce(
            &AppSnapshot::empty(key),
            Intent::AddProject {
                project: project.clone(),
            },
        )
        .snapshot;
        let submitted = crate::navigation::reduce(
            &unsent,
            Intent::IndexProject {
                operation: crate::model::index_operation::tests::claim(&project, 0x51),
                project: project.clone(),
                basis: key,
                request: RequestId::new(7),
            },
        )
        .snapshot;
        let actor = EngineActor::start(NoIo, 4).expect("actor");
        let mut runtime = crate::runtime::DesktopRuntime::new(submitted.clone(), actor);
        assert!(
            runtime
                .reject_unsent_index(&project, key, "late save refusal".into())
                .is_empty()
        );
        assert_eq!(*runtime.snapshot(), submitted);
        let actor = EngineActor::start(NoIo, 4).expect("actor");
        let mut runtime = crate::runtime::DesktopRuntime::new(unsent.clone(), actor);
        let stale = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("preflight".into(), "old".into())]),
            1,
        );
        assert!(
            runtime
                .reject_unsent_index(&project, stale, "obsolete refusal".into())
                .is_empty()
        );
        assert_eq!(*runtime.snapshot(), unsent);
        assert!(
            !runtime
                .reject_unsent_index(&project, key, "current save refusal".into())
                .is_empty()
        );
        assert_eq!(
            runtime.snapshot().workspace().projects[0].phase,
            ProjectPhase::Failed
        );
    }
}
