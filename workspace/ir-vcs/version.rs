//! Strongly-typed version identity for the historical-replay model.
//!
//! Efficient historical replay does **not** snapshot every version eagerly.
//! Storage is the content-shared pijul graph (unchanged symbols stored once
//! across all versions); a published version is a frozen libpijul channel —
//! forked from the working channel at publish time and never recorded onto
//! again — that shares that graph. Any version reconstructs in `O(state)`, any
//! symbol in `O(symbol)`, and per-symbol history / version diffs are native
//! graph reads.
//!
//! A [`VersionLabel`] names such a version channel; a [`VersionState`] is the
//! channel-tip Merkle that identifies its reconstructable IR (and keys the
//! serve cache).

use crate::vcs_types::ChangeSetFingerprint;
use smol_str::SmolStr;

use crate::error::VcsError;

// ---------------------------------------------------------------------------
// VersionLabel
// ---------------------------------------------------------------------------

/// A published version label (e.g. `"2.0.0"`).
///
/// Each tagged version is a frozen libpijul channel named `version/{label}`.
/// The label is validated to be a safe channel-name component so it can never
/// collide with the working channel namespace or corrupt a channel name.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct VersionLabel(SmolStr);

impl VersionLabel {
    /// Channel-name prefix under which every version channel lives. Chosen so a
    /// version channel can never equal a branch (a raw channel name).
    pub(crate) const CHANNEL_PREFIX: &'static str = "version/";

    /// Build a version label, rejecting empty or channel-unsafe strings.
    ///
    /// A label may not be empty and may not contain a path/channel separator
    /// (`/`), an ASCII control character, or whitespace.
    pub fn new(label: impl Into<SmolStr>) -> Result<Self, VcsError> {
        let label = label.into();
        if label.is_empty() {
            return Err(VcsError::InvalidRefName {
                kind: "version".to_owned(),
                name: label.to_string(),
                reason: "empty".to_owned(),
            });
        }
        if let Some(bad) = label
            .chars()
            .find(|c| *c == '/' || c.is_control() || c.is_whitespace())
        {
            return Err(VcsError::InvalidRefName {
                kind: "version".to_owned(),
                name: label.to_string(),
                reason: format!("illegal character {bad:?}"),
            });
        }
        Ok(Self(label))
    }

    /// The underlying label string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The libpijul channel name for this version (`"version/{label}"`).
    pub(crate) fn channel_name(&self) -> String {
        let mut s = String::with_capacity(Self::CHANNEL_PREFIX.len() + self.0.len());
        s.push_str(Self::CHANNEL_PREFIX);
        s.push_str(self.0.as_str());
        s
    }
}

// ---------------------------------------------------------------------------
// VersionState
// ---------------------------------------------------------------------------

/// The 32-byte channel-tip Merkle identifying a materialized version state.
///
/// Two version channels with the same `VersionState` reconstruct byte-identical
/// IR (they share the same alive-set over the content-shared pristine graph), so
/// this is the natural cache key for a sealed serve archive.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct VersionState([u8; 32]);

impl VersionState {
    /// Wrap the raw compressed-Merkle bytes of a channel tip.
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw compressed-Merkle bytes.
    pub fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Expose the state as an equality-only [`ChangeSetFingerprint`] for
    /// manifests / durable version→state maps.
    pub fn fingerprint(self) -> ChangeSetFingerprint {
        ChangeSetFingerprint::from_raw(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_rejects_empty_and_unsafe() {
        assert!(VersionLabel::new("").is_err(), "empty label rejected");
        assert!(VersionLabel::new("1/2").is_err(), "slash rejected");
        assert!(VersionLabel::new("a b").is_err(), "whitespace rejected");
        assert!(VersionLabel::new("v\u{7}").is_err(), "control char rejected");
    }

    #[test]
    fn label_channel_name_is_namespaced() {
        let v = VersionLabel::new("2.0.0").unwrap();
        assert_eq!(v.as_str(), "2.0.0");
        assert_eq!(v.channel_name(), "version/2.0.0");
        // Never collides with the working channel.
        assert_ne!(v.channel_name(), "main");
    }

    #[test]
    fn state_round_trips_bytes() {
        let s = VersionState::from_bytes([7u8; 32]);
        assert_eq!(s.to_bytes(), [7u8; 32]);
        assert_eq!(s.fingerprint().as_bytes(), &[7u8; 32]);
    }
}
