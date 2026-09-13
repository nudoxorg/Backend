//! Logical execution times, antichain frontiers, and retention pins.

use crate::FlowError;
use std::sync::Arc;

/// A logical execution epoch.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Epoch(pub u64);

/// A logical dataflow time.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Time {
    /// Commit or control epoch.
    pub epoch: Epoch,
    /// Recursive iteration coordinate.
    pub iteration: u16,
}

impl Time {
    /// Constructs a logical time from an execution epoch and iteration.
    #[must_use]
    pub const fn new(epoch: Epoch, iteration: u16) -> Self {
        Self { epoch, iteration }
    }

    /// Returns the smallest representable time strictly after this time.
    ///
    /// Iteration advances first. At the terminal iteration, the epoch
    /// advances while the terminal iteration is retained; resetting the
    /// iteration would make the new timestamp incomparable with its own
    /// predecessor under the product order used by [`Frontier`]. A carry
    /// past the largest epoch is reported instead of wrapping around.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when the epoch cannot be advanced.
    pub fn successor(self) -> Result<Self, FlowError> {
        if self.iteration < u16::MAX {
            return Ok(Self::new(self.epoch, self.iteration + 1));
        }
        Ok(Self::new(
            Epoch(self.epoch.0.checked_add(1).ok_or(FlowError::Overflow)?),
            u16::MAX,
        ))
    }

    /// Returns whether this time is strictly before `other`.
    #[must_use]
    pub fn before(self, other: Self) -> bool {
        self.strictly_less(other)
    }

    /// Returns the component-wise logical-time order used by antichains.
    #[must_use]
    pub fn less_equal(self, other: Self) -> bool {
        self.epoch <= other.epoch && self.iteration <= other.iteration
    }

    /// Returns whether this time is component-wise below `other`.
    #[must_use]
    pub fn strictly_less(self, other: Self) -> bool {
        self.less_equal(other) && self != other
    }

    /// Returns the component-wise least upper bound of two logical times.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        Self::new(
            Epoch(self.epoch.0.max(other.epoch.0)),
            self.iteration.max(other.iteration),
        )
    }
}

/// An execution upper frontier represented as an antichain of logical times.
///
/// Most ordinary flow inputs use a singleton frontier. Recursive inputs may
/// carry incomparable `(epoch, iteration)` coordinates; retaining the
/// antichain prevents one lane from falsely advancing another lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frontier {
    times: Arc<[Time]>,
    representative: Time,
}

impl Frontier {
    /// Maximum number of coordinates retained in one execution frontier.
    /// Recursive progress is intentionally a small antichain; accepting an
    /// unbounded wire list here would make normalization an allocation/CPU
    /// denial-of-service surface.
    pub const MAX_ELEMENTS: usize = 4_096;

    /// Creates a singleton frontier.
    #[must_use]
    pub fn new(upper: Time) -> Self {
        Self {
            times: Arc::from([upper]),
            representative: upper,
        }
    }

    /// Admits and normalizes an execution antichain.
    ///
    /// A time dominated by another member is removed. An empty frontier is
    /// rejected because it cannot serve as a progress or retention fence.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidFrontier`] when no time is supplied or
    /// [`FlowError::RecursionWorkLimit`] when the coordinate bound is
    /// exceeded.
    pub fn from_antichain(times: impl IntoIterator<Item = Time>) -> Result<Self, FlowError> {
        Self::normalize(times, None)
    }

    /// Admits and normalizes an antichain while charging each supplied
    /// coordinate to a caller-owned work scope.
    ///
    /// The budget is checked before sorting, so a hostile frontier cannot
    /// spend uncharged CPU or memory on normalization.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::from_antichain`] and
    /// [`FlowError::RecursionWorkLimit`] when `scope` cannot admit the input
    /// coordinates.
    pub fn from_antichain_budgeted(
        times: impl IntoIterator<Item = Time>,
        scope: &mut crate::WorkScope,
    ) -> Result<Self, FlowError> {
        Self::normalize(times, Some(scope))
    }

    fn normalize(
        times: impl IntoIterator<Item = Time>,
        mut scope: Option<&mut crate::WorkScope>,
    ) -> Result<Self, FlowError> {
        let mut normalized_input: Vec<Time> = Vec::new();
        for time in times {
            if let Some(scope) = scope.as_deref_mut() {
                scope.charge_rows(1)?;
            }
            if normalized_input.len() == Self::MAX_ELEMENTS {
                return Err(FlowError::RecursionWorkLimit);
            }
            normalized_input.push(time);
        }
        if normalized_input.is_empty() {
            return Err(FlowError::InvalidFrontier);
        }
        normalized_input.sort_unstable();
        normalized_input.dedup();
        // `Time` is sorted lexicographically by `(epoch, iteration)`. A
        // later coordinate can only be dominated by an earlier coordinate,
        // so the smallest iteration seen so far is enough to remove every
        // dominated element. This is O(n) after the deterministic sort and
        // retains incomparable recursive lanes exactly.
        let mut minimum_iteration = None;
        let mut normalized: Vec<Time> = Vec::with_capacity(normalized_input.len());
        for candidate in normalized_input {
            if minimum_iteration.is_some_and(|minimum| minimum <= candidate.iteration) {
                continue;
            }
            minimum_iteration = Some(candidate.iteration);
            normalized.push(candidate);
        }
        match normalized.last().copied() {
            Some(representative) => Ok(Self {
                times: Arc::from(normalized.into_boxed_slice()),
                representative,
            }),
            None => Err(FlowError::InvalidFrontier),
        }
    }

    /// Returns the incomparable frontier elements in deterministic order.
    #[must_use]
    pub fn elements(&self) -> &[Time] {
        &self.times
    }

    /// Returns a deterministic representative for APIs that require one
    /// coordinate. Callers that reason about progress should use
    /// [`Frontier::elements`] or [`Frontier::covers`].
    #[must_use]
    pub fn upper(&self) -> Time {
        self.representative
    }

    /// Returns whether a time is complete under this frontier.
    #[must_use]
    pub fn covers(&self, time: Time) -> bool {
        self.times.iter().any(|upper| time.strictly_less(*upper))
    }

    /// Returns whether the frontier can admit observations through `time`.
    #[must_use]
    pub fn allows_time(&self, time: Time) -> bool {
        self.times.iter().any(|upper| time.less_equal(*upper))
    }

    /// Returns whether `time` is at or after every retained lower bound.
    #[must_use]
    pub fn at_or_after(&self, time: Time) -> bool {
        self.times.iter().all(|since| since.less_equal(time))
    }

    /// Returns whether `new` is at least as advanced as `old`.
    #[must_use]
    pub fn advances_from(&self, old: &Self) -> bool {
        old.times.iter().all(|previous| {
            self.times
                .iter()
                .any(|current| previous.less_equal(*current))
        })
    }

    /// Returns whether two frontiers have the same upper bound.
    #[must_use]
    pub fn equivalent(&self, other: &Self) -> bool {
        self == other
    }
}

/// A trace observation pin.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Pin(u64);

impl Pin {
    /// Returns the opaque pin sequence for diagnostics.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// Monotone logical progress shared by a batch and its arrangement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceSpine {
    upper: Frontier,
    since: Frontier,
    next_pin: u64,
}

impl Default for TraceSpine {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceSpine {
    /// Creates an empty trace at the zero frontier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            upper: Frontier::new(Time::new(Epoch(0), 0)),
            since: Frontier::new(Time::new(Epoch(0), 0)),
            next_pin: 1,
        }
    }

    /// Creates a trace at a declared upper frontier with no retained history
    /// before that frontier. A trace constructed from an existing frontier
    /// has no claim to observations earlier than the supplied upper bound.
    #[must_use]
    pub fn from_frontier(upper: Frontier) -> Self {
        Self {
            since: upper.clone(),
            upper,
            next_pin: 1,
        }
    }

    /// Returns the current upper frontier.
    #[must_use]
    pub fn upper(&self) -> Frontier {
        self.upper.clone()
    }

    /// Returns the oldest retained logical time.
    #[must_use]
    pub fn since(&self) -> Time {
        self.since.upper()
    }

    /// Returns the complete retained since antichain.
    #[must_use]
    pub fn since_frontier(&self) -> Frontier {
        self.since.clone()
    }

    /// Advances the upper frontier monotonically.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::FrontierRegressed`] when the new antichain does
    /// not dominate the current upper frontier.
    pub fn advance_upper(&mut self, upper: Frontier) -> Result<(), FlowError> {
        if !upper.advances_from(&self.upper) {
            return Err(FlowError::FrontierRegressed);
        }
        self.upper = upper;
        Ok(())
    }

    /// Advances the since frontier without passing upper or regressing.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::SinceBeyondUpper`] or
    /// [`FlowError::SinceRegressed`] when the requested bound is invalid.
    pub fn advance_since(&mut self, since: Time) -> Result<(), FlowError> {
        self.advance_since_frontier(Frontier::new(since))
    }

    /// Advances the retained since antichain without passing upper or
    /// regressing any retained lane.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::SinceBeyondUpper`] when a lane exceeds upper, or
    /// [`FlowError::SinceRegressed`] when a lane moves backwards.
    pub fn advance_since_frontier(&mut self, since: Frontier) -> Result<(), FlowError> {
        if !since
            .elements()
            .iter()
            .all(|time| self.upper.allows_time(*time))
        {
            return Err(FlowError::SinceBeyondUpper);
        }
        if !since.advances_from(&self.since) {
            return Err(FlowError::SinceRegressed);
        }
        self.since = since;
        Ok(())
    }

    /// Allocates an opaque pin token without wrapping.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when the pin sequence is exhausted.
    pub fn pin(&mut self) -> Result<Pin, FlowError> {
        let pin = Pin(self.next_pin);
        self.next_pin = self.next_pin.checked_add(1).ok_or(FlowError::Overflow)?;
        Ok(pin)
    }

    /// Allocates a pin for an observation time at or after since.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidPin`] when the observation is outside the
    /// retained and upper frontiers, or [`FlowError::Overflow`] when the pin
    /// sequence is exhausted.
    pub fn pin_at(&mut self, time: Time) -> Result<Pin, FlowError> {
        if !self.since.at_or_after(time) || !self.upper.allows_time(time) {
            return Err(FlowError::InvalidPin);
        }
        self.pin()
    }
}
