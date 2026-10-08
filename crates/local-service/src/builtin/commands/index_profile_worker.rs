//! The existing compiler worker also admits its private semantic candidate.
//!
//! The actor retains immutable read proofs. A startup channel transfers the
//! unique writers only after thread creation succeeds; the owned JoinHandle
//! returns them even when compilation or admission unwinds.

use super::super::super::semantic_authority::DetachedSemanticAuthority;
use super::super::super::{BuiltinModel, BuiltinModelError};
use super::super::index::{
    DeferredIndex, DeferredProfileFailure, DeferredProfileTicket, deferred_compile_was_cancelled,
    finish_deferred_profile_for_snapshot, run_deferred_compile,
};
use backend_engine::application::{LocalCompilerClient, OwnedPackageSourceSet};
use backend_engine::{ReadHeadWriter, WorkspaceSnapshot};
use backend_semantic::vocabulary::LanguageProfile;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

pub(super) struct ProfileIdentity {
    pub(super) attempt: backend_extension_turso::CandidateAttempt,
    pub(super) profile: LanguageProfile,
    pub(super) ordinal: u16,
    pub(super) total: u16,
}

pub(super) enum ProfileOutcome {
    Cancelled,
    Admitted(Result<(), DeferredProfileFailure>),
}

#[must_use = "return both unique writers after joining the actual compiler worker"]
pub(super) struct ProfileWork {
    pub(super) semantic: Option<DetachedSemanticAuthority>,
    pub(super) read_head: Option<ReadHeadWriter<BuiltinModel>>,
    pub(super) job: DeferredIndex,
    pub(super) identity: ProfileIdentity,
    snapshot: WorkspaceSnapshot,
    compiler: LocalCompilerClient,
    profile: Option<DeferredProfileTicket>,
    sources: Option<OwnedPackageSourceSet>,
    cancelled: Arc<AtomicBool>,
}

impl ProfileWork {
    pub(super) fn new(
        semantic: Option<DetachedSemanticAuthority>,
        read_head: ReadHeadWriter<BuiltinModel>,
        job: DeferredIndex,
        snapshot: WorkspaceSnapshot,
        compiler: LocalCompilerClient,
        profile: DeferredProfileTicket,
        sources: OwnedPackageSourceSet,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        let identity = ProfileIdentity {
            attempt: profile.candidate_attempt().clone(),
            profile: profile.profile(),
            ordinal: profile.ordinal(),
            total: profile.total(),
        };
        Self {
            semantic,
            read_head: Some(read_head),
            job,
            identity,
            snapshot,
            compiler,
            profile: Some(profile),
            sources: Some(sources),
            cancelled,
        }
    }

    #[cfg(test)]
    pub(super) fn cancellation_for_test(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    fn run_inner(&mut self) -> ProfileOutcome {
        let compiled = run_deferred_compile(
            &self.compiler,
            self.sources
                .take()
                .expect("single-use compiler source frontier"),
            Arc::clone(&self.cancelled),
        );
        if self.cancelled.load(Ordering::Acquire) || deferred_compile_was_cancelled(&compiled) {
            return ProfileOutcome::Cancelled;
        }
        ProfileOutcome::Admitted(finish_deferred_profile_for_snapshot(
            &self.snapshot,
            self.semantic
                .as_mut()
                .expect("owned semantic writer")
                .authority_mut(),
            &mut self.job,
            self.profile.take().expect("single-use compiler profile"),
            compiled,
        ))
    }

    fn run(self) -> ProfileCompletion {
        self.run_with(Self::run_inner)
    }

    pub(super) fn run_with(
        mut self,
        operation: impl FnOnce(&mut Self) -> ProfileOutcome,
    ) -> ProfileCompletion {
        // Borrow the work across the unwind boundary. The Turso connection,
        // OS writer lease and durable view sink are never consumed by the
        // fallible compiler/admission call and therefore survive its panic.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut self)))
            .unwrap_or_else(|_| ProfileOutcome::Admitted(Err(BuiltinModelError(
                "compiler or semantic candidate admission panicked; the original writers were retained".to_owned(),
            ).into())));
        ProfileCompletion {
            work: self,
            outcome,
        }
    }
}

#[must_use = "settle the returned writers before exposing a terminal receipt"]
pub(super) struct ProfileCompletion {
    pub(super) work: ProfileWork,
    pub(super) outcome: ProfileOutcome,
}

#[must_use = "poll and join the owned compiler worker"]
pub(super) enum ProfileWorker {
    Running(JoinHandle<ProfileCompletion>),
    Returned(ProfileCompletion),
}

impl ProfileWorker {
    pub(super) fn spawn(work: ProfileWork) -> Result<Self, (ProfileWork, std::io::Error)> {
        Self::spawn_with(work, |receive| {
            std::thread::Builder::new()
                .name("locald-index-compile".to_owned())
                .spawn(move || {
                    receive
                        .recv()
                        .expect("owned compiler startup transfer")
                        .run()
                })
        })
    }

    pub(super) fn spawn_with(
        work: ProfileWork,
        spawn: impl FnOnce(
            std::sync::mpsc::Receiver<ProfileWork>,
        ) -> std::io::Result<JoinHandle<ProfileCompletion>>,
    ) -> Result<Self, (ProfileWork, std::io::Error)> {
        // No authority enters the closure until spawn has succeeded. Failed
        // channel transfer returns the exact work rather than dropping it.
        let (send, receive) = std::sync::mpsc::sync_channel::<ProfileWork>(1);
        let handle = match spawn(receive) {
            Ok(handle) => handle,
            Err(error) => return Err((work, error)),
        };
        if let Err(error) = send.send(work) {
            let _ = handle.join();
            return Err((
                error.0,
                std::io::Error::other("compiler worker refused its startup transfer"),
            ));
        }
        Ok(Self::Running(handle))
    }

    pub(super) fn join(self) -> ProfileCompletion {
        match self {
            Self::Running(handle) => handle
                .join()
                .expect("the compiler worker catches unwinds while borrowing its unique writers"),
            Self::Returned(completion) => completion,
        }
    }

    pub(super) fn poll(self) -> Result<ProfileCompletion, Self> {
        match self {
            Self::Running(handle) if !handle.is_finished() => Err(Self::Running(handle)),
            running @ Self::Running(_) => Ok(running.join()),
            Self::Returned(completion) => Ok(completion),
        }
    }
}

/// Every accepted attempt receives cleanup even when an earlier durable
/// retirement fails. Keep one bounded cause and a count, rather than allowing
/// `?` to abandon the remaining queue or accumulating unbounded error text.
pub(super) fn retire_failed_attempts(
    attempts: impl IntoIterator<Item = backend_extension_turso::CandidateAttempt>,
    mut retire: impl FnMut(&backend_extension_turso::CandidateAttempt) -> Result<(), BuiltinModelError>,
) -> Result<(), BuiltinModelError> {
    let mut first_failure = None;
    let mut failures = 0usize;
    for attempt in attempts {
        if let Err(error) = retire(&attempt) {
            failures = failures.saturating_add(1);
            first_failure.get_or_insert(error);
        }
    }
    match first_failure {
        Some(error) => Err(BuiltinModelError(format!(
            "retire index attempts failed for {failures} attempts: {error}"
        ))),
        None => Ok(()),
    }
}
