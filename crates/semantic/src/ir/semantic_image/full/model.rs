//! Closed planning values for terminal pools, entity rows, and graph facts.

use alloc::{vec, vec::Vec};
use core::fmt;

use crate::ir::{
    ArenaRange, EntityId, FactAvailability, Language, LinkId, LinkOccurrenceId,
    SemanticImageAuthority,
};

use super::super::fault::CoreSemanticImageFault;
use super::super::full_wire::FullSemanticImageFault;
use super::super::typed::TypedPlanError;

/// Terminal list domains remain outside the recursive type dependency graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPoolDomain {
    /// Entity lists interned by their canonical sequence of entity ordinals.
    EntityList,
    /// Documentation fragments interned by their canonical byte sequence.
    Documentation,
}

/// Exact terminal-list planner rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPoolFault {
    /// Sizing or indexing the terminal-pool arena exceeded its coordinate width.
    GeometryOverflow {
        /// Terminal-list namespace being planned.
        domain: TerminalPoolDomain,
        /// Host row count that could not be represented as a wire count.
        rows: usize,
    },
    /// A terminal key byte range cannot be represented or lies outside its key arena.
    KeyLengthOverflow {
        /// Terminal-list namespace containing the row.
        domain: TerminalPoolDomain,
        /// Zero-based raw row ordinal whose key could not be sized.
        row: u32,
    },
    /// A raw terminal-list coordinate does not name a row in the pool.
    MissingRow {
        /// Terminal-list namespace being indexed.
        domain: TerminalPoolDomain,
        /// Raw row ordinal requested by the caller.
        row: u32,
        /// Number of rows available in this pool.
        count: u32,
    },
    /// Two terminal rows have the same canonical byte key.
    DuplicateCanonicalKey {
        /// Terminal-list namespace with the collision.
        domain: TerminalPoolDomain,
        /// Earlier row with this key.
        first: u32,
        /// Later row with the same key.
        second: u32,
    },
}

/// Exact full-entity row planning rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullEntityFault {
    /// Entity table size or row index exceeded the image's coordinate width.
    GeometryOverflow {
        /// Host row count that could not be represented in the image.
        rows: usize,
    },
    /// A fragment entity coordinate does not name an entity in the full plan.
    MissingEntity {
        /// Raw fragment entity coordinate requested by the planner.
        entity: EntityId,
        /// Number of entities available to the full plan.
        count: u32,
    },
    /// A canonical entity key is too large for its encoded length field.
    KeyLengthOverflow {
        /// Entity whose declaration key could not be framed.
        entity: EntityId,
    },
}

/// Exact graph-plan rejection. It preserves graph coordinates as diagnostics,
/// but never uses them to decide canonical output order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphPlanFault {
    /// Relation or occurrence table size exceeded the image coordinate width.
    GeometryOverflow {
        /// Host row count that could not be represented in the image.
        rows: usize,
    },
    /// A relation coordinate does not name a row in the fragment graph.
    MissingLink {
        /// Raw relation coordinate requested by the planner.
        link: LinkId,
        /// Number of relations available in the fragment.
        count: u32,
    },
    /// An occurrence coordinate does not name a row in the occurrence plane.
    MissingOccurrence {
        /// Raw occurrence ordinal requested by the relation.
        occurrence: u32,
        /// Number of occurrence rows available in the plane.
        count: u32,
    },
    /// Two source relations produce an identical canonical graph key.
    DuplicateCanonicalRelation {
        /// Earlier raw relation coordinate with the canonical key.
        first: LinkId,
        /// Later raw relation coordinate with the same canonical key.
        second: LinkId,
    },
    /// Source-span presence disagrees with the occurrence's claimed source availability.
    OccurrenceAuthority {
        /// Raw occurrence ordinal whose evidence claim failed.
        occurrence: u32,
        /// Source availability stored for this occurrence in its authority plane.
        claimed: FactAvailability,
        /// Whether a source span is present in the decoded row.
        has_source: bool,
    },
}

/// Exact sparse-extension canonicalization rejection. The closed `Language`
/// operand prevents a generic tagged-union error from erasing which of the
/// seven named sparse planes was malformed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionPlanFault {
    /// Sparse extension pool geometry exceeded the image coordinate width.
    GeometryOverflow {
        /// Language plane being planned.
        language: Language,
        /// Host row count that could not be represented in the image.
        rows: usize,
    },
    /// An extension key byte range cannot be represented or lies outside its key arena.
    KeyLengthOverflow {
        /// Language plane containing the key.
        language: Language,
        /// Raw extension-fact row ordinal.
        fact: u32,
    },
    /// A referenced raw extension fact does not exist in its plane.
    MissingFact {
        /// Language plane being indexed.
        language: Language,
        /// Raw extension-fact row requested by the entity binding.
        fact: u32,
        /// Number of fact rows in the plane.
        count: u32,
    },
    /// Two sparse facts have the same language-specific canonical key.
    DuplicateCanonicalKey {
        /// Language plane with the collision.
        language: Language,
        /// Earlier raw fact row with the canonical key.
        first: u32,
        /// Later raw fact row with the same key.
        second: u32,
    },
    /// A fact row cannot be emitted because no declaration binds it.
    UnboundFact {
        /// Language plane containing the unused fact.
        language: Language,
        /// Raw fact row without an entity binding.
        fact: u32,
    },
    /// A sparse binding names a fact outside the language plane.
    SparseBinding {
        /// Language plane selected by the binding.
        language: Language,
        /// Entity row carrying the binding.
        entity: EntityId,
        /// Raw fact ordinal encoded by the binding.
        fact: u32,
        /// Number of facts available in this language plane.
        count: u32,
    },
    /// A language-specific plane conflicts with the image's language authority.
    Profile {
        /// Language whose extension facts were supplied.
        language: Language,
        /// Authority profile carried by the image.
        authority: SemanticImageAuthority,
    },
    /// One entity claims more than one mutually exclusive sparse language plane.
    MultiplePlanes {
        /// Entity with conflicting sparse authorities.
        entity: EntityId,
        /// First language plane claimed by the entity.
        first: Language,
        /// Additional language plane claimed by the entity.
        second: Language,
    },
    /// An entity's authority bit disagrees with whether a fact row is bound.
    Authority {
        /// Entity whose authority claim failed.
        entity: EntityId,
        /// Availability value declared by the entity.
        claimed: FactAvailability,
        /// Whether the language plane contains a bound fact for the entity.
        present: bool,
    },
}

/// Full planner failure keeps common-plan, typed-graph, terminal-pool, entity,
/// and graph causes distinct rather than collapsing them to an image error.
#[derive(Debug)]
pub enum FullPlanError {
    /// Shared canonical header or provenance planning failed.
    Core(CoreSemanticImageFault),
    /// Full-image wire grammar or geometry could not be represented.
    Wire(FullSemanticImageFault),
    /// Canonical typed dependency planning failed.
    Typed(TypedPlanError),
    /// Terminal entity-list or documentation pool planning failed.
    Terminal(TerminalPoolFault),
    /// Full entity row planning failed.
    Entity(FullEntityFault),
    /// Relation or occurrence graph planning failed.
    Graph(GraphPlanFault),
    /// Sparse extension-fact canonicalization failed.
    Extension(ExtensionPlanFault),
}

impl fmt::Display for FullPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(cause) => fmt::Display::fmt(cause, formatter),
            Self::Wire(cause) => fmt::Display::fmt(cause, formatter),
            Self::Typed(cause) => fmt::Display::fmt(cause, formatter),
            Self::Terminal(cause) => write!(
                formatter,
                "terminal semantic-image plan rejected: {cause:?}"
            ),
            Self::Entity(cause) => {
                write!(formatter, "entity semantic-image plan rejected: {cause:?}")
            }
            Self::Graph(cause) => {
                write!(formatter, "graph semantic-image plan rejected: {cause:?}")
            }
            Self::Extension(cause) => write!(
                formatter,
                "extension semantic-image plan rejected: {cause:?}"
            ),
        }
    }
}
impl core::error::Error for FullPlanError {}

impl From<CoreSemanticImageFault> for FullPlanError {
    fn from(value: CoreSemanticImageFault) -> Self {
        Self::Core(value)
    }
}
impl From<FullSemanticImageFault> for FullPlanError {
    fn from(value: FullSemanticImageFault) -> Self {
        Self::Wire(value)
    }
}
impl From<TypedPlanError> for FullPlanError {
    fn from(value: TypedPlanError) -> Self {
        Self::Typed(value)
    }
}
impl From<TerminalPoolFault> for FullPlanError {
    fn from(value: TerminalPoolFault) -> Self {
        Self::Terminal(value)
    }
}
impl From<FullEntityFault> for FullPlanError {
    fn from(value: FullEntityFault) -> Self {
        Self::Entity(value)
    }
}
impl From<GraphPlanFault> for FullPlanError {
    fn from(value: GraphPlanFault) -> Self {
        Self::Graph(value)
    }
}
impl From<ExtensionPlanFault> for FullPlanError {
    fn from(value: ExtensionPlanFault) -> Self {
        Self::Extension(value)
    }
}

/// Flat, domain-framed key arena for one terminal pooled-list space.
pub(crate) struct TerminalPoolPlan {
    pub(in crate::ir::semantic_image) order: Vec<u32>,
    remap: Vec<u32>,
    pub(in crate::ir::semantic_image) key_bytes: Vec<u8>,
    pub(in crate::ir::semantic_image) key_ranges: Vec<ArenaRange>,
}

impl TerminalPoolPlan {
    pub(crate) fn canonical(
        &self,
        raw: u32,
        domain: TerminalPoolDomain,
    ) -> Result<u32, TerminalPoolFault> {
        self.remap
            .get(
                usize::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
                    domain,
                    rows: self.remap.len(),
                })?,
            )
            .copied()
            .ok_or(TerminalPoolFault::MissingRow {
                domain,
                row: raw,
                count: u32::try_from(self.remap.len()).map_err(|_| {
                    TerminalPoolFault::GeometryOverflow {
                        domain,
                        rows: self.remap.len(),
                    }
                })?,
            })
    }

    #[cfg(test)]
    pub(crate) fn key(
        &self,
        raw: u32,
        domain: TerminalPoolDomain,
    ) -> Result<&[u8], TerminalPoolFault> {
        let index = usize::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
            domain,
            rows: self.key_ranges.len(),
        })?;
        let range = self
            .key_ranges
            .get(index)
            .copied()
            .ok_or(TerminalPoolFault::MissingRow {
                domain,
                row: raw,
                count: u32::try_from(self.key_ranges.len()).map_err(|_| {
                    TerminalPoolFault::GeometryOverflow {
                        domain,
                        rows: self.key_ranges.len(),
                    }
                })?,
            })?;
        let start = usize::try_from(range.start)
            .map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row: raw })?;
        let end = start
            .checked_add(
                usize::try_from(range.len)
                    .map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row: raw })?,
            )
            .ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row: raw })?;
        self.key_bytes
            .get(start..end)
            .ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row: raw })
    }

    pub(crate) fn from_key_arena(
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
            let end = start
                .checked_add(length)
                .ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row })?;
            if end > key_bytes.len() {
                return Err(TerminalPoolFault::KeyLengthOverflow { domain, row });
            }
        }
        let mut order = Vec::with_capacity(count);
        for raw in 0..count {
            order.push(
                u32::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
                    domain,
                    rows: count,
                })?,
            );
        }
        order.sort_unstable_by(|left, right| {
            // The complete key arena was prevalidated immediately above. A
            // closure for the standard sort cannot return a typed error, so
            // this total accessor is justified by that one admission proof.
            key_after_validation(&key_bytes, &key_ranges, *left).cmp(key_after_validation(
                &key_bytes,
                &key_ranges,
                *right,
            ))
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
        Ok(Self {
            order,
            remap,
            key_bytes,
            key_ranges,
        })
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
pub(crate) struct FullEntityRow {
    pub(in crate::ir::semantic_image) entity: EntityId,
    /// Canonical core entity coordinate, whose ordering is the exact
    /// declaration identity order retained by the common image plan.
    pub(in crate::ir::semantic_image) canonical_entity: u32,
    pub(in crate::ir::semantic_image) semantic_type: Option<u32>,
    pub(in crate::ir::semantic_image) members: u32,
    pub(in crate::ir::semantic_image) docs: u32,
    pub(in crate::ir::semantic_image) attributes: u32,
}

pub(crate) struct FullEntityPlan {
    pub(in crate::ir::semantic_image) rows: Vec<FullEntityRow>,
}

/// Canonical relation rows, plus only the raw-to-canonical map necessary for
/// occurrence evidence. Identical occurrence evidence has no map because its
/// independent multiplicity is deliberately retained.
pub(crate) struct GraphPlan {
    pub(in crate::ir::semantic_image) relations: Vec<LinkId>,
    pub(in crate::ir::semantic_image) relation_remap: Vec<u32>,
    pub(in crate::ir::semantic_image) occurrences: Vec<LinkOccurrenceId>,
}

/// One entity-to-canonical-fact sparse binding. Both coordinates are already
/// canonical image lanes, never builder/interner ordinals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExtensionBinding {
    pub(in crate::ir::semantic_image) entity: u32,
    pub(in crate::ir::semantic_image) fact: u32,
}

/// Flat canonical facts and sparse entity bindings for one named language
/// plane. Keys are retained only for later explicit wire rows/validation;
/// facts remain typed values in the owned IR and are never serialized here.
pub(crate) struct ExtensionPlanePlan {
    pub(in crate::ir::semantic_image) order: Vec<u32>,
    pub(in crate::ir::semantic_image) bindings: Vec<ExtensionBinding>,
    pub(in crate::ir::semantic_image) key_bytes: Vec<u8>,
    pub(in crate::ir::semantic_image) key_ranges: Vec<ArenaRange>,
}

/// Seven named plans, intentionally not an erased per-row payload union.
pub(crate) struct ExtensionPlans {
    pub(in crate::ir::semantic_image) typescript: ExtensionPlanePlan,
    pub(in crate::ir::semantic_image) csharp: ExtensionPlanePlan,
    pub(in crate::ir::semantic_image) go: ExtensionPlanePlan,
    pub(in crate::ir::semantic_image) rust: ExtensionPlanePlan,
    pub(in crate::ir::semantic_image) python: ExtensionPlanePlan,
    pub(in crate::ir::semantic_image) java: ExtensionPlanePlan,
    pub(in crate::ir::semantic_image) clang: ExtensionPlanePlan,
}
