//! One affected-version specification.
//!
//! OSV JSON, RustSec TOML, and a catalog `version_range` string all lower into
//! this enum. Listings and the semver window test read the enum, so the
//! comma-joined wire string is only a rendering.

/// How an advisory names the versions it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ingest::advisory) enum VersionWindow {
    /// Versions the feed named explicitly.
    Listed(Vec<String>),
    /// An OSV event sequence, in feed order.
    Events(Vec<RangeEvent>),
}

/// One event in an OSV range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ingest::advisory) enum RangeEvent {
    /// `introduced`. `0` opens the window at the beginning.
    Introduced(String),
    /// `fixed`, exclusive.
    Fixed(String),
    /// `last_affected`, inclusive.
    LastAffected(String),
}

impl VersionWindow {
    /// Parse the canonical wire rendering. An empty or unrecognized string is
    /// not a window.
    pub(in crate::ingest::advisory) fn parse(raw: &str) -> Option<Self> {
        let mut listed = Vec::new();
        let mut events = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some(introduced) = part.strip_prefix("introduced:") {
                events.push(RangeEvent::Introduced(introduced.to_owned()));
            } else if let Some(fixed) = part.strip_prefix("fixed:") {
                events.push(RangeEvent::Fixed(fixed.to_owned()));
            } else if let Some(last) = part.strip_prefix("last_affected:") {
                events.push(RangeEvent::LastAffected(last.to_owned()));
            } else if events.is_empty() {
                listed.push(part.to_owned());
            } else {
                return None;
            }
        }
        if !events.is_empty() {
            Some(Self::Events(events))
        } else if !listed.is_empty() {
            Some(Self::Listed(listed))
        } else {
            None
        }
    }

    /// The wire string stored on the advisory row.
    pub(in crate::ingest::advisory) fn render(&self) -> String {
        match self {
            Self::Listed(versions) => versions.join(","),
            Self::Events(events) => events
                .iter()
                .map(|event| match event {
                    RangeEvent::Introduced(version) => format!("introduced:{version}"),
                    RangeEvent::Fixed(version) => format!("fixed:{version}"),
                    RangeEvent::LastAffected(version) => format!("last_affected:{version}"),
                })
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    /// Whether `version` is inside an event window.
    ///
    /// A listed set is not an event window. A token that is not semver,
    /// including a git revision, is outside.
    pub(in crate::ingest::advisory) fn contains(&self, version: &str) -> bool {
        let Self::Events(events) = self else {
            return false;
        };
        let Some(candidate) = parse_semver(version) else {
            return false;
        };
        let mut inside = false;
        for event in events {
            match event {
                RangeEvent::Introduced(introduced) => {
                    inside = introduced == "0"
                        || parse_semver(introduced).is_some_and(|floor| candidate >= floor);
                }
                RangeEvent::Fixed(fixed) => {
                    if inside && parse_semver(fixed).is_some_and(|ceiling| candidate >= ceiling) {
                        inside = false;
                    }
                }
                RangeEvent::LastAffected(last) => {
                    if inside && parse_semver(last).is_some_and(|ceiling| candidate > ceiling) {
                        inside = false;
                    }
                }
            }
        }
        inside
    }

    pub(in crate::ingest::advisory) fn is_events(&self) -> bool {
        matches!(self, Self::Events(_))
    }
}

fn parse_semver(raw: &str) -> Option<semver::Version> {
    let raw = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    semver::Version::parse(raw).ok()
}
