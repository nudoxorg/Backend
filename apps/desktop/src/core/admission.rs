//! One live-read rule shared by Find and Ask. Retained bytes are evidence for
//! disclosed read-only pixels, never permission to issue an action.

use super::{Activity, Resource, ResourceTerminal, VersionedRoot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadHoldReason {
    OwnerUnavailable,
    AuthorityChanged,
    Reading,
    NotReady,
}

/// Completion of a read, independent of whether retained bytes can be drawn.
/// Joining concurrent reads gives failures precedence over pending work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ReadPhase { Ready, Pending, Terminal }

impl ReadPhase {
    #[must_use]
    pub fn join(self, other: Self) -> Self { self.max(other) }
}

#[derive(Debug)]
pub enum ResourceAdmission<'a, T> {
    Current(&'a T),
    Retained { value: &'a T, reason: ReadHoldReason },
    Pending(ReadHoldReason),
    Failed { retained: Option<&'a T>, terminal: &'a ResourceTerminal },
}

impl<'a, T> ResourceAdmission<'a, T> {
    #[must_use]
    pub fn phase(&self) -> ReadPhase {
        match self {
            Self::Current(_) => ReadPhase::Ready,
            Self::Retained { .. } | Self::Pending(_) => ReadPhase::Pending,
            Self::Failed { .. } => ReadPhase::Terminal,
        }
    }

    #[must_use]
    pub fn allows_actions(&self) -> bool { matches!(self, Self::Current(_)) }

    /// Only this value may supply routes/actions for the current owner.
    #[must_use]
    pub fn current_value(&self) -> Option<&'a T> {
        match self { Self::Current(value) => Some(*value), _ => None }
    }

    /// A visible predecessor must be disclosed and noninteractive.
    #[must_use]
    pub fn retained_value(&self) -> Option<&'a T> {
        match self { Self::Retained { value, .. } => Some(*value), Self::Failed { retained, .. } => *retained, _ => None }
    }
}

#[must_use]
pub fn admit_resource<T>(resource: &Resource<T>, current: VersionedRoot, owner_serving: bool) -> ResourceAdmission<'_, T> {
    let value = resource.loaded_value();
    // A failed destination never becomes Current merely because old bytes exist.
    if matches!(resource.terminal(), ResourceTerminal::Fault(_) | ResourceTerminal::Unavailable(_)) {
        return ResourceAdmission::Failed { retained: value, terminal: resource.terminal() };
    }
    let reason = if !owner_serving {
        Some(ReadHoldReason::OwnerUnavailable)
    } else if value.is_some() && (current.is_unserved() || !resource.value_root().is_some_and(|root| root.same_authority(current))) {
        Some(ReadHoldReason::AuthorityChanged)
    } else if matches!(resource.terminal(), ResourceTerminal::Partial) || resource.activity() != Activity::Rest {
        Some(ReadHoldReason::Reading)
    } else if value.is_none() {
        Some(ReadHoldReason::NotReady)
    } else { None };
    match (value, reason) {
        (Some(value), None) => ResourceAdmission::Current(value),
        (Some(value), Some(reason)) => ResourceAdmission::Retained { value, reason },
        (None, reason) => ResourceAdmission::Pending(reason.unwrap_or(ReadHoldReason::NotReady)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FaultCode, UnavailableReason};

    fn root(epoch: u64) -> VersionedRoot {
        VersionedRoot::synthetic(backend_library::view_state_root(&[("admission".into(), "fixture".into())]), epoch)
    }

    #[test]
    fn authority_activity_and_owner_all_have_to_admit_the_same_value() {
        let resource = Resource::loaded_at(7_u32, root(1));
        let current = admit_resource(&resource, root(1).observed_at(99), true);
        assert_eq!(current.current_value(), Some(&7));
        assert!(current.allows_actions(), "UI observation metadata is not authority");
        for (authority, serving, expected) in [
            (root(2), true, ReadHoldReason::AuthorityChanged),
            (root(1), false, ReadHoldReason::OwnerUnavailable),
        ] {
            let admission = admit_resource(&resource, authority, serving);
            assert!(!admission.allows_actions());
            assert_eq!(admission.retained_value(), Some(&7));
            assert!(matches!(admission, ResourceAdmission::Retained { reason, .. } if reason == expected));
        }
        let unserved = Resource::loaded_at(7_u32, VersionedRoot::unserved());
        assert!(!admit_resource(&unserved, VersionedRoot::unserved(), true).allows_actions(), "matching unserved placeholders are not producer authority");
        let partial = Resource::partial_at(7_u32, root(1)).resting();
        assert!(matches!(admit_resource(&partial, root(1), true), ResourceAdmission::Retained { reason: ReadHoldReason::Reading, .. }));
        assert!(!admit_resource(&partial, root(1), true).allows_actions(), "stopped intermediate bytes are never a completed reading");
        let working = resource.working();
        assert!(matches!(admit_resource(&working, root(1), true), ResourceAdmission::Retained { reason: ReadHoldReason::Reading, .. }));
    }

    #[test]
    fn failure_wins_over_retained_bytes_and_owner_absence() {
        let failed = Resource::loaded_at(7_u32, root(1)).mark_error(FaultCode::Transport, "fixture failed");
        let admission = admit_resource(&failed, root(1), false);
        assert!(matches!(admission, ResourceAdmission::Failed { .. }));
        assert_eq!(admission.retained_value(), Some(&7));
        assert!(!admission.allows_actions());
        let absent = Resource::<u32>::unavailable(UnavailableReason::OutOfScope);
        assert!(matches!(admit_resource(&absent, root(1), true), ResourceAdmission::Failed { retained: None, .. }));
        let pending = Resource::<u32>::not_yet();
        assert!(matches!(admit_resource(&pending, root(1), true), ResourceAdmission::Pending(_)));
    }

    #[test]
    fn a_failed_read_wins_over_waiting_in_either_completion_order() {
        let pending = admit_resource(&Resource::<u32>::not_yet().waiting(), root(1), true).phase();
        for failed in [Resource::<u32>::error(FaultCode::Missing, "source missing"), Resource::unavailable(UnavailableReason::OutOfScope)] {
            let terminal = admit_resource(&failed, root(1), false).phase();
            assert_eq!(terminal.join(pending), ReadPhase::Terminal);
            assert_eq!(pending.join(terminal), ReadPhase::Terminal);
        }
    }

    #[test]
    fn completion_requires_current_authority_and_a_completed_live_check() {
        let loaded = Resource::loaded_at(7_u32, root(1));
        assert_eq!(admit_resource(&loaded, root(1), true).phase(), ReadPhase::Ready);
        assert_eq!(admit_resource(&loaded, root(1).observed_at(99), true).phase(), ReadPhase::Ready);
        assert_eq!(admit_resource(&loaded, root(2), true).phase(), ReadPhase::Pending);
        assert_eq!(admit_resource(&loaded, root(1), false).phase(), ReadPhase::Pending);
        assert_eq!(admit_resource(&loaded.mark_error(FaultCode::Cancelled, "revoked"), root(1), false).phase(), ReadPhase::Terminal);
    }
}
