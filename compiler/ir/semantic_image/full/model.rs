//! Closed planning values for terminal pools, entity rows, and graph facts.

use alloc::{vec, vec::Vec};
use core::fmt;

use crate::{
    ArenaRange, EntityId, FactAvailability, Language, LinkId, LinkOccurrenceId,
    SemanticImageAuthority,
};

use super::super::fault::CoreSemanticImageFault;
use super::super::typed::TypedPlanError;

/// Terminal list domains remain outside the recursive type dependency graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalPoolDomain {
    EntityList,
    Documentation,
}

/// Exact terminal-list planner rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalPoolFault {
    GeometryOverflow { domain: TerminalPoolDomain, rows: usize },
    KeyLengthOverflow { domain: TerminalPoolDomain, row: u32 },
    MissingRow { domain: TerminalPoolDomain, row: u32, count: u32 },
    DuplicateCanonicalKey { domain: TerminalPoolDomain, first: u32, second: u32 },
}

/// Exact full-entity row planning rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FullEntityFault {
    GeometryOverflow { rows: usize },
    MissingEntity { entity: EntityId, count: u32 },
    KeyLengthOverflow { entity: EntityId },
}

/// Exact graph-plan rejection. It preserves graph coordinates as diagnostics,
/// but never uses them to decide canonical output order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GraphPlanFault {
    GeometryOverflow { rows: usize },
    MissingLink { link: LinkId, count: u32 },
    MissingOccurrence { occurrence: u32, count: u32 },
    DuplicateCanonicalRelation { first: LinkId, second: LinkId },
    OccurrenceAuthority {
        occurrence: u32,
        claimed: FactAvailability,
        has_source: bool,
    },
}

/// Exact sparse-extension canonicalization rejection. The closed `Language`
/// operand prevents a generic tagged-union error from erasing which of the
/// seven named sparse planes was malformed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExtensionPlanFault {
    GeometryOverflow { language: Language, rows: usize },
    KeyLengthOverflow { language: Language, fact: u32 },
    MissingFact { language: Language, fact: u32, count: u32 },
    DuplicateCanonicalKey { language: Language, first: u32, second: u32 },
    UnboundFact { language: Language, fact: u32 },
    SparseBinding { language: Language, entity: EntityId, fact: u32, count: u32 },
    Profile {
        language: Language,
        authority: SemanticImageAuthority,
    },
    MultiplePlanes {
        entity: EntityId,
        first: Language,
        second: Language,
    },
    Authority {
        entity: EntityId,
        claimed: FactAvailability,
        present: bool,
    },
}

/// Full planner failure keeps common-plan, typed-graph, terminal-pool, entity,
/// and graph causes distinct rather than collapsing them to an image error.
#[derive(Debug)]
pub(super) enum FullPlanError {
    Core(CoreSemanticImageFault),
    Typed(TypedPlanError),
    Terminal(TerminalPoolFault),
    Entity(FullEntityFault),
    Graph(GraphPlanFault),
    Extension(ExtensionPlanFault),
}

impl fmt::Display for FullPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(cause) => fmt::Display::fmt(cause, formatter),
            Self::Typed(cause) => fmt::Display::fmt(cause, formatter),
            Self::Terminal(cause) => write!(formatter, "terminal semantic-image plan rejected: {cause:?}"),
            Self::Entity(cause) => write!(formatter, "entity semantic-image plan rejected: {cause:?}"),
            Self::Graph(cause) => write!(formatter, "graph semantic-image plan rejected: {cause:?}"),
            Self::Extension(cause) => write!(formatter, "extension semantic-image plan rejected: {cause:?}"),
        }
    }
}
impl core::error::Error for FullPlanError {}

impl From<CoreSemanticImageFault> for FullPlanError {
    fn from(value: CoreSemanticImageFault) -> Self { Self::Core(value) }
}
impl From<TypedPlanError> for FullPlanError {
    fn from(value: TypedPlanError) -> Self { Self::Typed(value) }
}
impl From<TerminalPoolFault> for FullPlanError {
    fn from(value: TerminalPoolFault) -> Self { Self::Terminal(value) }
}
impl From<FullEntityFault> for FullPlanError {
    fn from(value: FullEntityFault) -> Self { Self::Entity(value) }
}
impl From<GraphPlanFault> for FullPlanError {
    fn from(value: GraphPlanFault) -> Self { Self::Graph(value) }
}
impl From<ExtensionPlanFault> for FullPlanError {
    fn from(value: ExtensionPlanFault) -> Self { Self::Extension(value) }
}

/// Flat, domain-framed key arena for one terminal pooled-list space.
pub(super) struct TerminalPoolPlan {
    pub(super) order: Vec<u32>,
    remap: Vec<u32>,
    key_bytes: Vec<u8>,
    key_ranges: Vec<ArenaRange>,
}

impl TerminalPoolPlan {
    pub(super) fn canonical(&self, raw: u32, domain: TerminalPoolDomain) -> Result<u32, TerminalPoolFault> {
        self.remap.get(usize::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
            domain,
            rows: self.remap.len(),
        })?).copied().ok_or(TerminalPoolFault::MissingRow {
            domain,
            row: raw,
            count: u32::try_from(self.remap.len()).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain,
                rows: self.remap.len(),
            })?,
        })
    }

    pub(super) fn key(&self, raw: u32, domain: TerminalPoolDomain) -> Result<&[u8], TerminalPoolFault> {
        let index = usize::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
            domain,
            rows: self.key_ranges.len(),
        })?;
        let range = self.key_ranges.get(index).copied().ok_or(TerminalPoolFault::MissingRow {
            domain,
            row: raw,
            count: u32::try_from(self.key_ranges.len()).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain,
                rows: self.key_ranges.len(),
            })?,
        })?;
        let start = usize::try_from(range.start).map_err(|_| TerminalPoolFault::KeyLengthOverflow {
            domain,
            row: raw,
        })?;
        let end = start.checked_add(usize::try_from(range.len).map_err(|_| {
            TerminalPoolFault::KeyLengthOverflow { domain, row: raw }
        })?).ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row: raw })?;
        self.key_bytes.get(start..end).ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row: raw })
    }

    pub(super) fn from_key_arena(
        domain: TerminalPoolDomain,
        key_bytes: Vec<u8>,
        key_ranges: Vec<ArenaRange>,
    ) -> Result<Self, TerminalPoolFault> {
        let count = key_ranges.len();
        for (index, range) in key_ranges.iter().copied().enumerate() {
            let row = u32::try_from(index).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain,
                rows: count,
            })?;
            let start = usize::try_from(range.start)
                .map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row })?;
            let length = usize::try_from(range.len)
                .map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row })?;
            let end = start.checked_add(length)
                .ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row })?;
            if end > key_bytes.len() {
                return Err(TerminalPoolFault::KeyLengthOverflow { domain, row });
            }
        }
        let mut order = Vec::with_capacity(count);
        for raw in 0..count {
            order.push(u32::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain,
                rows: count,
            })?);
        }
        order.sort_unstable_by(|left, right| {
            // The complete key arena was prevalidated immediately above. A
            // closure for the standard sort cannot return a typed error, so
            // this total accessor is justified by that one admission proof.
            key_after_validation(&key_bytes, &key_ranges, *left)
                .cmp(key_after_validation(&key_bytes, &key_ranges, *right))
        });
        for pair in order.windows(2) {
            let [left, right] = pair else { continue };
            if key_after_validation(&key_bytes, &key_ranges, *left)
                == key_after_validation(&key_bytes, &key_ranges, *right)
            {
                return Err(TerminalPoolFault::DuplicateCanonicalKey {
                    domain,
                    first: *left,
                    second: *right,
                });
            }
        }
        let mut remap = vec![0_u32; count];
        for (canonical, raw) in order.iter().copied().enumerate() {
            remap[usize::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain,
                rows: count,
            })?] = u32::try_from(canonical).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain,
                rows: count,
            })?;
        }
        Ok(Self { order, remap, key_bytes, key_ranges })
    }
}

#[allow(
    clippy::as_conversions,
    reason = "the containing preflight proves every u32 wire range fits this address space"
)]
fn key_after_validation<'a>(bytes: &'a [u8], ranges: &[ArenaRange], raw: u32) -> &'a [u8] {
    let index = raw as usize;
    let range = ranges[index];
    let start = range.start as usize;
    let end = start + range.len as usize;
    &bytes[start..end]
}

/// One canonical entity row’s full-only pooled references. Entity order itself
/// remains the core declaration-identity order; this value is its semantic
/// row key for future full wire validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FullEntityRow {
    pub(super) entity: EntityId,
    /// Canonical core entity coordinate, whose ordering is the exact
    /// declaration identity order retained by the common image plan.
    pub(super) canonical_entity: u32,
    pub(super) semantic_type: Option<u32>,
    pub(super) members: u32,
    pub(super) docs: u32,
    pub(super) attributes: u32,
}

pub(super) struct FullEntityPlan {
    pub(super) rows: Vec<FullEntityRow>,
    pub(super) key_bytes: Vec<u8>,
    pub(super) key_ranges: Vec<ArenaRange>,
}

/// Canonical relation rows, plus only the raw-to-canonical map necessary for
/// occurrence evidence. Identical occurrence evidence has no map because its
/// independent multiplicity is deliberately retained.
pub(super) struct GraphPlan {
    pub(super) relations: Vec<LinkId>,
    pub(super) relation_remap: Vec<u32>,
    pub(super) occurrences: Vec<LinkOccurrenceId>,
}

/// One entity-to-canonical-fact sparse binding. Both coordinates are already
/// canonical image lanes, never builder/interner ordinals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExtensionBinding {
    pub(super) entity: u32,
    pub(super) fact: u32,
}

/// Flat canonical facts and sparse entity bindings for one named language
/// plane. Keys are retained only for later explicit wire rows/validation;
/// facts remain typed values in the owned IR and are never serialized here.
pub(super) struct ExtensionPlanePlan {
    pub(super) order: Vec<u32>,
    pub(super) bindings: Vec<ExtensionBinding>,
    pub(super) key_bytes: Vec<u8>,
    pub(super) key_ranges: Vec<ArenaRange>,
}

/// Seven named plans, intentionally not an erased per-row payload union.
pub(super) struct ExtensionPlans {
    pub(super) typescript: ExtensionPlanePlan,
    pub(super) csharp: ExtensionPlanePlan,
    pub(super) go: ExtensionPlanePlan,
    pub(super) rust: ExtensionPlanePlan,
    pub(super) python: ExtensionPlanePlan,
    pub(super) java: ExtensionPlanePlan,
    pub(super) clang: ExtensionPlanePlan,
}
