//! Layered source federation.
//!
//! We have a single DEFINITIVE registry and any number of overlays that people can host themselves.

use super::source::SourceId;
use serde::{Deserialize, Serialize};

/// The role a source plays in a federation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum SourceRole {
    Definitive,
    Overlay,
}

/// A value tagged with the source it was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: SourceId,
    pub role: SourceRole,
}

impl<T> Sourced<T> {
    /// Map the value, preserving the source tag.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Sourced<U> {
        Sourced {
            value: f(self.value),
            source: self.source,
            role: self.role,
        }
    }

    /// Converts a reference to a Sourced<T> into a Sourced<&T>.
    pub fn as_ref(&self) -> Sourced<&T> {
        Sourced {
            value: &self.value,
            source: self.source,
            role: self.role,
        }
    }
}

/// A federation of registries: exactly one definitive base plus zero or more
/// precedence-ordered overlays.
pub struct Federation<S> {
    base: Sourced<S>,
    overlays: Vec<Sourced<S>>,
}

impl<S> Federation<S> {
    /// Start a federation from its definitive base.
    pub fn new(base_id: SourceId, base: S) -> Self {
        Self {
            base: Sourced {
                value: base,
                source: base_id,
                role: SourceRole::Definitive,
            },
            overlays: Vec::new(),
        }
    }

    /// Append an overlay at the lowest overlay precedence.
    pub fn with_overlay(mut self, id: SourceId, handle: S) -> Self {
        self.overlays.push(Sourced {
            value: handle,
            source: id,
            role: SourceRole::Overlay,
        });
        self
    }

    /// The definitive base's handle — the single-source fast path.
    pub fn base(&self) -> &S {
        &self.base.value
    }

    /// Every source in resolution-precedence order: overlays (highest first),
    /// then the definitive base.
    pub fn in_precedence(&self) -> impl Iterator<Item = Sourced<&S>> {
        self.overlays
            .iter()
            .map(|s| s.as_ref())
            .chain(std::iter::once(self.base.as_ref()))
    }

    /// The caller provides a closure to attempt resolution
    /// on a single handle.
    /// The first `Some(T)` found is automatically wrapped in its source metadata.
    pub fn query<T, F>(&self, mut f: F) -> Option<Sourced<T>>
    where
        F: FnMut(&S) -> Option<T>,
    {
        self.in_precedence().find_map(|sourced_handle| {
            // Apply the closure. If a package is found, attach the source metadata to it.
            f(sourced_handle.value).map(|resolved_val| sourced_handle.map(|_| resolved_val))
        })
    }
}
