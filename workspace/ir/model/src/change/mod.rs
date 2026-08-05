//! # nudox-ir change — content-addressed IDENTITY vocabulary
//!
//! Domain-separated BLAKE3 identity primitives and canonical little-endian
//! encoding helpers. This is the subset of the `workspace/ir` change vocabulary
//! that nudox-ir needs for its own identity layer: [`ContentBlake3`],
//! [`IntroId`], [`StableRef`], [`PackageLineageId`], [`EcosystemId`], and
//! [`PackageName`].
//!
//! VCS-layer types (ChangeId, GenerationStamp, CasKey, ChangeSetFingerprint,
//! LinkDomainKey) are deferred — those are owned by the higher VCS/manifest
//! crates and are not needed here.

pub mod encode;
pub mod hash;
pub mod ids;

pub use hash::{ContentBlake3, IntroId};
pub use ids::{EcosystemId, PackageLineageId, PackageName, StableRef};

/// nudox-ir's own identity format version.
///
/// Intentionally v-next, NOT byte-compatible with workspace/ir's buggy v2
/// IntroId (the disambiguator fix changes the preimage). Consumers of
/// nudox-ir must use this constant, not the workspace/ir domain strings, when
/// computing IntroId preimages for symbols originating from this crate.
pub const FORMAT_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::encode::{encode_str, write_u16le};

    // ── Golden byte pins ──────────────────────────────────────────────────────
    //
    // These are computed by running the code and pasting the real output. They
    // freeze the preimage layout so any accidental change to the hashing logic
    // breaks the test immediately.

    #[test]
    fn content_blake3_from_domain_golden() {
        // blake3("nudox.intro.v2" || "example-preimage")
        // Preimage: domain bytes then payload bytes, no framing between them.
        let hex = ContentBlake3::from_domain("nudox.intro.v2", b"example-preimage").to_hex();
        assert_eq!(
            hex, "75aee5b7f758d9028ff141c1f027f9e847839933a7cbdf30b5e0a951e9ec7f32",
            "ContentBlake3::from_domain golden pin changed — preimage layout regression"
        );
    }

    #[test]
    fn intro_id_from_raw_golden() {
        let id = IntroId::from_raw([7u8; 32]);
        assert_eq!(
            id.to_hex(),
            "0707070707070707070707070707070707070707070707070707070707070707",
            "IntroId::from_raw should be a transparent wrapper — hex must equal input bytes"
        );
    }

    #[test]
    fn stable_ref_serde_round_trip() {
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo"));
        let intro = IntroId::from_raw([7u8; 32]);
        let sref = StableRef::new(lineage, intro);
        let json = serde_json::to_string(&sref).expect("serialize");
        let back: StableRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(sref, back, "StableRef serde round-trip must be identity");
    }

    #[test]
    fn encode_str_golden() {
        // encode_str("hi") = u32le(2) || b"hi" = [0x02, 0x00, 0x00, 0x00, 0x68, 0x69]
        let mut out = Vec::new();
        encode_str(&mut out, "hi");
        assert_eq!(
            out,
            &[0x02, 0x00, 0x00, 0x00, b'h', b'i'],
            "encode_str must be u32le(len) || utf8"
        );
    }

    #[test]
    fn write_u16le_golden() {
        // 0x0102 little-endian = [0x02, 0x01]
        let mut out = Vec::new();
        write_u16le(&mut out, 0x0102);
        assert_eq!(out, &[0x02, 0x01], "write_u16le must be little-endian");
    }
}
