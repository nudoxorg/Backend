//! A unified, typed reference model — the git-like ref layer over the IR-VCS.
//!
//! The store is a **frozen mirror** of an upstream VCS, specialized to IR:
//! nobody hand-edits it; every state is re-derived from an upstream reference
//! and recorded. So *references*, not interactive edits, are the whole
//! interface. This module gives those references a single typed vocabulary:
//!
//! - [`Ref::Branch`] — a **channel** (movable). Mirrors an upstream branch head;
//!   re-recording advances it. Channels *are* branches.
//! - [`Ref::Tag`] — a frozen channel (`tag/{name}`). Mirrors an upstream tag.
//! - [`Ref::Version`] — a frozen channel (`version/{semver}`). A published
//!   package version; a specialization of a tag with a validated
//!   [`VersionLabel`] identity.
//! - [`Ref::Change`] — a content-addressed **change** (a commit). A point in a
//!   branch's history; first-class for history/diff/membership, not a channel to
//!   serve directly.
//!
//! Branches, tags, and versions all resolve to a **channel** and share one fast
//! serve path (`output`). Tags and versions live in reserved channel-name
//! namespaces (`tag/…`, `version/…`) so they can never collide with a branch,
//! which is a raw channel name (`main`, `develop`, `release/2.x`).

use std::fmt;

use smol_str::SmolStr;

use crate::error::VcsError;
use crate::repo::ChangeHashHex;
use crate::version::{VersionLabel, VersionState};

/// Reserved channel-name prefix for tags.
pub(crate) const TAG_PREFIX: &str = "tag/";
/// Reserved channel-name prefix for versions (mirrors [`VersionLabel::CHANNEL_PREFIX`]).
pub(crate) const VERSION_PREFIX: &str = VersionLabel::CHANNEL_PREFIX;

/// Validate a reference-name component (branch or tag). Interior `/` is allowed
/// (hierarchical names like `release/2.x`); leading/trailing/double `/`,
/// whitespace, control characters, and emptiness are not.
fn validate_component(kind: &str, name: &str) -> Result<(), VcsError> {
    let invalid = |reason: &str| VcsError::InvalidRefName {
        kind: kind.to_owned(),
        name: name.to_owned(),
        reason: reason.to_owned(),
    };
    if name.is_empty() {
        return Err(invalid("empty"));
    }
    if name.starts_with('/') || name.ends_with('/') {
        return Err(invalid("leading or trailing '/'"));
    }
    if name.contains("//") {
        return Err(invalid("empty path segment ('//')"));
    }
    if let Some(bad) = name.chars().find(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid(&format!("illegal character {bad:?}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// BranchName
// ---------------------------------------------------------------------------

/// A branch name — a live, movable channel. Mirrors an upstream branch head.
///
/// A branch is a **raw channel name**, so it may not begin with a reserved
/// namespace prefix (`tag/`, `version/`) that is carved out for tags/versions.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BranchName(SmolStr);

impl BranchName {
    /// Build a branch name, rejecting reserved-prefixed or channel-unsafe names.
    pub fn new(name: impl Into<SmolStr>) -> Result<Self, VcsError> {
        let name = name.into();
        validate_component("branch", &name)?;
        if name.starts_with(TAG_PREFIX) || name.starts_with(VERSION_PREFIX) {
            return Err(VcsError::InvalidRefName {
                kind: "branch".to_owned(),
                name: name.to_string(),
                reason: "uses a reserved namespace prefix (tag/ or version/)".to_owned(),
            });
        }
        Ok(Self(name))
    }

    /// The branch name string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The libpijul channel name — for a branch this is the raw name.
    pub(crate) fn channel_name(&self) -> String {
        self.0.to_string()
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// TagName
// ---------------------------------------------------------------------------

/// A tag name — an immutable marker. Mirrors an upstream tag.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TagName(SmolStr);

impl TagName {
    /// Build a tag name, rejecting channel-unsafe names.
    pub fn new(name: impl Into<SmolStr>) -> Result<Self, VcsError> {
        let name = name.into();
        validate_component("tag", &name)?;
        Ok(Self(name))
    }

    /// The tag name string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The libpijul channel name — `tag/{name}`.
    pub(crate) fn channel_name(&self) -> String {
        format!("{TAG_PREFIX}{}", self.0)
    }
}

impl fmt::Display for TagName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// RefKind / Ref
// ---------------------------------------------------------------------------

/// The kind of a [`Ref`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefKind {
    /// A branch (movable channel).
    Branch,
    /// A tag (frozen channel).
    Tag,
    /// A version (frozen channel, semver identity).
    Version,
    /// A change (a commit; a point in a branch's history).
    Change,
}

/// A first-class reference into the IR-VCS: a branch, tag, version, or change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ref {
    /// A branch head.
    Branch(BranchName),
    /// An immutable tag.
    Tag(TagName),
    /// A published version.
    Version(VersionLabel),
    /// A specific change (commit).
    Change(ChangeHashHex),
}

impl Ref {
    /// Convenience: a branch reference from a raw name.
    pub fn branch(name: &str) -> Result<Ref, VcsError> {
        Ok(Ref::Branch(BranchName::new(name)?))
    }

    /// Convenience: a tag reference from a raw name.
    pub fn tag(name: &str) -> Result<Ref, VcsError> {
        Ok(Ref::Tag(TagName::new(name)?))
    }

    /// Convenience: a version reference from a raw label.
    pub fn version(label: &str) -> Result<Ref, VcsError> {
        Ok(Ref::Version(VersionLabel::new(label)?))
    }

    /// This reference's [`RefKind`].
    pub fn kind(&self) -> RefKind {
        match self {
            Ref::Branch(_) => RefKind::Branch,
            Ref::Tag(_) => RefKind::Tag,
            Ref::Version(_) => RefKind::Version,
            Ref::Change(_) => RefKind::Change,
        }
    }

    /// The backing libpijul channel name, for channel-backed references
    /// (branch/tag/version). `None` for [`Ref::Change`] — a change is a point in
    /// a branch's history, not a channel tip.
    pub(crate) fn channel_name(&self) -> Option<String> {
        match self {
            Ref::Branch(b) => Some(b.channel_name()),
            Ref::Tag(t) => Some(t.channel_name()),
            Ref::Version(v) => Some(v.channel_name()),
            Ref::Change(_) => None,
        }
    }

    /// Whether this reference resolves to a servable channel.
    pub fn is_channel_backed(&self) -> bool {
        self.channel_name().is_some()
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Branch(b) => write!(f, "branch:{b}"),
            Ref::Tag(t) => write!(f, "tag:{t}"),
            Ref::Version(v) => write!(f, "version:{}", v.as_str()),
            Ref::Change(c) => write!(f, "change:{}", c.0),
        }
    }
}

// ---------------------------------------------------------------------------
// ResolvedRef
// ---------------------------------------------------------------------------

/// A resolved channel-backed reference: the channel it names and that channel's
/// current tip state.
#[derive(Clone, Debug)]
pub struct ResolvedRef {
    /// The kind of the resolved reference.
    pub kind: RefKind,
    /// The libpijul channel name backing it.
    pub channel_name: String,
    /// The channel's tip state (Merkle).
    pub state: VersionState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_reject_reserved_prefixes() {
        assert!(BranchName::new("main").is_ok());
        assert!(
            BranchName::new("release/2.x").is_ok(),
            "hierarchical branch ok"
        );
        assert!(BranchName::new("tag/foo").is_err(), "reserved tag/ prefix");
        assert!(
            BranchName::new("version/1.0.0").is_err(),
            "reserved version/ prefix"
        );
        assert!(BranchName::new("").is_err());
        assert!(BranchName::new("/leading").is_err());
        assert!(BranchName::new("trailing/").is_err());
        assert!(BranchName::new("a//b").is_err());
        assert!(BranchName::new("a b").is_err());
    }

    #[test]
    fn channel_names_are_namespaced_and_distinct() {
        let b = BranchName::new("main").unwrap();
        let t = TagName::new("release-1").unwrap();
        assert_eq!(b.channel_name(), "main");
        assert_eq!(t.channel_name(), "tag/release-1");
        assert_eq!(
            Ref::version("1.2.3").unwrap().channel_name().unwrap(),
            "version/1.2.3"
        );
        // No two kinds share a channel name.
        assert_ne!(b.channel_name(), t.channel_name());
    }

    #[test]
    fn change_ref_is_not_channel_backed() {
        let r = Ref::Change(ChangeHashHex("deadbeef".to_owned()));
        assert_eq!(r.kind(), RefKind::Change);
        assert!(!r.is_channel_backed());
        assert!(r.channel_name().is_none());
    }
}
