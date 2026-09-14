use super::{EmptyWire, WireCertificate, WireSchema};
use crate::canonical::encode_id;
use crate::{Coverage, Freshness, Lane, Reason};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FreshnessWire {
    Current(EmptyWire),
    Stale(StaleWire),
    Unknown(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StaleWire {
    observed: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoverageWire {
    Complete(EmptyWire),
    Partial(PartialWire),
    Unavailable(UnavailableWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PartialWire {
    lane: String,
    completed: u16,
    total: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnavailableWire {
    lane: String,
    reason: String,
}

pub(crate) fn freshness_to_wire(freshness: Freshness) -> FreshnessWire {
    match freshness {
        Freshness::Current => FreshnessWire::Current(EmptyWire {}),
        Freshness::Stale { observed } => FreshnessWire::Stale(StaleWire {
            observed: encode_id(observed.as_bytes()),
        }),
        Freshness::Unknown => FreshnessWire::Unknown(EmptyWire {}),
    }
}

pub(crate) fn freshness_from_wire(
    freshness: FreshnessWire,
    certificate: &WireCertificate,
) -> Result<Freshness, String> {
    Ok(match freshness {
        FreshnessWire::Current(_) => Freshness::Current,
        FreshnessWire::Stale(value) => Freshness::Stale {
            observed: certificate
                .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.observed)?,
        },
        FreshnessWire::Unknown(_) => Freshness::Unknown,
    })
}

pub(crate) fn coverage_to_wire(coverage: Coverage) -> CoverageWire {
    match coverage {
        Coverage::Complete => CoverageWire::Complete(EmptyWire {}),
        Coverage::Partial {
            lane,
            completed,
            total,
        } => CoverageWire::Partial(PartialWire {
            lane: lane_name(lane).to_owned(),
            completed,
            total,
        }),
        Coverage::Unavailable { lane, reason } => CoverageWire::Unavailable(UnavailableWire {
            lane: lane_name(lane).to_owned(),
            reason: reason_name(reason).to_owned(),
        }),
    }
}

pub(crate) fn coverage_from_wire(coverage: CoverageWire) -> Result<Coverage, String> {
    Ok(match coverage {
        CoverageWire::Complete(_) => Coverage::Complete,
        CoverageWire::Partial(value) if value.completed <= value.total => Coverage::Partial {
            lane: parse_lane(&value.lane)?,
            completed: value.completed,
            total: value.total,
        },
        CoverageWire::Partial(_) => return Err("invalid partial coverage bounds".to_owned()),
        CoverageWire::Unavailable(value) => Coverage::Unavailable {
            lane: parse_lane(&value.lane)?,
            reason: match value.reason.as_str() {
                "no_index" => Reason::NoIndex,
                "unconfigured" => Reason::Unconfigured,
                "offline" => Reason::Offline,
                "cancelled" => Reason::Cancelled,
                "incomplete" => Reason::Incomplete,
                _ => return Err("unknown coverage reason".to_owned()),
            },
        },
    })
}

fn parse_lane(value: &str) -> Result<Lane, String> {
    match value {
        "exact" => Ok(Lane::Exact),
        "names" => Ok(Lane::Names),
        "graph" => Ok(Lane::Graph),
        "semantic" => Ok(Lane::Semantic),
        _ => Err("unknown coverage lane".to_owned()),
    }
}

fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Exact => "exact",
        Lane::Names => "names",
        Lane::Graph => "graph",
        Lane::Semantic => "semantic",
    }
}

fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "no_index",
        Reason::Unconfigured => "unconfigured",
        Reason::Offline => "offline",
        Reason::Cancelled => "cancelled",
        Reason::Incomplete => "incomplete",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_coverage_round_trips_its_lane() {
        let coverage = Coverage::Partial {
            lane: Lane::Semantic,
            completed: 2,
            total: 7,
        };
        assert_eq!(coverage_from_wire(coverage_to_wire(coverage)), Ok(coverage));
    }
}
