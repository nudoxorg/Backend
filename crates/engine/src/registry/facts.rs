//! Versioned mutable facts about an immutable package release.
//!
//! Archive bytes and their content identity never change. Registry policy and
//! observations do: a release can be yanked, deprecated, unlisted, retracted,
//! or associated with advisories after the archive was cached. Keeping these
//! facts in their own content-versioned value lets the owner publish a tiny
//! metadata delta without downloading or re-indexing the artifact again.

/// Registry policy applied to an otherwise immutable release.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ReleaseStanding {
    /// The registry currently offers the release to new resolutions.
    Available = 0,
    /// The publisher withdrew the release from new resolution.
    Yanked = 1,
    /// The publisher retained the release but recommends a replacement.
    Deprecated = 2,
    /// The release remains addressable but is hidden from ordinary listings.
    Unlisted = 3,
    /// The ecosystem declared the version unsuitable for selection.
    Retracted = 4,
    /// The registry previously reported the release and now reports deletion.
    Removed = 5,
}

impl TryFrom<u8> for ReleaseStanding {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Available),
            1 => Ok(Self::Yanked),
            2 => Ok(Self::Deprecated),
            3 => Ok(Self::Unlisted),
            4 => Ok(Self::Retracted),
            5 => Ok(Self::Removed),
            _ => Err(()),
        }
    }
}

/// Why an ecosystem cannot provide a meaningful download count.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum DownloadCountGap {
    /// The registry API exposes no count.
    Unsupported = 0,
    /// The registry exposes counts only to an authenticated publisher.
    Privileged = 1,
    /// The value is sampled or withheld for privacy.
    Withheld = 2,
    /// The source was temporarily unavailable while other metadata succeeded.
    Unavailable = 3,
}

impl TryFrom<u8> for DownloadCountGap {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unsupported),
            1 => Ok(Self::Privileged),
            2 => Ok(Self::Withheld),
            3 => Ok(Self::Unavailable),
            _ => Err(()),
        }
    }
}

/// A count observation whose uncertainty is never collapsed to zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DownloadCount {
    /// Registry supplied an exact cumulative release count.
    Exact(u64),
    /// Registry supplied an estimate or a periodically sampled value.
    Approximate(u64),
    /// Registry cannot provide the observation, with a typed reason.
    NotReported(DownloadCountGap),
}

/// Security state for the release at one advisory frontier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SecurityStanding {
    /// No advisory authority has evaluated this release yet.
    Unassessed,
    /// The selected advisory frontier contains no matching active advisory.
    NoKnownAdvisory,
    /// One or more active advisories match the release.
    Affected {
        /// Number of distinct canonical advisories after alias coalescing.
        advisories: u32,
        /// Highest normalized severity, 0 (unknown) through 4 (critical).
        maximum_severity: u8,
    },
}

/// Mutable release metadata with a deterministic content identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseFacts {
    standing: ReleaseStanding,
    downloads: DownloadCount,
    security: SecurityStanding,
    native_metadata_version: [u8; 32],
    version: [u8; 32],
}

impl Default for ReleaseFacts {
    fn default() -> Self {
        Self::new(
            ReleaseStanding::Available,
            DownloadCount::NotReported(DownloadCountGap::Unsupported),
            SecurityStanding::Unassessed,
        )
    }
}

impl ReleaseFacts {
    /// Constructs a canonical fact value and its content identity.
    #[must_use]
    pub fn new(
        standing: ReleaseStanding,
        downloads: DownloadCount,
        security: SecurityStanding,
    ) -> Self {
        Self::new_with_native_metadata(standing, downloads, security, [0; 32])
    }

    /// Constructs facts bound to one native metadata identity.
    #[must_use]
    pub fn new_with_native_metadata(
        standing: ReleaseStanding,
        downloads: DownloadCount,
        security: SecurityStanding,
        native_metadata_version: [u8; 32],
    ) -> Self {
        let mut canonical = [0_u8; 48];
        canonical[..16].copy_from_slice(&canonical_facts(standing, downloads, security));
        canonical[16..].copy_from_slice(&native_metadata_version);
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry.release-facts.v2\0");
        hasher.update(&canonical);
        Self {
            standing,
            downloads,
            security,
            native_metadata_version,
            version: *hasher.finalize().as_bytes(),
        }
    }

    /// Registry selection policy at this fact frontier.
    #[must_use]
    pub const fn standing(self) -> ReleaseStanding {
        self.standing
    }

    /// Download observation without manufacturing zero for missing data.
    #[must_use]
    pub const fn downloads(self) -> DownloadCount {
        self.downloads
    }

    /// Advisory result at the selected security frontier.
    #[must_use]
    pub const fn security(self) -> SecurityStanding {
        self.security
    }

    /// Content identity used to avoid unchanged policy work.
    #[must_use]
    pub const fn version(self) -> [u8; 32] {
        self.version
    }

    /// Identity of the native metadata included in this fact frontier.
    #[must_use]
    pub const fn native_metadata_version(self) -> [u8; 32] {
        self.native_metadata_version
    }

    pub(crate) fn from_wire(
        standing: ReleaseStanding,
        downloads: DownloadCount,
        security: SecurityStanding,
        native_metadata_version: [u8; 32],
    ) -> Self {
        Self::new_with_native_metadata(standing, downloads, security, native_metadata_version)
    }

    pub(crate) fn with_security(self, security: SecurityStanding) -> Self {
        Self::new_with_native_metadata(
            self.standing,
            self.downloads,
            security,
            self.native_metadata_version,
        )
    }

    pub(crate) fn with_native_metadata(self, native_metadata_version: [u8; 32]) -> Self {
        Self::new_with_native_metadata(
            self.standing,
            self.downloads,
            self.security,
            native_metadata_version,
        )
    }
}

fn canonical_facts(
    standing: ReleaseStanding,
    downloads: DownloadCount,
    security: SecurityStanding,
) -> [u8; 16] {
    let mut canonical = [0_u8; 16];
    canonical[0] = standing as u8;
    match downloads {
        DownloadCount::Exact(value) => {
            canonical[1] = 0;
            canonical[2..10].copy_from_slice(&value.to_be_bytes());
        }
        DownloadCount::Approximate(value) => {
            canonical[1] = 1;
            canonical[2..10].copy_from_slice(&value.to_be_bytes());
        }
        DownloadCount::NotReported(reason) => {
            canonical[1] = 2;
            canonical[2] = reason as u8;
        }
    }
    match security {
        SecurityStanding::Unassessed => canonical[10] = 0,
        SecurityStanding::NoKnownAdvisory => canonical[10] = 1,
        SecurityStanding::Affected {
            advisories,
            maximum_severity,
        } => {
            canonical[10] = 2;
            canonical[11..15].copy_from_slice(&advisories.to_be_bytes());
            canonical[15] = maximum_severity.min(4);
        }
    }
    canonical
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_policy_changes_produce_distinct_fact_versions() {
        let available = ReleaseFacts::default();
        let yanked = ReleaseFacts::new(
            ReleaseStanding::Yanked,
            available.downloads(),
            available.security(),
        );
        let vulnerable = ReleaseFacts::new(
            ReleaseStanding::Available,
            available.downloads(),
            SecurityStanding::Affected {
                advisories: 1,
                maximum_severity: 4,
            },
        );
        assert_ne!(available.version(), yanked.version());
        assert_ne!(available.version(), vulnerable.version());
        assert_ne!(yanked.version(), vulnerable.version());
    }
}
