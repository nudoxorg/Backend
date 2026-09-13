//! Admission bounds and canonical domain contracts for the journal.

use super::record::JournalError;
use std::path::Path;

pub(super) const HEADER_BYTES: usize = 94;
pub(super) const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
pub(super) const MAX_JOURNAL_FRAMES: usize = 1_000_000;
pub(super) const MAX_JOURNAL_BYTES: usize = 256 * 1024 * 1024;

pub(super) fn containing_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Bounds retained while opening or recovering a journal.
///
/// The append path remains O(payload), while restart cannot be made into an
/// unbounded allocation by a valid but hostile history.  Callers that need a
/// smaller recovery envelope can pass an explicit value to
/// [`crate::journal::HashChainJournal::open_with_limits`] or
/// [`crate::journal::HashChainJournal::recover_with_limits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalLimits {
    /// Maximum number of complete frames accepted by a scan. The compatibility
    /// recovery API retains those frames; streaming visitors do not.
    pub max_frames: usize,
    /// Maximum on-disk bytes consumed by one scan, excluding a separately
    /// authenticated checkpoint frame.
    pub max_bytes: usize,
}

impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_frames: MAX_JOURNAL_FRAMES,
            max_bytes: MAX_JOURNAL_BYTES,
        }
    }
}

pub(super) fn validate_limits(limits: JournalLimits) -> Result<(), JournalError> {
    if limits.max_frames > MAX_JOURNAL_FRAMES || limits.max_bytes > MAX_JOURNAL_BYTES {
        return Err(JournalError::Bounds);
    }
    Ok(())
}

/// A journal domain supplies stable tags.
pub trait JournalDomain: Send + Sync + 'static {
    /// Domain byte.
    const DOMAIN: u8;
    /// Record type tag.
    const TYPE: u16;
    /// Record codec version.
    const VERSION: u8;
}

/// Canonical record codec used by [`crate::journal::HashChainJournal`].
pub trait JournalCodec: JournalDomain {
    /// Record carried by this domain.
    type Record;

    /// Encodes one record without an outer frame.
    fn encode(record: &Self::Record, output: &mut Vec<u8>);
    /// Decodes one bounded canonical record.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError>;

    /// Validates one bounded canonical record without retaining it.
    ///
    /// The default implementation preserves the original codec contract by
    /// decoding and re-encoding the value.  Codecs whose wire grammar can be
    /// checked directly should override this method to avoid allocating a
    /// second owned record during every journal scan.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn validate(bytes: &[u8]) -> Result<(), JournalError> {
        let record = Self::decode(bytes)?;
        let mut canonical = Vec::new();
        Self::encode(&record, &mut canonical);
        if canonical != bytes {
            return Err(JournalError::Corrupt("noncanonical record"));
        }
        Ok(())
    }
}
