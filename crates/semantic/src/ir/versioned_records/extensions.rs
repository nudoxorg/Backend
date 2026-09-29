//! Profile-specific language-extension SPIR row family.
#![deny(
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    unsafe_code
)]

mod catalog;
mod plan;
#[cfg(test)]
mod tests;
mod wire;

pub use catalog::{CheckedLanguageExtensionFamilyV2, validate_language_extension_family_v2};
pub use plan::{
    LanguageExtensionRows, encode_language_extension_plane,
    verify_language_extension_plane_against_reader,
};
pub(super) fn validate_record(
    kind: crate::ir::SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), crate::ir::SemanticPlaneRecordError> {
    wire::validate_record(kind, key, tag, payload)
}
