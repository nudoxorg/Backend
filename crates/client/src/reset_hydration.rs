//! One proof, page-count, continuation, and deadline path for reset readers.

use crate::ClientError;
use crate::monotonic::MonotonicClock;
use crate::reset_budget::{ResetBudget, ResetFault};
use crate::subscription::snapshot_page_from_bytes_with_verifier;
use backend_library::{
    Cursor, CursorRead, ProducerObservationVerifier, SnapshotHydrator, ViewRoot,
};
use std::time::Instant;

/// A failure while admitting one producer-certified reset page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResetHydrationError {
    /// The shared row, page, or absolute-time contract refused the page.
    Budget(ResetFault),
    /// The wire proof, continuation, or hydrated root was invalid.
    Invalid(ClientError),
}

impl From<ClientError> for ResetHydrationError {
    fn from(error: ClientError) -> Self {
        Self::Invalid(error)
    }
}

impl ResetHydrationError {
    pub(crate) fn into_client_error(self) -> ClientError {
        match self {
            Self::Budget(fault) => fault.into(),
            Self::Invalid(error) => error,
        }
    }
}

/// The authenticated descriptor's shared reset budget, for observation ticks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResetBinding {
    pub(crate) started: Instant,
    pub(crate) rows: u64,
    pub(crate) pages: u16,
    pub(crate) deadline: Instant,
}

/// Progress after admitting exactly one response page.
pub(crate) enum ResetPageProgress {
    /// The admitted page names this exact continuation next.
    Continue {
        continuation: Box<[u8]>,
        newly_bound: Option<ResetBinding>,
    },
    /// Every authenticated row was admitted and the complete root rebuilt.
    Complete {
        cursor: Cursor,
        root: Box<ViewRoot>,
        newly_bound: Option<ResetBinding>,
    },
}

/// State shared by the durable publication observer and one-shot bootstrap.
pub(crate) struct ResetHydration {
    previous: Cursor,
    budget: ResetBudget,
    expected_page: Box<[u8]>,
    hydrator: Option<SnapshotHydrator>,
    #[cfg(test)]
    after_proof: Option<Box<dyn FnMut()>>,
}

impl ResetHydration {
    pub(crate) fn begin(previous: Cursor, now: std::time::Instant) -> Result<Self, ResetFault> {
        Ok(Self {
            previous,
            budget: ResetBudget::begin(now)?,
            expected_page: Box::new([]),
            hydrator: None,
            #[cfg(test)]
            after_proof: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_after_proof_for_test(&mut self, hook: Box<dyn FnMut()>) {
        self.after_proof = Some(hook);
    }

    pub(crate) fn budget(&self) -> &ResetBudget {
        &self.budget
    }

    pub(crate) fn check(&self, clock: &dyn MonotonicClock) -> Result<(), ResetFault> {
        self.budget.check(clock.now())
    }

    /// Verifies, charges, and hydrates one exact page under the same clock.
    ///
    /// The provisional deadline is checked before parsing and after proof
    /// admission. The admitted descriptor may fix the total allowance only
    /// after that second check; row accumulation and root reconstruction are
    /// checked again before another page can be requested or a root returned.
    pub(crate) fn admit_page<V: ProducerObservationVerifier>(
        &mut self,
        page: &[u8],
        next: Option<&[u8]>,
        payload: &[u8],
        peer: &V,
        clock: &dyn MonotonicClock,
    ) -> Result<ResetPageProgress, ResetHydrationError> {
        self.check(clock).map_err(ResetHydrationError::Budget)?;
        if page != self.expected_page.as_ref() {
            return Err(ResetHydrationError::Invalid(ClientError::Protocol(
                "publication reset page mismatch".to_owned(),
            )));
        }

        let claim = snapshot_page_from_bytes_with_verifier(payload, self.previous, None, peer)?;
        #[cfg(test)]
        if let Some(mut hook) = self.after_proof.take() {
            hook();
        }
        self.check(clock).map_err(ResetHydrationError::Budget)?;

        let was_unbound = self.budget.descriptor().is_none();
        self.budget
            .admit_page(claim.descriptor().row_count(), clock.now())
            .map_err(ResetHydrationError::Budget)?;
        let newly_bound = if was_unbound {
            self.budget.descriptor().map(|(rows, pages)| ResetBinding {
                started: self.budget.started(),
                rows,
                pages,
                deadline: self.budget.deadline(),
            })
        } else {
            None
        };

        let admitted_next = claim
            .next_token()
            .map_err(|error| ResetHydrationError::Invalid(ClientError::Protocol(error)))?;
        self.check(clock).map_err(ResetHydrationError::Budget)?;
        if admitted_next.as_deref() == Some(page) {
            return Err(ResetHydrationError::Budget(
                ResetFault::RepeatedContinuation,
            ));
        }
        if admitted_next.as_deref() != next {
            return Err(ResetHydrationError::Invalid(ClientError::Protocol(
                "publication reset continuation is not authenticated".to_owned(),
            )));
        }

        let current = match self.hydrator.take() {
            Some(mut current) => {
                current
                    .push_page(claim)
                    .map_err(|error| ResetHydrationError::Invalid(ClientError::Protocol(error)))?;
                current
            }
            None => SnapshotHydrator::start(self.previous, claim)
                .map_err(|error| ResetHydrationError::Invalid(ClientError::Protocol(error)))?,
        };
        self.check(clock).map_err(ResetHydrationError::Budget)?;

        if current.is_complete() {
            let result = current
                .finish()
                .map_err(|error| ResetHydrationError::Invalid(ClientError::Protocol(error)))?;
            self.check(clock).map_err(ResetHydrationError::Budget)?;
            return match result {
                CursorRead::Reset { cursor, root, .. } => Ok(ResetPageProgress::Complete {
                    cursor,
                    root,
                    newly_bound,
                }),
                CursorRead::Events { .. } => {
                    Err(ResetHydrationError::Invalid(ClientError::Protocol(
                        "publication hydration completed as an event batch".to_owned(),
                    )))
                }
            };
        }

        self.expected_page = admitted_next.ok_or_else(|| {
            ResetHydrationError::Invalid(ClientError::Protocol(
                "publication reset omitted continuation".to_owned(),
            ))
        })?;
        self.hydrator = Some(current);
        self.check(clock).map_err(ResetHydrationError::Budget)?;
        Ok(ResetPageProgress::Continue {
            continuation: self.expected_page.clone(),
            newly_bound,
        })
    }
}
