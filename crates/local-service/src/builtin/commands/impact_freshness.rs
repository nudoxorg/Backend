//! Lossless encoding of selected Stylo attempt evidence into the owner-local
//! lease basis. These values describe capture time, not current freshness.
use super::impact_retention::{Freshness, Worker};
use backend_library::impact_comparison::ImpactAttemptAtCapture;
use backend_library::stylo_query::{StyloAttemptState, StyloFailure, StyloWorkerCapability};

pub(super) fn from_query(
    attempt: StyloAttemptState,
    worker: StyloWorkerCapability,
) -> (Freshness, Worker) {
    let freshness = match attempt {
        StyloAttemptState::LatestObserved => Freshness::LatestObserved,
        StyloAttemptState::RetainedAfterUnavailable { reason } => {
            Freshness::RetainedAfterUnavailable(match reason {
                StyloFailure::ReadUnavailable => 0,
                StyloFailure::SelectionUnavailable => 1,
                StyloFailure::InvalidUtf8 => 2,
                StyloFailure::Limit => 3,
                StyloFailure::ParserUnavailable => 4,
                StyloFailure::UnsupportedHardLimit => 5,
                StyloFailure::Deadline => 6,
                StyloFailure::WorkerFailure => 7,
                StyloFailure::PreferencesChanged => 8,
            })
        }
        StyloAttemptState::UnknownLegacy => Freshness::UnknownLegacy,
        _ => Freshness::Unknown,
    };
    let worker = match worker {
        StyloWorkerCapability::Unknown => Worker::Unknown,
        StyloWorkerCapability::NoPermit => Worker::NoPermit,
        StyloWorkerCapability::Admitted(id) => Worker::Admitted(id),
    };
    (freshness, worker)
}
pub(super) fn to_query(
    freshness: Freshness,
    worker: Worker,
) -> Option<(ImpactAttemptAtCapture, StyloWorkerCapability)> {
    let attempt = match freshness {
        Freshness::Unknown => ImpactAttemptAtCapture::Unknown,
        Freshness::UnknownLegacy => ImpactAttemptAtCapture::UnknownLegacy,
        Freshness::LatestObserved => ImpactAttemptAtCapture::LatestObserved,
        Freshness::RetainedAfterUnavailable(code) => {
            ImpactAttemptAtCapture::RetainedAfterUnavailable {
                reason: match code {
                    0 => StyloFailure::ReadUnavailable,
                    1 => StyloFailure::SelectionUnavailable,
                    2 => StyloFailure::InvalidUtf8,
                    3 => StyloFailure::Limit,
                    4 => StyloFailure::ParserUnavailable,
                    5 => StyloFailure::UnsupportedHardLimit,
                    6 => StyloFailure::Deadline,
                    7 => StyloFailure::WorkerFailure,
                    8 => StyloFailure::PreferencesChanged,
                    _ => return None,
                },
            }
        }
    };
    let worker = match worker {
        Worker::Unknown => StyloWorkerCapability::Unknown,
        Worker::NoPermit => StyloWorkerCapability::NoPermit,
        Worker::Admitted(id) => StyloWorkerCapability::Admitted(id),
    };
    Some((attempt, worker))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_retained_and_unknown_round_trip_without_inventing_freshness() {
        let cases = [
            (
                StyloAttemptState::LatestObserved,
                StyloWorkerCapability::Admitted([1; 32]),
                ImpactAttemptAtCapture::LatestObserved,
            ),
            (
                StyloAttemptState::RetainedAfterUnavailable {
                    reason: StyloFailure::ParserUnavailable,
                },
                StyloWorkerCapability::NoPermit,
                ImpactAttemptAtCapture::RetainedAfterUnavailable {
                    reason: StyloFailure::ParserUnavailable,
                },
            ),
            (
                StyloAttemptState::UnknownLegacy,
                StyloWorkerCapability::Unknown,
                ImpactAttemptAtCapture::UnknownLegacy,
            ),
        ];
        for (attempt, worker, expected) in cases {
            let encoded = from_query(attempt, worker);
            assert_eq!(to_query(encoded.0, encoded.1), Some((expected, worker)));
        }
        for reason in [
            StyloFailure::ReadUnavailable,
            StyloFailure::SelectionUnavailable,
            StyloFailure::InvalidUtf8,
            StyloFailure::Limit,
            StyloFailure::ParserUnavailable,
            StyloFailure::UnsupportedHardLimit,
            StyloFailure::Deadline,
            StyloFailure::WorkerFailure,
            StyloFailure::PreferencesChanged,
        ] {
            let encoded = from_query(
                StyloAttemptState::RetainedAfterUnavailable { reason },
                StyloWorkerCapability::NoPermit,
            );
            assert_eq!(
                to_query(encoded.0, encoded.1),
                Some((
                    ImpactAttemptAtCapture::RetainedAfterUnavailable { reason },
                    StyloWorkerCapability::NoPermit
                ))
            );
        }
        assert_eq!(
            to_query(Freshness::Unknown, Worker::Unknown),
            Some((
                ImpactAttemptAtCapture::Unknown,
                StyloWorkerCapability::Unknown
            ))
        );
        assert!(to_query(Freshness::RetainedAfterUnavailable(255), Worker::Unknown).is_none());
    }
}
