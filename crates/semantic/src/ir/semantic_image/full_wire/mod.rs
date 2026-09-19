//! Full portable semantic-image grammar.
//!
//! This module owns every plane reachable from [`crate::ir::SemanticReader`].
//! Its encoder, structural validator, and borrowed reader form one
//! transaction; it never reuses native `Ir` layout bytes:
//! every tag, range, and endpoint is written as explicit little-endian cells.

mod decode;
mod encode;
mod extensions_decode;
mod fault;
mod plan;
mod typed;
mod typed_decode;
mod validate;
mod view;
mod wire;

#[cfg(test)]
mod tests;

pub use encode::{encode_full_semantic_image, full_semantic_image_len};
pub use fault::{
    FullSemanticImageError, FullSemanticImageFault, FullSemanticImageField,
    FullSemanticImageIdentityField,
};
pub(crate) use typed::FullTypedPlan;
pub use view::SemanticImageView;
