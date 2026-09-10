//! Defines project behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the project invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Projection budgets and the exact failures a projector reports instead of an empty page.

use compiler_ir::EntityId;

use crate::{ByteBudget, Count};

/// Most member rows one page retains before reporting truncation.
pub const MAX_PAGE_MEMBERS: u32 = 4096;
/// Most relation rows one page retains before reporting truncation.
pub const MAX_PAGE_RELATIONS: u32 = 4096;
/// Most signature tokens one declaration renders.
pub const MAX_SIGNATURE_TOKENS: u32 = 512;
/// Most documentation bytes one page retains.
pub const MAX_PROSE_BYTES: u32 = 256 * 1024;

/// Explicit budgets a projector must respect.
///
/// Row budgets are soft: exceeding one yields the admitted prefix plus a [`PageTruncation`] the
/// renderer shows. The prose budget is hard, because documentation that overruns a quarter of a
/// megabyte is a corruption-class event rather than a large page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionLimits {
    /// Member row budget.
    pub members: Count,
    /// Relation row budget.
    pub relations: Count,
    /// Signature token budget.
    pub signature_tokens: Count,
    /// Prose byte budget.
    pub prose_bytes: ByteBudget,
}

impl Default for ProjectionLimits {
    fn default() -> Self {
        Self {
            members: Count(MAX_PAGE_MEMBERS),
            relations: Count(MAX_PAGE_RELATIONS),
            signature_tokens: Count(MAX_SIGNATURE_TOKENS),
            prose_bytes: ByteBudget(MAX_PROSE_BYTES),
        }
    }
}

/// What a page had to leave out, so a renderer can say so in place instead of quietly showing a
/// prefix as if it were the whole thing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageTruncation {
    /// Member rows the image held beyond the admitted prefix.
    pub members: Option<Count>,
    /// Relation rows the image held beyond the admitted prefix.
    pub relations: Option<Count>,
    /// Signature tokens the declaration needed beyond the admitted prefix.
    pub signature_tokens: Option<Count>,
}

impl PageTruncation {
    /// Whether the page is the complete projection of its declaration.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.members.is_none() && self.relations.is_none() && self.signature_tokens.is_none()
    }
}

/// Exact projection failure. Every missing coordinate keeps its owner so the surface can say
/// precisely what the image lacked instead of showing an empty page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionError {
    /// The requested entity is not in the image.
    MissingEntity {
        /// Requested coordinate.
        entity: EntityId,
    },
    /// The image has no declaration identity for this entity, so no key can be minted.
    MissingIdentity {
        /// Coordinate without an identity.
        entity: EntityId,
    },
    /// A pooled coordinate the entity named was absent.
    MissingPool {
        /// Owning entity.
        entity: EntityId,
        /// Which pool.
        pool: MissingPool,
    },
    /// A member's parent did not agree with the list owner.
    ParentageMismatch {
        /// Owning entity.
        entity: EntityId,
        /// Offending member.
        member: EntityId,
    },
    /// Documentation exceeded the prose budget; the page retains the admitted prefix count.
    ProseBudget {
        /// Owning entity.
        entity: EntityId,
        /// Bytes required.
        required: ByteBudget,
        /// Bytes admitted.
        maximum: ByteBudget,
    },
    /// The address for an entity could not be spelled within the path budget.
    AddressDepth {
        /// Offending entity.
        entity: EntityId,
    },
}

/// Which pooled plane was absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingPool {
    /// Name atom.
    Name,
    /// Member list.
    Members,
    /// Documentation list.
    Documentation,
    /// Semantic type.
    Type,
    /// Text atom inside documentation.
    Text,
}
