use super::*;

/// Persistent local-first policy for forge acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeAcquisitionPolicy {
    /// Resolve and fetch missing objects through the configured transport.
    Online,
    /// Read only durable records and content objects.
    Offline,
}

/// Bounds applied before an archive, metadata body, or manifest can be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForgeAcquisitionLimits {
    /// Maximum source archive bytes.
    pub max_archive_bytes: u64,
    /// Maximum metadata response bytes.
    pub max_metadata_bytes: usize,
    /// Maximum README bytes retained in metadata.
    pub max_readme_bytes: usize,
    /// Maximum admitted file rows/bytes in the source tree.
    pub archive_budget: ArchiveBudget,
}

impl Default for ForgeAcquisitionLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            max_metadata_bytes: MAX_METADATA_BYTES,
            max_readme_bytes: MAX_README_BYTES,
            archive_budget: ArchiveBudget {
                max_entries: 100_000,
                max_bytes: 256 * 1024 * 1024,
                max_path_bytes: 4 * 1024,
                max_entry_bytes: 64 * 1024 * 1024,
            },
        }
    }
}

impl ForgeAcquisitionLimits {
    fn validate(self) -> Result<Self, ForgeRejectReason> {
        if self.max_archive_bytes == 0
            || self.max_archive_bytes > MAX_ARCHIVE_BYTES
            || self.max_metadata_bytes == 0
            || self.max_metadata_bytes > MAX_METADATA_BYTES
            || self.max_readme_bytes == 0
            || self.max_readme_bytes > MAX_README_BYTES
            || self.archive_budget.max_entries == 0
            || self.archive_budget.max_bytes == 0
            || self.archive_budget.max_bytes > MAX_MANIFEST_BYTES.saturating_mul(64)
        {
            return Err(ForgeRejectReason::Bounds);
        }
        Ok(self)
    }
}
