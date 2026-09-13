//! Deterministic local/remote placement policy.

use super::capability::{LocalCapability, LocalState, RemoteCapability, RemoteState};
use super::cost::CostSnapshot;

/// Placement policy named by the local-first architecture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementClass {
    /// Reserve and start local work immediately; optional remote work may race
    /// only in separately admitted resources.
    LocalPreferred,
    /// Preserve a local fallback while selecting the lower calibrated
    /// completion-time route.
    RemoteOptional,
    /// The requested capability/input closure exists only remotely.
    RemoteRequired,
}

/// Route selected by the placement policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementDecision {
    /// Start local execution now.
    Local,
    /// Dispatch remotely while local fallback remains available.
    Remote,
    /// Start equivalent local and remote pure attempts under hedge budgets.
    Hedge,
    /// Hold the local reservation until its bounded latest-start deadline.
    WaitLocal,
    /// No authorized remote route exists for a remote-required request.
    RemoteOnlyUnavailable,
    /// The local dependency closure is incomplete and no remote route exists.
    MissingLocalInput,
    /// Neither local capability nor an authorized remote route is available.
    CapabilityUnavailable,
}

/// Detailed, checked placement inputs for policy-aware selection.
///
/// Every capability and cost snapshot is opaque and bound to one work key.
/// The chooser checks that the three observations agree before it considers
/// their state. Callers cannot route using a hand-written warm/ready enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlacementRequest {
    /// Checked local capability/input observation.
    pub local: LocalCapability,
    /// Checked remote capability/health observation.
    pub remote: RemoteCapability,
    /// Checked route-cost observation.
    pub costs: CostSnapshot,
    /// Owner-local clock used to validate observation windows.
    pub now: u64,
    /// Policy class.
    pub class: PlacementClass,
    /// Whether output equivalence permits duplicate pure execution.
    pub pure: bool,
    /// Whether global/per-job hedge admission is available.
    pub hedge_budget: bool,
    /// Whether the caller can preserve a local fallback reservation.
    pub fallback_reserved: bool,
}

/// Failure to admit a placement plan from checked observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementPlanError {
    /// The observations were bound to different work or authority keys.
    ObservationMismatch,
    /// No route satisfying the request's policy is currently available.
    Unavailable(PlacementDecision),
    /// Duplicate execution was selected without a repeatable recipe.
    HedgeNotAllowed,
}

impl std::fmt::Display for PlacementPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "placement plan error: {self:?}")
    }
}

impl std::error::Error for PlacementPlanError {}

/// Immutable placement capability admitted from one complete request.
///
/// The plan retains the exact checked observations that produced its route.
/// Consumers can pass this value across scheduling boundaries without
/// recomputing policy from mutable or caller supplied enum labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlacementPlan {
    request: PlacementRequest,
    decision: PlacementDecision,
}

impl PlacementPlan {
    /// Admits a route plan after checking all observation bindings and policy
    /// constraints.
    ///
    /// # Errors
    ///
    /// Returns [`PlacementPlanError::ObservationMismatch`] for observations
    /// referring to different work, [`PlacementPlanError::HedgeNotAllowed`]
    /// for a non-repeatable hedge, or [`PlacementPlanError::Unavailable`] if
    /// no permitted capability is available.
    pub fn admit(request: PlacementRequest) -> Result<Self, PlacementPlanError> {
        if request.local.key() != request.remote.key()
            || request.local.key() != request.costs.key()
            || request.local.authority() != request.remote.authority()
        {
            return Err(PlacementPlanError::ObservationMismatch);
        }
        let decision = choose_placement_with(request);
        if decision == PlacementDecision::Hedge && !request.pure {
            return Err(PlacementPlanError::HedgeNotAllowed);
        }
        if matches!(
            decision,
            PlacementDecision::RemoteOnlyUnavailable
                | PlacementDecision::MissingLocalInput
                | PlacementDecision::CapabilityUnavailable
        ) {
            return Err(PlacementPlanError::Unavailable(decision));
        }
        Ok(Self { request, decision })
    }

    /// Returns the route admitted by this plan.
    #[must_use]
    pub const fn decision(&self) -> PlacementDecision {
        self.decision
    }

    /// Returns the checked source request retained by this plan.
    #[must_use]
    pub const fn request(&self) -> PlacementRequest {
        self.request
    }

    /// Returns whether the admitted route duplicates a pure attempt.
    #[must_use]
    pub const fn is_hedged(&self) -> bool {
        matches!(self.decision, PlacementDecision::Hedge)
    }
}

/// Admits a placement plan from checked capabilities and costs.
///
/// # Errors
///
/// Returns the same validation errors as [`PlacementPlan::admit`].
pub fn admit_placement_plan(
    request: PlacementRequest,
) -> Result<PlacementPlan, PlacementPlanError> {
    PlacementPlan::admit(request)
}

/// Deterministic first choice using the explicit policy request.
#[must_use]
pub fn choose_placement_with(request: PlacementRequest) -> PlacementDecision {
    let key = request.local.key();
    if key != request.remote.key()
        || key != request.costs.key()
        || request.local.authority() != request.remote.authority()
    {
        return PlacementDecision::CapabilityUnavailable;
    }
    let local_valid = request
        .local
        .valid_for(key, request.local.authority(), request.now);
    let remote_valid = request
        .remote
        .valid_for(key, request.remote.authority(), request.now);
    let local = if local_valid {
        request.local.state()
    } else {
        LocalState::Unavailable
    };
    let remote = if remote_valid {
        request.remote.state()
    } else {
        RemoteState::Unavailable
    };

    if request.class == PlacementClass::RemoteRequired {
        return if remote.is_available() {
            PlacementDecision::Remote
        } else {
            PlacementDecision::RemoteOnlyUnavailable
        };
    }

    if local == LocalState::Unavailable {
        return if remote.is_available() {
            PlacementDecision::Remote
        } else {
            PlacementDecision::CapabilityUnavailable
        };
    }

    if local == LocalState::MissingInputs {
        return if remote.is_available() {
            PlacementDecision::Remote
        } else {
            PlacementDecision::MissingLocalInput
        };
    }

    if request.class == PlacementClass::LocalPreferred {
        // Local baseline starts immediately. An explicitly reserved hedge may
        // still race when pure work has a materially better remote estimate.
        if request.pure
            && request.hedge_budget
            && remote.is_available()
            && request.costs.usable_for(key, request.now)
            && (request.costs.remote().total() < request.costs.local()
                || request.costs.remote_p95() > request.costs.local())
        {
            return PlacementDecision::Hedge;
        }
        return PlacementDecision::Local;
    }

    // A stale or low-confidence estimate cannot move optional work off a
    // usable local path. It may still be used for remote-required work above.
    let costs_usable = request.costs.usable_for(key, request.now);
    if remote.is_available()
        && costs_usable
        && request.costs.remote().total() < request.costs.local()
        && (local == LocalState::Overloaded || request.fallback_reserved)
    {
        return PlacementDecision::Remote;
    }
    if local == LocalState::Overloaded && remote.is_available() && costs_usable {
        return PlacementDecision::Remote;
    }
    if local == LocalState::Ready {
        // Optional work keeps local latency when the remote capability or its
        // estimate is unavailable. The local reservation is executable now.
        return PlacementDecision::Local;
    }
    PlacementDecision::WaitLocal
}
