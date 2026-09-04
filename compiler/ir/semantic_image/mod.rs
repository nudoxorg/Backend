//! Portable subordinate semantic-image grammar.
//!
//! This is deliberately **not** a `FragmentView` section yet.  The first
//! grammar admits a self-contained core authority image only; its distinct
//! reader capability makes every omitted pooled/type/graph/extension plane
//! unrepresentable rather than treating absent bytes as semantic emptiness.

mod canonical;
mod encode;
mod decode;
mod fault;
mod model;
mod plan;
mod validate;
mod view;
mod wire;

#[cfg(test)]
mod tests;

pub(crate) use encode::{core_semantic_image_len, encode_core_semantic_image};
pub(crate) use fault::{CoreSemanticImageFault, CoreSemanticImageField};
pub(crate) use validate::reopen_core_semantic_image;
pub(crate) use view::CoreSemanticImageView;
