//! A typed deferred reading intent. Navigation owns it exclusively while the
//! engine prepares; delivery proves the navigation epoch and package identity.
use crate::semantics::tour::Tour;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TourIntent {
    pub(crate) package: u32,
    pub(crate) at: usize,
    pub(crate) epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TourResolution {
    /// Another navigation owns the graph now. Delivery must do nothing.
    Stale,
    /// The requested package has fewer than three meaningful stops.
    Unavailable,
    /// A proven eligible tour, with its requested stop clamped once.
    Ready { data: Tour, at: usize },
}

impl TourIntent {
    pub(crate) fn resolve(self, current_epoch: u64, tour: Option<&Tour>) -> TourResolution {
        if self.epoch != current_epoch || tour.is_some_and(|tour| tour.package != self.package) {
            return TourResolution::Stale;
        }
        let Some(tour) = tour.filter(|tour| tour.shown()) else {
            return TourResolution::Unavailable;
        };
        TourResolution::Ready {
            data: tour.clone(),
            at: self.at.min(tour.stops.len() - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::tour::{Role, Stop, Why};

    fn tour(package: u32, count: usize) -> Tour {
        Tour {
            package,
            stops: (0..count)
                .map(|n| Stop {
                    node: n as u32,
                    role: Role::WhatYouHold,
                    why: Why::TurnsOnIt,
                })
                .collect(),
            items: count,
        }
    }

    #[test]
    fn delivery_requires_the_exact_navigation_epoch_and_package() {
        let intent = TourIntent {
            package: 7,
            at: 1,
            epoch: 4,
        };
        assert!(matches!(
            intent.resolve(4, Some(&tour(7, 3))),
            TourResolution::Ready { at: 1, .. }
        ));
        assert_eq!(intent.resolve(5, Some(&tour(7, 3))), TourResolution::Stale);
        assert_eq!(intent.resolve(4, Some(&tour(8, 3))), TourResolution::Stale);
        assert_eq!(intent.resolve(5, None), TourResolution::Stale);
    }

    #[test]
    fn pending_requests_do_not_claim_a_reading_path_until_it_exists() {
        let intent = TourIntent {
            package: 7,
            at: usize::MAX,
            epoch: 4,
        };
        assert_eq!(intent.resolve(4, None), TourResolution::Unavailable);
        assert_eq!(
            intent.resolve(4, Some(&tour(7, 2))),
            TourResolution::Unavailable
        );
        assert!(matches!(
            intent.resolve(4, Some(&tour(7, 3))),
            TourResolution::Ready { at: 2, .. }
        ));
    }
}
