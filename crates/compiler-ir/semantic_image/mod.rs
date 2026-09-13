//! Portable complete semantic-image grammar.
//!
//! The canonical remap and header/provenance decoder are shared preparation
//! machinery for the complete `NXFI` grammar below. There is deliberately no
//! second, partial reopen path: callers either receive the fully validated
//! [`SemanticImageView`] or an exact admission failure.

mod canonical;
mod decode;
mod fault;
mod full;
mod full_wire;
mod typed;

pub use fault::{
    CoreProvenanceFault, CoreProvenanceIdentityField, CoreSemanticImageFault,
    CoreSemanticImageField, ScopeComponent,
};
pub use full::{
    ExtensionPlanFault, FullEntityFault, FullPlanError, GraphPlanFault, TerminalPoolDomain,
    TerminalPoolFault,
};
pub use full_wire::{
    FullSemanticImageError, FullSemanticImageFault, FullSemanticImageField,
    FullSemanticImageIdentityField, SemanticImageView, encode_full_semantic_image,
    full_semantic_image_len,
};
/// Exact planning failures from [`encode_full_semantic_image`].
pub type SemanticImageEncodeError = FullPlanError;
/// Exact reopening failures from [`SemanticImageView::reopen`].
pub type SemanticImageReopenError = FullSemanticImageError;
/// Identity of the canonical complete portable semantic-image bytes.
pub type SemanticImageIdentity = backend_version::ArtifactId<
    backend_version::IrSemanticImageEncoding,
    backend_version::IrSemanticImageDomain,
>;
