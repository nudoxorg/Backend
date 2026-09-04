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
mod full;
mod full_wire;
mod model;
mod plan;
mod typed;
mod validate;
mod view;
mod wire;

#[cfg(test)]
mod tests;

pub(crate) use encode::{core_semantic_image_len, encode_core_semantic_image};
pub use fault::{
    CoreAuthorityFault, CoreAuthorityPlane, CoreProvenanceFault,
    CoreProvenanceIdentityField, CoreSemanticImageFault, CoreSemanticImageField,
    ScopeComponent,
};
pub use full::{
    ExtensionPlanFault, FullEntityFault, FullPlanError, GraphPlanFault,
    TerminalPoolDomain, TerminalPoolFault,
};
pub use full_wire::{
    encode_full_semantic_image, full_semantic_image_len, FullSemanticImageError,
    FullSemanticImageFault, FullSemanticImageField, FullSemanticImageIdentityField,
    SemanticImageView,
};
pub use wire::DirectoryKind;
/// Exact planning failures from [`encode_full_semantic_image`].
pub type SemanticImageEncodeError = FullPlanError;
/// Exact reopening failures from [`SemanticImageView::reopen`].
pub type SemanticImageReopenError = FullSemanticImageError;
/// Identity of the canonical complete portable semantic-image bytes.
pub type SemanticImageIdentity = heart_identity::ArtifactId<
    heart_identity::IrSemanticImageEncoding,
    heart_identity::IrSemanticImageDomain,
>;
pub(crate) use validate::reopen_core_semantic_image;
