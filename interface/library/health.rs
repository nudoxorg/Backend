//! Defines health behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the health invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Honest capability health every surface shows in the same words.

/// One capability the library composes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Capability {
    /// The local compiler owner.
    Compiler,
    /// The shelf store.
    Shelf,
    /// Durable lexical projections.
    Lexical,
    /// The Trustfall relation graph.
    Graph,
    /// The Qdrant vector endpoint.
    Vector,
    /// An embedding model for query vectors.
    Embedder,
    /// The package registries over the network.
    Registry,
    /// Materialised package sources on this machine.
    Sources,
}

/// How many capabilities the library composes.
pub const CAPABILITY_COUNT: usize = 8;

impl Capability {
    /// Every capability in display order.
    pub const ALL: [Self; CAPABILITY_COUNT] = [
        Self::Compiler,
        Self::Shelf,
        Self::Lexical,
        Self::Graph,
        Self::Vector,
        Self::Embedder,
        Self::Registry,
        Self::Sources,
    ];

    /// Reader-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Compiler => "compiler",
            Self::Shelf => "shelf",
            Self::Lexical => "lexical index",
            Self::Graph => "relation graph",
            Self::Vector => "vector search",
            Self::Embedder => "embedding model",
            Self::Registry => "package registries",
            Self::Sources => "package sources",
        }
    }
}

/// State of one capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityState {
    /// Available now.
    Ready,
    /// Configured but the last probe failed.
    Unreachable {
        /// Bounded description.
        detail: Box<str>,
    },
    /// Not configured; the library never fabricates it.
    Unconfigured,
    /// This process opened the library without it.
    Detached,
}

/// Health of every capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Health {
    rows: [(Capability, CapabilityState); CAPABILITY_COUNT],
}

impl Health {
    pub(crate) fn new(rows: [(Capability, CapabilityState); CAPABILITY_COUNT]) -> Self {
        Self { rows }
    }

    /// Rows in display order.
    #[must_use]
    pub fn rows(&self) -> &[(Capability, CapabilityState); CAPABILITY_COUNT] {
        &self.rows
    }

    /// State of one capability.
    #[must_use]
    pub fn of(&self, capability: Capability) -> &CapabilityState {
        &self.rows[capability as usize].1
    }
}
