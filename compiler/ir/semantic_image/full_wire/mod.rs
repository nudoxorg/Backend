//! Full portable semantic-image grammar.
//!
//! This module is intentionally private while its encoder, structural
//! validator, and borrowed reader are assembled as one transaction.  Unlike
//! the three-directory core image, this grammar owns every plane reachable
//! from [`crate::SemanticReader`].  It never reuses native `Ir` layout bytes:
//! every tag, range, and endpoint is written as explicit little-endian cells.

mod fault;
mod encode;
mod plan;
mod typed;
mod wire;

pub(super) use fault::{FullSemanticImageFault, FullSemanticImageField};
pub(super) use encode::{encode_full_semantic_image, full_semantic_image_len};
pub(super) use typed::{FullTypedPlan, FullTypedPlanEdge, FullTypedPlanTarget};
pub(super) use wire::{FullDirectoryKind, FullImageLayout};
