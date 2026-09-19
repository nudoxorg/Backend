//! First-valid-winner hedging and cancellation propagation.

use crate::{CancelHandle, Cancellation, OutputAdmission, OutputVersion};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// Side of a duplicated pure-work race.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HedgeSide {
    /// Local attempt.
    Local,
    /// Remote attempt.
    Remote,
}

/// One accepted valid winner of a pure-work hedge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HedgeWinner {
    /// Side that published first valid output.
    pub side: HedgeSide,
    /// Immutable output selected for publication.
    pub output: OutputVersion,
}

/// Result of trying to publish one hedge candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HedgeError {
    /// A valid candidate already won.
    AlreadyWon,
    /// The candidate's side was cancelled before it could publish.
    Cancelled,
}

/// Coordinates first-valid-winner selection and cancellation of the loser.
pub struct HedgeRace {
    // Bits 0 and 1 record explicit side cancellation. Bits 2 and 3 record a
    // local or remote winner. One atomic word gives cancellation and winner
    // publication a single linearization point.
    winner: AtomicU8,
    winner_output: Mutex<Option<OutputVersion>>,
    local_cancel: CancelHandle,
    remote_cancel: CancelHandle,
    local_observation: Cancellation,
    remote_observation: Cancellation,
}

impl std::fmt::Debug for HedgeRace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HedgeRace")
            .field("winner", &self.winner.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl HedgeRace {
    /// Creates a race and the cancellation observations for local/remote
    /// workers. The race is valid only for repeatable pure work.
    #[must_use]
    pub fn new() -> (Self, Cancellation, Cancellation) {
        let (local, local_handle) = Cancellation::new();
        let (remote, remote_handle) = Cancellation::new();
        (
            Self {
                winner: AtomicU8::new(0),
                winner_output: Mutex::new(None),
                local_cancel: local_handle,
                remote_cancel: remote_handle,
                local_observation: local.clone(),
                remote_observation: remote.clone(),
            },
            local,
            remote,
        )
    }

    /// Attempts to publish a candidate that already carries an output
    /// admission capability. The capability is consumed whether the side
    /// wins or loses the race.
    /// # Errors
    ///
    /// Returns [`HedgeError::AlreadyWon`] after another valid candidate has
    /// published, or [`HedgeError::Cancelled`] after this side was cancelled.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the admission capability is consumed at the race linearization point"
    )]
    pub(crate) fn try_win(
        &self,
        side: HedgeSide,
        admission: OutputAdmission,
    ) -> Result<HedgeWinner, HedgeError> {
        let output = admission.output();
        // Hold the output slot across the winner CAS. A scheduler completion
        // can race a worker that already published through this API; keeping
        // the mutex guard until the output is written means a confirmation
        // never observes a winner bit without the corresponding output.
        let mut published = self
            .winner_output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (cancel_bit, winner_bit) = match side {
            HedgeSide::Local => (1, 4),
            HedgeSide::Remote => (2, 8),
        };
        loop {
            let state = self.winner.load(Ordering::Acquire);
            if state & 0b1100 != 0 {
                return Err(HedgeError::AlreadyWon);
            }
            if state & cancel_bit != 0 {
                return Err(HedgeError::Cancelled);
            }
            if self
                .winner
                .compare_exchange(
                    state,
                    state | winner_bit,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                *published = Some(output);
                break;
            }
        }
        drop(published);
        match side {
            HedgeSide::Local => self.cancel(HedgeSide::Remote),
            HedgeSide::Remote => self.cancel(HedgeSide::Local),
        }
        Ok(HedgeWinner { side, output })
    }

    pub(crate) fn confirm_winner(&self, side: HedgeSide, output: OutputVersion) -> bool {
        let state = self.winner.load(Ordering::Acquire);
        let winner_bit = match side {
            HedgeSide::Local => 4,
            HedgeSide::Remote => 8,
        };
        if state & winner_bit == 0 {
            return false;
        }
        self.winner_output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|published| published == output)
    }

    /// Returns the process/remote cancellation token for one side of the
    /// race. The worker adapter should forward its cancelled state to the
    /// child process or remote cancellation protocol.
    #[must_use]
    pub fn cancellation(&self, side: HedgeSide) -> Cancellation {
        match side {
            HedgeSide::Local => self.local_observation.clone(),
            HedgeSide::Remote => self.remote_observation.clone(),
        }
    }

    /// Propagates cancellation to one side without publishing a winner.
    pub fn cancel(&self, side: HedgeSide) {
        let (cancel_bit, winner_bit) = match side {
            HedgeSide::Local => (1, 4),
            HedgeSide::Remote => (2, 8),
        };
        let mut state = self.winner.load(Ordering::Acquire);
        while state & (cancel_bit | winner_bit) == 0 {
            match self.winner.compare_exchange_weak(
                state,
                state | cancel_bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => state = observed,
            }
        }
        if state & winner_bit != 0 {
            return;
        }
        match side {
            HedgeSide::Local => self.local_cancel.cancel(),
            HedgeSide::Remote => self.remote_cancel.cancel(),
        }
    }

    /// Returns the selected side, if one has won.
    #[must_use]
    pub fn winner(&self) -> Option<HedgeSide> {
        match self.winner.load(Ordering::Acquire) {
            state if state & 4 != 0 => Some(HedgeSide::Local),
            state if state & 8 != 0 => Some(HedgeSide::Remote),
            _ => None,
        }
    }

    /// Returns whether the selected winner has cancelled the supplied side.
    #[must_use]
    pub fn is_cancelled(&self, side: HedgeSide) -> bool {
        let bit = match side {
            HedgeSide::Local => 1,
            HedgeSide::Remote => 2,
        };
        self.winner.load(Ordering::Acquire) & bit != 0
    }
}

/// A shared race handle can be kept by a scheduler and passed to both worker
/// adapters without giving either side publication custody.
pub type SharedHedgeRace = Arc<HedgeRace>;
