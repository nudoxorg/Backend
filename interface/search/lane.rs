//! Defines lane behavior for `interface-search`, whose purpose is to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! This module owns the lane invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The four retrieval lanes and the coverage each one reports.

use interface_documents::Count;

/// One retrieval lane.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Lane {
    /// Exact key or exact-name lookup over local segments.
    Exact,
    /// Lexical name ranking over local segments and the durable Tantivy projection.
    Lexical,
    /// Relation traversal through Trustfall over reopened images.
    Graph,
    /// Vector similarity through Qdrant.
    Semantic,
}

impl Lane {
    /// Every lane in display order.
    pub const ALL: [Self; 4] = [Self::Exact, Self::Lexical, Self::Graph, Self::Semantic];

    /// Short label shown beside results.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Lexical => "names",
            Self::Graph => "graph",
            Self::Semantic => "semantic",
        }
    }
}

/// Bit set of lanes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LaneSet(u8);

/// Bit position of one lane inside a [`LaneSet`].
///
/// An explicit match rather than a discriminant cast, so the set's shape stays a decision of this
/// module rather than a consequence of the enum's declaration order.
const fn lane_bit(lane: Lane) -> u8 {
    1 << match lane {
        Lane::Exact => 0_u8,
        Lane::Lexical => 1,
        Lane::Graph => 2,
        Lane::Semantic => 3,
    }
}

impl LaneSet {
    /// Every lane.
    pub const ALL: Self = Self(0b1111);
    /// Only the local, always-available lanes.
    pub const LOCAL: Self = Self(lane_bit(Lane::Exact) | lane_bit(Lane::Lexical));
    /// No lanes.
    pub const EMPTY: Self = Self(0);

    /// Adds one lane.
    #[must_use]
    pub const fn with(self, lane: Lane) -> Self {
        Self(self.0 | lane_bit(lane))
    }

    /// Removes one lane.
    #[must_use]
    pub const fn without(self, lane: Lane) -> Self {
        Self(self.0 & !lane_bit(lane))
    }

    /// Whether a lane is requested.
    #[must_use]
    pub const fn contains(self, lane: Lane) -> bool {
        self.0 & lane_bit(lane) != 0
    }

    /// Requested lanes in display order.
    pub fn iter(self) -> impl Iterator<Item = Lane> {
        Lane::ALL.into_iter().filter(move |lane| self.contains(*lane))
    }
}

/// Why a lane could not run at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unavailability {
    /// No embedding model is configured, so no query vector exists.
    NoEmbedder,
    /// No Qdrant endpoint is configured.
    NoQdrant,
    /// The configured Qdrant endpoint did not answer.
    QdrantUnreachable,
    /// No package in scope has an index for this lane.
    NoIndex,
    /// The shelf is empty.
    NoPackages,
    /// The caller cancelled before the lane started.
    Cancelled,
    /// The caller did not request this lane.
    NotRequested,
}

/// Why a lane ran with reduced fidelity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Degradation {
    /// Some segments were unreadable and were skipped.
    MissingSegments,
    /// The durable projection lagged behind the newest publication.
    StaleProjection,
    /// Ranking hit its scratch budget and truncated candidates.
    CandidateBudget,
    /// The remote lane timed out after partial results.
    RemoteTimeout,
}

/// What a lane is entitled to claim about its rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coverage {
    /// Every in-scope package was searched.
    Complete,
    /// Only `searched` of `total` in-scope packages were searched.
    Partial {
        /// Packages searched.
        searched: Count,
        /// Packages in scope.
        total: Count,
    },
    /// Every package was searched, but with reduced fidelity.
    Degraded {
        /// Exact cause.
        reason: Degradation,
    },
    /// The lane produced no rows because it could not run.
    Unavailable {
        /// Exact cause.
        reason: Unavailability,
    },
}

impl Coverage {
    /// Whether the lane produced authoritative rows.
    #[must_use]
    pub const fn ran(self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }
}

/// Elapsed microseconds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Micros(pub u64);

/// One lane's report beside a terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneReport {
    /// Reporting lane.
    pub lane: Lane,
    /// Coverage claim.
    pub coverage: Coverage,
    /// Rows the lane contributed before merging.
    pub hits: Count,
    /// Wall time when measured.
    pub elapsed: Option<Micros>,
}

impl LaneReport {
    /// A lane the caller did not ask for.
    #[must_use]
    pub const fn not_requested(lane: Lane) -> Self {
        Self {
            lane,
            coverage: Coverage::Unavailable {
                reason: Unavailability::NotRequested,
            },
            hits: Count(0),
            elapsed: None,
        }
    }
}
