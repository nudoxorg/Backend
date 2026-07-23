//! The `nudox.body.v1` serialization envelope for a [`crate::body::BodyEmbed`],
//! plus the companion-file path helper.
//!
//! # Why a separate channel, not new F1 keys
//!
//! The declaration file (`{intro_hex}.nir`, NdIrF1) is a strict, frozen
//! key-registry format: its parser rejects unknown keys and enforces key order,
//! and its bytes gate `api_surface_hash` / `embed_hash`. Bolting body facts onto
//! it would break the strict parser *and* move the declaration hash on every
//! body edit — exactly what §6 of the IR-unification spec forbids.
//!
//! Instead the body is a **companion channel sharing the entry's IntroId**:
//! `{intro_hex}.nb`. A body edit touches only the `.nb` bytes; the `.nir` stays
//! byte-identical, so semver / embed / graph rewrites are skipped by
//! construction. This is *not* a sibling store — it is the implementation half
//! of the same entry, one IntroId, versioned in the same channel.
//!
//! # Envelope
//!
//! ```text
//! b"nudox.body.v1\0" || postcard(BodyEmbed)
//! ```
//!
//! The domain prefix separates body bytes from any other postcard payload and
//! carries the format version. Readers support the current version and reject
//! unknown ones; new versions never mutate old bytes (ID-22 / format_version
//! discipline).

use crate::change::IntroId;

use crate::body::BodyEmbed;

/// The domain tag + format version prefixed to every serialized body payload.
///
/// The trailing NUL terminates the version token unambiguously.
pub const BODY_DOMAIN_V1: &[u8] = b"nudox.body.v1\0";

/// The companion body file extension. The declaration file is `{intro}.nir`;
/// the body companion sharing its IntroId is `{intro}.nb`.
pub const BODY_FILE_EXTENSION: &str = "nb";

/// Errors from body (de)serialization.
#[derive(Debug, thiserror::Error)]
pub enum BodyWireError {
    /// The payload did not begin with [`BODY_DOMAIN_V1`].
    #[error("body payload has wrong or missing domain prefix (expected nudox.body.v1)")]
    BadDomain,
    /// The postcard body failed to decode.
    #[error("body postcard decode failed: {0}")]
    Decode(postcard::Error),
    /// The postcard body failed to encode.
    #[error("body postcard encode failed: {0}")]
    Encode(postcard::Error),
}

/// Serialize a [`BodyEmbed`] into `nudox.body.v1` envelope bytes.
///
/// Deterministic: same input → same bytes (postcard is canonical for a fixed
/// type layout, and the body fact vectors are stored in producer order).
pub fn serialize_body(body: &BodyEmbed) -> Result<Vec<u8>, BodyWireError> {
    let payload = postcard::to_allocvec(body).map_err(BodyWireError::Encode)?;
    let mut out = Vec::with_capacity(BODY_DOMAIN_V1.len() + payload.len());
    out.extend_from_slice(BODY_DOMAIN_V1);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Parse `nudox.body.v1` envelope bytes back into a [`BodyEmbed`].
///
/// Rejects any payload not carrying the exact domain prefix.
pub fn deserialize_body(bytes: &[u8]) -> Result<BodyEmbed, BodyWireError> {
    let rest = bytes
        .strip_prefix(BODY_DOMAIN_V1)
        .ok_or(BodyWireError::BadDomain)?;
    postcard::from_bytes(rest).map_err(BodyWireError::Decode)
}

/// The working-copy path of the companion body file for an entry.
///
/// Mirrors the declaration-file convention (`symbol_path` in `nudox-ir-vcs`
/// produces `{intro_hex}.nir`); the body companion is `{intro_hex}.nb`, sharing
/// the IntroId. Kept here as a pure helper so a later wave can wire it into the
/// recording session without this crate depending on the VCS.
pub fn body_path(intro: IntroId) -> String {
    format!("{}.{}", intro.to_hex(), BODY_FILE_EXTENSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{BodyCall, BodyMergeNote, OracleBody, TreesitterBody};
    use crate::vocab::RelSpan;
    use heart::Language;

    #[test]
    fn absent_round_trips() {
        let bytes = serialize_body(&BodyEmbed::Absent).expect("serialize");
        assert!(bytes.starts_with(BODY_DOMAIN_V1));
        let back = deserialize_body(&bytes).expect("deserialize");
        assert_eq!(back, BodyEmbed::Absent);
    }

    #[test]
    fn present_round_trips_deterministically() {
        let body = crate::body::merge_body(
            Language::Rust,
            TreesitterBody {
                calls: vec![BodyCall {
                    name: "route".into(),
                    receiver: None,
                    rel_span: RelSpan::new(0, 5),
                }],
                ..Default::default()
            },
            OracleBody::default(),
            BodyMergeNote::treesitter_only(),
        );
        let a = serialize_body(&body).expect("serialize a");
        let b = serialize_body(&body).expect("serialize b");
        assert_eq!(a, b, "serialization must be deterministic");
        assert_eq!(deserialize_body(&a).expect("round trip"), body);
    }

    #[test]
    fn rejects_foreign_prefix() {
        let err = deserialize_body(b"not-a-body-payload").unwrap_err();
        assert!(matches!(err, BodyWireError::BadDomain));
    }

    #[test]
    fn body_path_shares_intro_hex_with_nb_extension() {
        let intro = IntroId::from_domain("test", b"x");
        let path = body_path(intro);
        assert!(path.ends_with(".nb"));
        assert!(path.starts_with(&intro.to_hex()));
    }
}
