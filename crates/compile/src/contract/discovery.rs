//! Discovery inputs, manifests, and candidate snapshots.

use super::{
    InputContentSchema, InputContentVersion, InputManifestId, InputManifestSchema, manifest_scope,
    typed_of,
};
use backend_version::CoverageWitness;
use std::{
    collections::BTreeMap,
    fmt,
    path::{Component, Path},
};

/// Marks a result as explicitly partial for a discovered manifest.
#[must_use]
pub fn partial_coverage(manifest: &InputManifest) -> CoverageWitness {
    let scope = manifest_scope(manifest);
    CoverageWitness::Partial(backend_version::UntrustedCoverageScope::from_scope_root(
        scope,
    ))
}

/// The kind of an input in an authority manifest.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InputKind {
    /// Source file or source package bytes.
    Source,
    /// Generated source or generated configuration.
    Generated,
    /// Explicit build/profile configuration.
    Configuration,
    /// Build-script input or output.
    BuildScript,
    /// Toolchain, SDK, or compiler executable input.
    Toolchain,
    /// Relevant environment value.
    Environment,
    /// Positive dependency or imported input.
    Dependency,
    /// A witnessed dependency that was absent during discovery.
    NegativeDependency,
    /// A bounded directory, glob, or search range included in discovery.
    Range,
}

/// One complete, content-addressed authority input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    kind: InputKind,
    name: String,
    digest: InputContentVersion,
    present: bool,
}

impl Input {
    /// Constructs a present input from its canonical name and content.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::EmptyName`] or
    /// [`ManifestError::NonCanonicalName`] when `name` is not a canonical
    /// logical input name.
    pub fn new(
        kind: InputKind,
        name: impl Into<String>,
        bytes: &[u8],
    ) -> Result<Self, ManifestError> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self {
            kind,
            name,
            digest: typed_of::<InputContentSchema>(bytes),
            present: true,
        })
    }

    /// Constructs an explicit absent input, useful for negative dependencies.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::EmptyName`] or
    /// [`ManifestError::NonCanonicalName`] when `name` is not a canonical
    /// logical input name.
    pub fn absent(kind: InputKind, name: impl Into<String>) -> Result<Self, ManifestError> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self {
            kind,
            name,
            digest: typed_of::<InputContentSchema>(b"absent"),
            present: false,
        })
    }

    /// Returns the input class.
    #[must_use]
    pub const fn kind(&self) -> InputKind {
        self.kind
    }

    /// Returns the canonical logical name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the content or absence digest.
    #[must_use]
    pub const fn digest(&self) -> InputContentVersion {
        self.digest
    }

    /// Returns whether the input exists.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.present
    }
}

/// A sorted, duplicate-free manifest of every authority input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputManifest {
    inputs: Vec<Input>,
    digest: InputManifestId,
}

impl InputManifest {
    /// Builds a deterministic manifest and rejects ambiguous duplicate names.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Duplicate`] when two inputs have the same
    /// kind and logical name.
    pub fn new(mut inputs: Vec<Input>) -> Result<Self, ManifestError> {
        inputs.sort_by(|left, right| (left.kind, &left.name).cmp(&(right.kind, &right.name)));
        for pair in inputs.windows(2) {
            if pair[0].kind == pair[1].kind && pair[0].name == pair[1].name {
                return Err(ManifestError::Duplicate {
                    name: pair[0].name.clone(),
                });
            }
        }
        let mut encoded = Vec::new();
        for input in &inputs {
            encoded.push(input.kind as u8);
            encoded.push(u8::from(input.present));
            put_str(&mut encoded, &input.name);
            encoded.extend_from_slice(input.digest.as_bytes());
        }
        Ok(Self {
            inputs,
            digest: typed_of::<InputManifestSchema>(&encoded),
        })
    }

    /// Returns inputs in canonical order.
    #[must_use]
    pub fn inputs(&self) -> &[Input] {
        &self.inputs
    }

    /// Returns the deterministic manifest identity.
    #[must_use]
    pub const fn digest(&self) -> InputManifestId {
        self.digest
    }

    /// Finds an input by class and name.
    #[must_use]
    pub fn get(&self, kind: InputKind, name: &str) -> Option<&Input> {
        self.inputs
            .iter()
            .find(|input| input.kind == kind && input.name == name)
    }

    pub(crate) fn has_closed_boundary(&self) -> bool {
        self.inputs
            .iter()
            .any(|input| matches!(input.kind, InputKind::NegativeDependency | InputKind::Range))
    }

    pub(crate) fn has_present_source(&self) -> bool {
        self.inputs.iter().any(|input| {
            input.present && matches!(input.kind, InputKind::Source | InputKind::Generated)
        })
    }
}

/// Exact manifest construction failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    /// An input name was empty.
    EmptyName,
    /// An input name was absolute or used a non-canonical path component.
    NonCanonicalName(String),
    /// Two inputs had the same kind and logical name.
    Duplicate {
        /// The duplicate logical name.
        name: String,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => f.write_str("input name is empty"),
            Self::NonCanonicalName(name) => write!(f, "input name is not canonical: {name:?}"),
            Self::Duplicate { name } => write!(f, "duplicate input: {name:?}"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn validate_name(name: &str) -> Result<(), ManifestError> {
    if name.is_empty() {
        return Err(ManifestError::EmptyName);
    }
    let path = Path::new(name);
    if path.is_absolute()
        || name.contains('\\')
        || name.as_bytes().contains(&0)
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains("//")
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(ManifestError::NonCanonicalName(name.to_owned()));
    }
    Ok(())
}

fn put_str(output: &mut Vec<u8>, value: &str) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value.as_bytes());
}

/// A change in one discovered input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputChange {
    /// An input exists only in the newer manifest.
    Added(Input),
    /// An input exists only in the older manifest.
    Removed(Input),
    /// An input exists in both manifests with different content or presence.
    Modified {
        /// The older input value.
        before: Input,
        /// The newer input value.
        after: Input,
    },
}

/// Deterministic delta between two discovery snapshots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryDelta {
    changes: Vec<InputChange>,
}

impl DiscoveryDelta {
    /// Computes added, removed, and modified inputs by class and name.
    #[must_use]
    pub fn between(before: &InputManifest, after: &InputManifest) -> Self {
        let left: BTreeMap<_, _> = before
            .inputs
            .iter()
            .map(|input| ((input.kind, input.name.as_str()), input))
            .collect();
        let right: BTreeMap<_, _> = after
            .inputs
            .iter()
            .map(|input| ((input.kind, input.name.as_str()), input))
            .collect();
        let mut changes = Vec::new();
        for (key, old) in &left {
            match right.get(key) {
                None => changes.push(InputChange::Removed((*old).clone())),
                Some(new) if old.digest != new.digest || old.present != new.present => {
                    changes.push(InputChange::Modified {
                        before: (*old).clone(),
                        after: (*new).clone(),
                    });
                }
                Some(_) => {}
            }
        }
        for (key, new) in &right {
            if !left.contains_key(key) {
                changes.push(InputChange::Added((*new).clone()));
            }
        }
        changes.sort_by(|a, b| change_key(a).cmp(&change_key(b)));
        Self { changes }
    }

    /// Returns changes in canonical order.
    #[must_use]
    pub fn changes(&self) -> &[InputChange] {
        &self.changes
    }

    /// Returns whether no input changed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

fn change_key(change: &InputChange) -> (InputKind, &str) {
    match change {
        InputChange::Added(input) | InputChange::Removed(input) => (input.kind, &input.name),
        InputChange::Modified { after, .. } => (after.kind, &after.name),
    }
}

/// A discovery candidate owned by an authority implementation.
///
/// This value carries the manifest and revision requested from discovery. It
/// is deliberately insufficient to mint complete coverage: a complete result
/// requires an independent authority registry to bind an immutable command
/// and a validated typed response to this candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoverySnapshot {
    manifest: InputManifest,
    sequence: u64,
}

impl DiscoverySnapshot {
    /// Creates a snapshot with a caller-owned discovery sequence.
    #[must_use]
    pub const fn new(manifest: InputManifest, sequence: u64) -> Self {
        Self { manifest, sequence }
    }

    /// Returns the complete snapshot manifest.
    #[must_use]
    pub const fn manifest(&self) -> &InputManifest {
        &self.manifest
    }

    /// Returns the authority's monotonic discovery sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns an unavailable placeholder for callers that have not yet
    /// admitted an authority response.
    ///
    /// A discovery candidate is caller-owned input. It cannot mint complete
    /// coverage by itself; use the native adapter's checked authority path,
    /// which creates a private authority registry and validates typed
    /// records before publishing complete coverage.
    #[must_use]
    pub fn complete_coverage(&self) -> CoverageWitness {
        let scope = manifest_scope(&self.manifest);
        CoverageWitness::Unavailable(backend_version::UntrustedCoverageScope::from_scope_root(
            scope,
        ))
    }
}
