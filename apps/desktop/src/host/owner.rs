//! The owner's own thread (W-Open I1): the index owner starts — or this
//! window attaches to a live one — beside the window, never before it.
//!
//! What used to run on the main thread before `gpui::Application` existed
//! (the lease, composing an embedded owner, twelve retries over twenty
//! seconds, and a full hydration of the view only to learn its root) runs
//! here. It ends in one [`OwnerState`] on the gate: `Ready` with the owner's
//! root from one `revision()` round trip, or `Failed` in the host's own words.
//! Startup failures wait for "Try again" and start again. Publication Retry
//! keeps the exact live host and reacquires only its read-side subscription;
//! accepted index work retains its operation identity and uncertain outcome.

use super::lease::{DesktopHost, HostError, HostMode};
use crate::core::VersionedRoot;
use crate::model::ServiceMode;
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerHostLifetime, OwnerState};
use backend_client::Session;
use backend_runtime::WorkspacePaths;
use std::panic::AssertUnwindSafe;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Attempts before an owner that never answers is reported.
const ATTEMPTS: usize = 12;
/// Pause between attempts.
const RETRY: Duration = Duration::from_millis(120);
/// Wall-clock bound over all attempts.
const DEADLINE: Duration = Duration::from_secs(20);

/// The running owner thread; dropping it lets the host go and joins.
pub(crate) struct OwnerThread {
    gate: OwnerGate,
    join: Option<JoinHandle<()>>,
}

impl Drop for OwnerThread {
    fn drop(&mut self) {
        crate::runtime::trace::mark("close.owner_join", "closing gate before host join");
        self.gate.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// What a started owner hands the window: the host to keep alive until the
/// app quits, the root it answered at, and how this window reached it.
struct Started<H> {
    host: H,
    key: VersionedRoot,
    mode: ServiceMode,
}

/// Starts the owner for `paths` on its own thread. Returns at once.
pub(crate) fn spawn(paths: WorkspacePaths, gate: OwnerGate) -> Option<OwnerThread> {
    spawn_with_observer(
        gate,
        move |gate| {
            start(&paths, gate).map(|(host, key)| {
                let mode = match host.mode() {
                    HostMode::Embedded => ServiceMode::Embedded,
                    HostMode::Attached => ServiceMode::Attached,
                };
                Started { host, key, mode }
            })
        },
        |gate, host: &DesktopHost, lifetime| super::observation::serve(gate, host.endpoint(), lifetime),
    )
}

/// Discovery has a real retry producer even when no initial workspace was
/// available. Once installed, the binding is never rediscovered or replaced.
pub(crate) fn spawn_discovering(
    binding: super::bootstrap::Binding,
    gate: OwnerGate,
    mut discover: impl FnMut() -> Result<WorkspacePaths, String> + Send + 'static,
) -> Option<OwnerThread> {
    let mut first_fault = match gate.state() {
        OwnerState::Failed(fault) => Some(fault.to_string()),
        _ => None,
    };
    spawn_with_observer(gate, move |gate| {
        if let Some(fault) = first_fault.take() { return Err(fault); }
        let bound = match binding.get() {
            Some(bound) => bound,
            None => binding.install(discover()?)?,
        };
        start(&bound.paths, gate).map(|(host, key)| {
            let mode = match host.mode() {
                HostMode::Embedded => ServiceMode::Embedded,
                HostMode::Attached => ServiceMode::Attached,
            };
            Started { host, key, mode }
        })
    }, |gate, host: &DesktopHost, lifetime| super::observation::serve(gate, host.endpoint(), lifetime))
}

/// [`spawn`] over any way of starting an owner (a test's panics).
#[cfg(test)]
fn spawn_with<H: 'static>(
    gate: OwnerGate,
    starter: impl FnMut(&OwnerGate) -> Result<Started<H>, String> + Send + 'static,
) -> Option<OwnerThread> {
    spawn_with_observer(gate, starter, |gate, _, _| gate.await_close_or_restart())
}

fn spawn_with_observer<H: 'static>(
    gate: OwnerGate,
    starter: impl FnMut(&OwnerGate) -> Result<Started<H>, String> + Send + 'static,
    observer: impl FnMut(&OwnerGate, &H, OwnerHostLifetime) -> bool + Send + 'static,
) -> Option<OwnerThread> {
    let owner = gate.clone();
    match std::thread::Builder::new()
        .name("nudox-owner".to_owned())
        .spawn(move || run(&owner, starter, observer))
    {
        Ok(join) => Some(OwnerThread {
            gate,
            join: Some(join),
        }),
        Err(error) => {
            gate.disable_restart();
            gate.publish(OwnerState::Failed(OwnerFault::Host(
                format!("the owner's thread could not start: {error}").into(),
            )));
            None
        }
    }
}

/// The owner thread's life: start, publish, wait for the app to quit; or
/// publish why not and wait for "Try again". A start that panics is that
/// fault ([`OwnerFault::Panicked`]), never a thread that vanished with every
/// worker waiting on a gate nobody will open.
fn run<H>(
    gate: &OwnerGate,
    mut starter: impl FnMut(&OwnerGate) -> Result<Started<H>, String>,
    mut observer: impl FnMut(&OwnerGate, &H, OwnerHostLifetime) -> bool,
) {
    loop {
        let fault = match std::panic::catch_unwind(AssertUnwindSafe(|| starter(gate))) {
            Ok(Ok(Started { host, key, mode })) => {
                let Some(host_lifetime) = gate.publish_host(key, mode) else { return; };
                crate::runtime::trace::mark("boot.owner_ready", "published");
                let restart = loop {
                    let restart = match std::panic::catch_unwind(AssertUnwindSafe(|| observer(gate, &host, host_lifetime))) {
                        Ok(restart) => restart,
                        Err(panic) => {
                            gate.publish(OwnerState::Failed(OwnerFault::Panicked(
                                crate::runtime::offload::describe(panic.as_ref()),
                            )));
                            gate.await_restart()
                        }
                    };
                    if !restart || !gate.publication_retry_for(host_lifetime) {
                        break restart;
                    }
                    // Publication Retry replaces only the read-side socket.
                    // Keep the exact host and its durable index operation;
                    // the observer must freshly prove its retained cursor.
                };
                drop(host);
                if restart {
                    continue;
                }
                return;
            }
            Ok(Err(message)) => OwnerFault::Host(message.into()),
            Err(panic) => OwnerFault::Panicked(crate::runtime::offload::describe(panic.as_ref())),
        };
        gate.publish(OwnerState::Failed(fault));
        // `restart` publishes `Starting` itself.
        if !gate.await_restart() {
            return;
        }
    }
}

/// Tries until the owner answers or the deadline passes. The first failure
/// is published at once, so the window says what is wrong within one
/// attempt, not after twenty seconds; the later attempts continue behind
/// it, and one that succeeds replaces the failure with the owner's root.
fn start(paths: &WorkspacePaths, gate: &OwnerGate) -> Result<(DesktopHost, VersionedRoot), String> {
    let started = Instant::now();
    let mut last = "the local service did not become ready".to_owned();
    for attempt in 0..ATTEMPTS {
        let attempting = Instant::now();
        crate::runtime::trace::mark(
            "boot.owner_attempt.begin",
            format_args!("attempt {}", attempt + 1),
        );
        match attempt_once(paths, gate) {
            Ok(opened) => {
                crate::runtime::trace::span(
                    "boot.owner_attempt",
                    attempting,
                    format_args!("attempt {} succeeded", attempt + 1),
                );
                return Ok(opened);
            }
            Err(message) => {
                crate::runtime::trace::span(
                    "boot.owner_attempt",
                    attempting,
                    format_args!("attempt {} failed", attempt + 1),
                );
                if attempt == 0 {
                    gate.publish(OwnerState::Failed(OwnerFault::from(message.as_str())));
                }
                last = message;
            }
        }
        if started.elapsed() >= DEADLINE {
            return Err(format!(
                "{last} (gave up after {}s)",
                started.elapsed().as_secs()
            ));
        }
        if attempt.saturating_add(1) < ATTEMPTS {
            std::thread::sleep(RETRY);
        }
    }
    Err(last)
}

fn attempt_once(paths: &WorkspacePaths, gate: &OwnerGate) -> Result<(DesktopHost, VersionedRoot), String> {
    let starting = Instant::now();
    crate::runtime::trace::mark("boot.owner_host_start.begin", "starting selected host");
    let mut host = DesktopHost::start_with_paths(paths.clone()).map_err(|error| describe(&error))?;
    crate::runtime::trace::span(
        "boot.owner_start",
        starting,
        format_args!("{:?}", host.mode()),
    );
    if let Some(moved) = host.state_set_aside() {
        super::aside::record(moved);
    }
    // One round trip for the root, where a full hydration of the view used
    // to be (406-581 ms on the fixture index, LEDGER §2.1-2).
    let asking = Instant::now();
    crate::runtime::trace::mark("boot.revision.begin", "requesting owner revision");
    let revision = Session::connect(host.endpoint())
        .and_then(|mut session| session.revision())
        .map_err(|error| format!("read the owner's revision: {error}"))?;
    crate::runtime::trace::span("boot.revision", asking, "Session::revision");
    host.publish_registry(gate);
    Ok((host, VersionedRoot::from_revision(1, revision.cursor(), 0)))
}

fn describe(error: &HostError) -> String {
    format!("reach the local service at {}: {error}", error.operand())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn settle(gate: &OwnerGate, done: impl Fn(&OwnerState) -> bool) -> OwnerState {
        let mut state = gate.state();
        crate::runtime::wait::until(
            "the owner thread published the state the test waits for",
            || {
                state = gate.state();
                done(&state)
            },
        );
        state
    }

    #[test]
    fn an_owner_that_panics_is_a_typed_fault_on_the_gate_and_a_retry_can_start_it() {
        let gate = OwnerGate::starting();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&attempts);
        let thread = spawn_with(gate.clone(), move |_| {
            assert!(
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0,
                "the host tripped over its own lease"
            );
            Ok(Started {
                host: (),
                key: VersionedRoot::unserved(),
                mode: ServiceMode::Embedded,
            })
        })
        .expect("the owner thread");
        let failed = settle(&gate, |state| matches!(state, OwnerState::Failed(_)));
        let OwnerState::Failed(OwnerFault::Panicked(what)) = failed else {
            panic!("not a panic fault: {failed:?}")
        };
        assert!(
            what.contains("the host tripped over its own lease"),
            "the fault says what panicked: {what}"
        );
        assert!(
            gate.wait().is_err(),
            "a worker waiting on the gate is released with the fault, not left waiting"
        );
        // "Try again" starts it again, and this time it answers.
        assert!(gate.restart());
        settle(&gate, |state| matches!(state, OwnerState::Ready { .. }));
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2,
            "genuine startup failure retries the starter");
        drop(thread);
    }

    #[test]
    fn publication_retry_keeps_the_live_host_and_its_exact_operation_instead_of_restarting() {
        use crate::runtime::owner::{ObservationFailure, PublicationAdmission};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}, mpsc};
        struct Host {
            operation: [u8; 32],
            dropped: Arc<AtomicUsize>,
        }
        impl Drop for Host {
            fn drop(&mut self) { self.dropped.fetch_add(1, Ordering::SeqCst); }
        }
        let view = crate::runtime::owner::publication_tests::view();
        let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
        let key = VersionedRoot::from_revision(1, cursor, 0);
        let gate = OwnerGate::starting();
        let starts = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&starts);
        let dropped = Arc::clone(&drops);
        let (confirmed, confirmation) = mpsc::channel();
        let mut original = None;
        let owner = spawn_with_observer(gate.clone(), move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Started { host: Host { operation: [71; 32], dropped: Arc::clone(&dropped) }, key, mode: ServiceMode::Embedded })
        }, move |gate, host, lifetime| {
            assert!(gate.observation_attachment_for(lifetime).is_some());
            let (attachment, suspended) = gate.observation_attachment().expect("live worker attachment");
            let host_identity = host as *const Host as usize;
            if let Some(identity) = original {
                assert_eq!(identity, host_identity, "Retry reused the exact live host");
                assert!(suspended);
                assert_eq!(host.operation, [71; 32], "the accepted operation was retained");
                assert_eq!(gate.publish_view(attachment, Arc::clone(&view), cursor), PublicationAdmission::Admitted);
                assert!(gate.complete_observation(attachment));
                confirmed.send(()).expect("fresh read-side proof");
            } else {
                original = Some(host_identity);
                assert_eq!(gate.publish_view(attachment, Arc::clone(&view), cursor), PublicationAdmission::Admitted);
                assert!(gate.observation_failed(attachment, ObservationFailure::RecoveryExpired(Duration::from_secs(30))));
            }
            gate.await_close_or_restart()
        }).expect("worker");
        settle(&gate, |state| matches!(state, OwnerState::Failed(OwnerFault::Observation(_))));
        assert!(gate.restart());
        confirmation.recv_timeout(Duration::from_secs(2)).expect("same-host publication recovery");
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "publication retry cannot stop the accepted operation's host");
        drop(owner);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_lost_attachment_restarts_only_its_own_serving_generation() {
        let gate = OwnerGate::starting();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&attempts);
        let thread = spawn_with(gate.clone(), move |_| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Started {
                host: (),
                key: VersionedRoot::unserved(),
                mode: ServiceMode::Attached,
            })
        })
        .expect("attached owner thread");
        settle(&gate, |state| matches!(state, OwnerState::Ready { .. }));
        let old = gate.attached_ready_epoch().expect("attached generation");
        assert!(gate.attached_lost_at(old, "socket closed".into()));
        assert!(matches!(
            gate.state(),
            OwnerState::Failed(OwnerFault::Lost(_))
        ));
        assert!(gate.restart(), "Retry can reattach after confirmed loss");
        settle(&gate, |state| matches!(state, OwnerState::Ready { .. }));
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(!gate.attached_lost_at(old, "stale failure".into()));
        assert!(matches!(gate.state(), OwnerState::Ready { .. }));
        drop(thread);
    }

    #[test]
    fn a_real_attached_owner_restarts_with_page_and_actor_reads_waiting() {
        use crate::model::pages::{Generation, PageKey, PageValue};
        use crate::navigation::RequestId;
        use crate::runtime::actor::{CancellationToken, EngineActor, EngineDto, EngineRequest};
        use crate::runtime::client::LocalEngineClient;
        use crate::runtime::mailbox::PushResult;
        use crate::runtime::reads::{
            Delivery, Priority, ReadJob, ReadPool, ReadRequest, SessionReader,
        };
        use std::sync::mpsc;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = crate::host::scratch_base().join(format!(
            "nudox-owner-restart-{}-{nonce}",
            std::process::id()
        ));
        let project = root.join("project");
        crate::host::private_dir(&project.join("src")).expect("private project");
        std::fs::write(
            project.join("Cargo.toml"),
            b"[package]\nname='owner-retry-proof'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("manifest");
        std::fs::write(project.join("src/lib.rs"), b"pub struct RetryProof;\n").expect("source");
        let endpoint = root.join("owner.sock");
        let paths = WorkspacePaths::discover(
            Some(project.clone()),
            Some(root.join("data")),
            Some(endpoint.clone()),
        )
        .expect("paths");
        let external =
            DesktopHost::start_with_paths(paths.clone()).expect("external embedded owner");
        assert_eq!(external.mode(), HostMode::Embedded);

        let gate = OwnerGate::starting();
        let (retry_tx, retry_rx) = mpsc::channel::<()>();
        let mut attempts = 0;
        let thread = spawn_with(gate.clone(), move |gate| {
            if attempts > 0 {
                retry_rx.recv().expect("release retry");
            }
            attempts += 1;
            start(&paths, gate).map(|(host, key)| {
                let mode = match host.mode() {
                    HostMode::Embedded => ServiceMode::Embedded,
                    HostMode::Attached => ServiceMode::Attached,
                };
                Started { host, key, mode }
            })
        })
        .expect("GUI owner thread");
        settle(&gate, |state| {
            matches!(
                state,
                OwnerState::Ready {
                    mode: ServiceMode::Attached,
                    ..
                }
            )
        });

        let page_gate = gate.clone();
        let page_endpoint = endpoint.clone();
        let pool = ReadPool::start(1, move |_| {
            SessionReader::gated(&page_endpoint, page_gate.clone())
        })
        .expect("page lane");
        let project_id =
            crate::core::LocalProjectId::from_path(&project).expect("project identity");
        let actor = EngineActor::start(
            LocalEngineClient::gated(&endpoint, project_id, gate.clone()),
            8,
        )
        .expect("actor lane");
        let submit_page = |generation| {
            assert!(
                pool.submit(ReadJob {
                    key: PageKey::Health,
                    request: ReadRequest::Health,
                    generation: Generation::new(generation).expect("nonzero fixture generation"),
                    priority: Priority::Normal,
                    cancel: CancellationToken::new(),
                    affinity: None,
                })
                .is_ok()
            );
        };
        let submit_root = |request| {
            assert!(matches!(
                actor.try_submit(EngineRequest::Root {
                    project: None,
                    request: RequestId::new(request),
                    basis: VersionedRoot::unserved(),
                    cancel: CancellationToken::new(),
                }),
                PushResult::Enqueued
            ));
        };
        let page = |generation| {
            crate::runtime::wait::until_some("page read completed", || {
                pool.take(std::num::NonZeroUsize::MAX)
                    .outcomes
                    .into_iter()
                    .find(|outcome| {
                        outcome.generation
                            == Generation::new(generation).expect("nonzero fixture generation")
                            && outcome.is_terminal()
                    })
            })
        };
        let root_reply = |request| {
            crate::runtime::wait::until_some("actor request completed", || {
                actor
                    .drain_events()
                    .into_iter()
                    .find(|event| event.request == RequestId::new(request))
            })
        };

        submit_page(1);
        submit_root(1);
        assert!(
            matches!(
                page(1).delivery,
                Delivery::Terminal(Ok(PageValue::Health(_)))
            ),
            "the attached owner answered the page lane"
        );
        let initial_root = root_reply(1);
        assert!(
            matches!(&initial_root.result, Ok(EngineDto::Root { .. })),
            "the attached owner must answer the actor lane: {:?}",
            initial_root.result
        );

        drop(external);
        submit_page(2);
        submit_root(2);
        settle(&gate, |state| {
            matches!(state, OwnerState::Failed(OwnerFault::Lost(_)))
        });
        assert!(
            matches!(page(2).delivery, Delivery::Terminal(Err(_))),
            "the dead owner did not answer the page lane"
        );
        assert!(
            root_reply(2).result.is_err(),
            "the dead owner did not answer the actor lane"
        );

        assert!(gate.restart(), "the visible Retry requests a new owner");
        submit_page(3);
        submit_root(3);
        crate::runtime::wait::until("page read is waiting for restart", || pool.running() == 1);
        assert!(
            actor.drain_events().is_empty(),
            "the actor has no result while the owner is starting"
        );
        retry_tx.send(()).expect("start replacement owner");
        settle(&gate, |state| {
            matches!(
                state,
                OwnerState::Ready {
                    mode: ServiceMode::Embedded,
                    ..
                }
            )
        });
        assert!(
            matches!(
                page(3).delivery,
                Delivery::Terminal(Ok(PageValue::Health(_)))
            ),
            "the waiting page uses the replacement owner"
        );
        assert!(
            matches!(root_reply(3).result, Ok(EngineDto::Root { .. })),
            "the waiting actor uses the replacement owner"
        );

        drop(actor);
        drop(pool);
        drop(thread);
        std::fs::remove_dir_all(&root).expect("remove scratch owner");
    }
}
