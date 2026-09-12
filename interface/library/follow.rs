//! Defines follow behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the follow invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Subscriptions: following a package for releases, and the releases seen since.

use core::fmt;

use interface_core::PackageEcosystem;
use interface_documents::Count;
use interface_identity::{CoordinateParseError, PackageName, PackageVersion, ecosystem_tag, parse_ecosystem_tag};

use crate::{LibraryEpoch, ProjectError, ProjectId, ProjectSelector, Provenance, RegistryError, Timestamp};

/// Most packages one library follows.
pub const MAX_SUBSCRIPTIONS: usize = 4096;

/// One package regardless of version: what a subscription follows.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FollowKey {
    /// Ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Exact name.
    pub name: PackageName,
}

/// Exact follow-key admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowKeyError {
    /// No `ecosystem:` prefix.
    MissingEcosystem,
    /// The prefix is not one of the seven tags.
    UnknownEcosystem,
    /// The name was refused.
    Name(CoordinateParseError),
}

impl FollowKey {
    /// Parses `ecosystem:name`, also accepting a full coordinate and ignoring its version.
    ///
    /// # Errors
    ///
    /// Returns the exact missing or malformed part.
    pub fn parse(text: &str) -> Result<Self, FollowKeyError> {
        Self::parse_pinned(text).map(|(key, _)| key)
    }

    /// Parses `ecosystem:name[@version]`, returning the version when one was spelled.
    ///
    /// The version separator is the last `@` that is not the first byte of the name, so a scoped
    /// npm name such as `npm:@types/node` keeps its scope and `npm:@types/node@20.11.0` splits.
    ///
    /// # Errors
    ///
    /// Returns the exact missing or malformed part.
    pub fn parse_pinned(text: &str) -> Result<(Self, Option<PackageVersion>), FollowKeyError> {
        let Some((tag, rest)) = text.trim().split_once(':') else {
            return Err(FollowKeyError::MissingEcosystem);
        };
        let ecosystem = parse_ecosystem_tag(tag).ok_or(FollowKeyError::UnknownEcosystem)?;
        let (name, version) = match rest.rfind('@').filter(|index| *index > 0) {
            Some(index) => {
                let (name, tail) = rest.split_at(index);
                (name, tail.get(1..))
            }
            None => (rest, None),
        };
        let name = PackageName::new(name).map_err(FollowKeyError::Name)?;
        let version = match version {
            Some(spelling) => Some(PackageVersion::new(spelling).map_err(FollowKeyError::Name)?),
            None => None,
        };
        Ok((Self { ecosystem, name }, version))
    }
}

impl fmt::Display for FollowKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}",
            ecosystem_tag(self.ecosystem).as_str(),
            self.name.as_str()
        )
    }
}

/// One followed package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscription {
    /// What is followed.
    pub key: FollowKey,
    /// The newest version the reader has seen; `None` until the first check.
    pub seen: Option<PackageVersion>,
    /// When the follow was recorded.
    pub followed_at: Timestamp,
    /// The project folder it sits in, when any.
    pub project: Option<ProjectId>,
    /// When the registry was last consulted for it.
    pub checked_at: Option<Timestamp>,
    /// Releases newer than `seen` the last check found.
    pub unseen: Count,
}

/// Every subscription at one epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscriptions {
    /// Rows in follow order.
    pub rows: Box<[Subscription]>,
    /// Sum of unseen releases.
    pub unseen: Count,
    /// Epoch the rows were read at.
    pub epoch: LibraryEpoch,
}

/// One release newer than what the reader has seen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    /// Which subscription.
    pub key: FollowKey,
    /// The version.
    pub version: PackageVersion,
    /// When it was published.
    pub published_at: Option<Timestamp>,
    /// Carries a pre-release tag.
    pub prerelease: bool,
    /// Already marked seen by this reply.
    pub seen: bool,
}

/// One subscription the check could not consult.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseFault {
    /// Which subscription.
    pub key: FollowKey,
    /// Why.
    pub error: RegistryError,
}

/// The releases feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Releases {
    /// Newest first.
    pub rows: Box<[Release]>,
    /// Subscriptions whose registry could not answer.
    pub faults: Box<[ReleaseFault]>,
    /// When this check ran.
    pub checked_at: Timestamp,
    /// Freshness of the version lists consulted.
    pub provenance: Provenance,
}

/// One subscribe request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FollowRequest {
    /// What to follow.
    pub key: FollowKey,
    /// Folder to file it under.
    pub project: Option<ProjectSelector>,
}

/// One releases request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReleasesRequest {
    /// Whether to record every listed release as seen.
    pub mark_seen: bool,
}

/// Terminal of one subscribe or unsubscribe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FollowOutcome {
    /// Newly followed.
    Followed {
        /// The recorded row.
        subscription: Subscription,
    },
    /// Was already followed; the row is unchanged except for a project move.
    AlreadyFollowing {
        /// The row as it now stands.
        subscription: Subscription,
    },
    /// Stopped following.
    Unfollowed {
        /// What was dropped.
        key: FollowKey,
    },
    /// Was not followed; nothing changed.
    NotFollowing {
        /// What was asked.
        key: FollowKey,
    },
}

/// Exact follow failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FollowError {
    /// The subscription store failed.
    Store {
        /// Bounded description.
        detail: Box<str>,
    },
    /// The store is full.
    Full {
        /// Fixed maximum.
        maximum: usize,
    },
    /// The project to file under was refused.
    Project(ProjectError),
    /// The registry could not confirm the package exists.
    Registry(RegistryError),
}

impl FollowError {
    /// Stable cause slug, the same word on every surface.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::Store { .. } => "subscriptions-store",
            Self::Full { .. } => "subscriptions-full",
            Self::Project(error) => error.slug(),
            Self::Registry(error) => error.slug(),
        }
    }

    /// One line in the failure's own words.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Store { detail } => format!("the subscription store failed: {detail}"),
            Self::Full { maximum } => format!("the store holds its maximum of {maximum} subscriptions"),
            Self::Project(error) => error.detail(),
            Self::Registry(error) => error.detail(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_follow_key_ignores_the_version_and_round_trips() {
        let key = FollowKey::parse("cargo:serde@1.0.196");
        assert_eq!(key.as_ref().map(ToString::to_string), Ok("cargo:serde".to_owned()));
        assert_eq!(FollowKey::parse("npm:@types/node").map(|key| key.to_string()), Ok("npm:@types/node".to_owned()));
        assert_eq!(
            FollowKey::parse_pinned("npm:@types/node@20.11.0")
                .map(|(key, version)| (key.to_string(), version.map(|v| v.as_str().to_owned()))),
            Ok(("npm:@types/node".to_owned(), Some("20.11.0".to_owned())))
        );
        assert_eq!(
            FollowKey::parse_pinned("npm:@types/node").map(|(_, version)| version),
            Ok(None)
        );
        assert_eq!(FollowKey::parse("serde"), Err(FollowKeyError::MissingEcosystem));
        assert_eq!(FollowKey::parse("hex:serde"), Err(FollowKeyError::UnknownEcosystem));
    }
}
