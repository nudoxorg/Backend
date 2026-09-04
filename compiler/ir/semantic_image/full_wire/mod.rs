//! Full portable semantic-image grammar.
//!
//! This module is intentionally private while its encoder, structural
//! validator, and borrowed reader are assembled as one transaction.  Unlike
//! the three-directory core image, this grammar owns every plane reachable
//! from [`crate::SemanticReader`].  It never reuses native `Ir` layout bytes:
//! every tag, range, and endpoint is written as explicit little-endian cells.

mod fault;
mod encode;
mod extensions_decode;
mod decode;
mod plan;
mod typed;
mod typed_decode;
mod validate;
mod view;
mod wire;

#[cfg(test)]
mod tests;

pub use fault::{
    FullSemanticImageError, FullSemanticImageFault, FullSemanticImageField,
    FullSemanticImageIdentityField,
};
pub use encode::{encode_full_semantic_image, full_semantic_image_len};
pub use view::SemanticImageView;
pub(crate) use typed::FullTypedPlan;
pub use wire::FullDirectoryKind;
