//! # nudox-ir canonical codec
//!
//! Serialization and path-naming for the two IR planes:
//!
//! - **Declaration plane** (`{intro_hex}.nir`): an [`Entry`] serialized with a
//!   versioned envelope and postcard.
//! - **Body plane** (`{intro_hex}.nb`): a [`BodyEmbed`] serialized with its own
//!   versioned envelope and postcard.
//!
//! Both planes share an [`IntroId`] — that is the project's dual-plane design.
//! The [`Plane`] enum makes the pairing obvious and prevents confusing the two
//! file families.
//!
//! ## Envelope layout
//!
//! ```text
//! +----------+--------+--------------+
//! | magic    | ver    | postcard body|
//! | 12 bytes | 2 bytes| variable     |
//! +----------+--------+--------------+
//! ```
//!
//! - **magic** (12 bytes): `b"nudox.ir.v\0\0"` for the declaration plane,
//!   `b"nudox.nb.v\0\0"` for the body plane.  The magic tag uniquely identifies
//!   the plane and provides a human-readable sanity check.
//! - **ver** (2 bytes, little-endian u16): [`FORMAT_VERSION`] from
//!   [`crate::change`].  Decoders reject any version they do not understand.
//! - **postcard body**: the postcard-encoded payload (`Entry` or `BodyEmbed`).
//!
//! ## Canonicity contract
//!
//! **postcard** encodes in a canonical, deterministic, endian-stable format:
//! fixed integer widths, length-prefixed sequences, no hash randomization.  The
//! same logical value will always produce the same bytes on every platform
//! provided that all of the value's fields are themselves stable.
//!
//! ### Canonicity risks (read carefully)
//!
//! 1. **`usize` in `Symbol::span` and `Type::Array { length }`**. Both fields
//!    use `usize`, which is 64-bit on the targets in play (x86_64, aarch64) but
//!    32-bit on 32-bit platforms.  postcard encodes `usize` as a
//!    variable-length integer whose max width varies with the platform word
//!    size, so bytes produced on a 64-bit host could differ from those on a
//!    32-bit host for values > `u32::MAX`.  **Decision**: this crate does not
//!    attempt to normalise `usize` to a fixed width — that is a producer
//!    contract: producers must not store span offsets or array lengths that
//!    exceed `u32::MAX`.  At the formats layer the values are still `usize` in
//!    the Rust types (touching them is out of scope for this PR; doing so would
//!    require changing `Symbol` and `Type`, which are shared across the
//!    codebase).  This is a **known latent risk**, recorded here rather than
//!    silently ignored.
//!
//! 2. **`HashMap` iteration order**. `PristineIntroTable` uses `HashMap`
//!    internally, but the codec encodes single `Entry` values, not the whole
//!    table, so HashMap order is not a concern here.
//!
//! 3. **`f64` NaN payloads**. No `f64` fields exist in `Entry` or `BodyEmbed`
//!    as of this writing.
//!
//! 4. **`PathBuf` in `Symbol::source`**. `PathBuf` serializes as an OS string.
//!    On Unix this is UTF-8; on Windows it is WTF-8 / UTF-16 dependent on
//!    `OsStr` serde.  Sources are always recorded as forward-slash paths by
//!    producers, so this is only a risk if a Windows producer passes a
//!    backslash path — that would produce different bytes.  Recorded as a
//!    **known latent risk** with the same fix strategy (normalize to a slash
//!    string in the producer) rather than silently ignored.

use std::fmt;

use crate::{
    body::BodyEmbed,
    change::{FORMAT_VERSION, IntroId},
    entry::Entry,
};

// ---------------------------------------------------------------------------
// Plane
// ---------------------------------------------------------------------------

/// The two halves of one IR entry, sharing an [`IntroId`].
///
/// Use [`Plane::extension`] to get the file extension for a given plane, and
/// [`Plane::magic`] to get the envelope magic bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Plane {
    /// The declaration plane: `{intro_hex}.nir`.
    Declaration,
    /// The implementation body plane: `{intro_hex}.nb`.
    Body,
}

impl Plane {
    /// The file extension for this plane (without the leading `.`).
    #[inline]
    pub fn extension(self) -> &'static str {
        match self {
            Plane::Declaration => "nir",
            Plane::Body => "nb",
        }
    }

    /// The 12-byte magic tag for this plane's envelope.
    ///
    /// Magic tags are exactly 12 bytes so the envelope header is a fixed
    /// `MAGIC_LEN + 2` = 14 bytes before the postcard payload begins.
    #[inline]
    pub fn magic(self) -> &'static [u8; MAGIC_LEN] {
        match self {
            Plane::Declaration => b"nudox.ir.v\0\0",
            Plane::Body => b"nudox.nb.v\0\0",
        }
    }
}

// ---------------------------------------------------------------------------
// Envelope constants
// ---------------------------------------------------------------------------

/// Byte length of the magic tag in every envelope.
pub const MAGIC_LEN: usize = 12;

/// Total byte length of the fixed envelope header (magic + 2-byte version).
pub const HEADER_LEN: usize = MAGIC_LEN + 2;

// ---------------------------------------------------------------------------
// CodecError
// ---------------------------------------------------------------------------

/// All errors that can arise from encoding or decoding an IR blob or path.
#[derive(Debug)]
pub enum CodecError {
    /// The blob did not start with the expected magic bytes.
    ///
    /// Contains the first [`MAGIC_LEN`] bytes actually seen (or fewer if the
    /// blob is shorter than that).
    BadMagic(Vec<u8>),

    /// The format version in the envelope header is not supported by this
    /// decoder.
    ///
    /// The first value is what we support; the second is what we saw.
    UnsupportedVersion { expected: u16, got: u16 },

    /// The postcard payload could not be encoded.
    ///
    /// If this fires post-seal it is a bug in the caller: an `EntryIndex` that
    /// is not marked for serialization was found inside an `Entry` or
    /// `BodyEmbed`.  That means the seal pass has a bug — `Local` refs should
    /// have been lowered to `Intro`/`Foreign` before encoding.
    PostcardEncode(postcard::Error),

    /// The postcard payload could not be decoded.
    PostcardDecode(postcard::Error),

    /// The path did not have the expected structure or extension for either
    /// IR plane.
    MalformedPath,

    /// The hex portion of a path could not be decoded as a 32-byte [`IntroId`].
    BadHex(String),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::BadMagic(got) => write!(
                f,
                "IR blob has wrong or missing magic (got first {} bytes: {:?})",
                got.len(),
                got
            ),
            CodecError::UnsupportedVersion { expected, got } => write!(
                f,
                "IR blob has unsupported format version: expected {}, got {}",
                expected, got
            ),
            CodecError::PostcardEncode(e) => write!(
                f,
                "postcard encode failed: {} \
                 (if this fires post-seal the seal pass has a bug — \
                 Local EntryIndex must be lowered before encoding)",
                e
            ),
            CodecError::PostcardDecode(e) => write!(f, "postcard decode failed: {}", e),
            CodecError::MalformedPath => {
                write!(f, "path does not match the expected IR filename pattern")
            }
            CodecError::BadHex(s) => write!(f, "could not parse '{}' as a 64-char hex IntroId", s),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CodecError::PostcardEncode(e) => Some(e),
            CodecError::PostcardDecode(e) => Some(e),
            CodecError::BadMagic(_)
            | CodecError::UnsupportedVersion { .. }
            | CodecError::MalformedPath
            | CodecError::BadHex(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Low-level envelope helpers
// ---------------------------------------------------------------------------

/// Write a versioned envelope into `out`, then append the postcard-encoded
/// `value`.
fn write_envelope<T: serde::Serialize>(
    out: &mut Vec<u8>,
    plane: Plane,
    value: &T,
) -> Result<(), CodecError> {
    let payload = postcard::to_allocvec(value).map_err(CodecError::PostcardEncode)?;
    out.reserve(HEADER_LEN + payload.len());
    out.extend_from_slice(plane.magic());
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(())
}

/// Parse a versioned envelope, returning the postcard payload bytes on success.
///
/// `plane` selects which magic tag to expect.  A mismatch — including swapping
/// the two planes — produces [`CodecError::BadMagic`].
fn read_envelope(bytes: &[u8], plane: Plane) -> Result<&[u8], CodecError> {
    // A blob shorter than HEADER_LEN cannot carry a valid envelope.
    if bytes.len() < HEADER_LEN {
        return Err(CodecError::BadMagic(
            bytes[..bytes.len().min(MAGIC_LEN)].to_vec(),
        ));
    }
    let (magic, rest) = bytes.split_at(MAGIC_LEN);
    if magic != plane.magic().as_slice() {
        return Err(CodecError::BadMagic(magic.to_vec()));
    }
    let (ver_bytes, payload) = rest.split_at(2);
    let ver = u16::from_le_bytes([ver_bytes[0], ver_bytes[1]]);
    if ver != FORMAT_VERSION {
        return Err(CodecError::UnsupportedVersion {
            expected: FORMAT_VERSION,
            got: ver,
        });
    }
    Ok(payload)
}

// ---------------------------------------------------------------------------
// Entry (declaration plane)
// ---------------------------------------------------------------------------

/// Encode a sealed [`Entry`] into declaration-plane envelope bytes.
///
/// The returned buffer is `nudox.ir.v\0\0` || `u16le(FORMAT_VERSION)` ||
/// `postcard(entry)`.
///
/// # Panics
///
/// Does not panic.  If a non-serialization-marked `EntryIndex` is found inside
/// the value, postcard will propagate a serde error which is returned as
/// [`CodecError::PostcardEncode`] — a clear signal that the seal pass has a
/// bug.
pub fn encode_entry(entry: &Entry) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    write_envelope(&mut out, Plane::Declaration, entry)?;
    Ok(out)
}

/// Decode declaration-plane envelope bytes back into an [`Entry`].
///
/// Verifies the magic tag and format version before decoding.
pub fn decode_entry(bytes: &[u8]) -> Result<Entry, CodecError> {
    let payload = read_envelope(bytes, Plane::Declaration)?;
    postcard::from_bytes(payload).map_err(CodecError::PostcardDecode)
}

// ---------------------------------------------------------------------------
// BodyEmbed (body plane)
// ---------------------------------------------------------------------------

/// Encode a [`BodyEmbed`] into body-plane envelope bytes.
///
/// The returned buffer is `nudox.nb.v\0\0` || `u16le(FORMAT_VERSION)` ||
/// `postcard(body)`.
pub fn encode_body(body: &BodyEmbed) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    write_envelope(&mut out, Plane::Body, body)?;
    Ok(out)
}

/// Decode body-plane envelope bytes back into a [`BodyEmbed`].
///
/// Verifies the magic tag and format version before decoding.
pub fn decode_body(bytes: &[u8]) -> Result<BodyEmbed, CodecError> {
    let payload = read_envelope(bytes, Plane::Body)?;
    postcard::from_bytes(payload).map_err(CodecError::PostcardDecode)
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Construct the working-tree filename for a given plane and [`IntroId`].
///
/// ```text
/// Declaration → "{intro_hex}.nir"
/// Body        → "{intro_hex}.nb"
/// ```
///
/// The intro hex is always exactly 64 lowercase hex characters, making the
/// extension parse unambiguous.
pub fn ir_path(intro: IntroId, plane: Plane) -> String {
    format!("{}.{}", intro.to_hex(), plane.extension())
}

/// True if `path` is a valid working-tree filename for `plane`.
///
/// Requires:
/// - No directory separator (`/`).
/// - Correct extension for `plane`.
/// - Exactly `HEX_LEN + 1 + ext.len()` bytes total.
pub fn is_ir_path(path: &str, plane: Plane) -> bool {
    let ext = plane.extension();
    if path.contains('/') {
        return false;
    }
    // Format: "{64 hex chars}.{ext}"
    let expected_len = HEX_LEN + 1 + ext.len();
    if path.len() != expected_len {
        return false;
    }
    // Check suffix without allocating: byte at HEX_LEN must be '.' and the
    // remainder must match the extension.
    let bytes = path.as_bytes();
    bytes[HEX_LEN] == b'.' && &bytes[HEX_LEN + 1..] == ext.as_bytes()
}

/// Extract the [`IntroId`] from a working-tree path, or return an error.
///
/// Accepts paths from either plane; the caller passes the expected `plane` so
/// that cross-plane confusion is caught immediately.
pub fn intro_id_of(path: &str, plane: Plane) -> Result<IntroId, CodecError> {
    if !is_ir_path(path, plane) {
        return Err(CodecError::MalformedPath);
    }
    let hex = &path[..HEX_LEN];
    hex_to_intro_id(hex)
}

/// The hex length of an `IntroId` (32 bytes × 2 hex chars each).
const HEX_LEN: usize = 64;

/// Parse exactly 64 lowercase hex chars into an [`IntroId`].
fn hex_to_intro_id(hex: &str) -> Result<IntroId, CodecError> {
    if hex.len() != HEX_LEN {
        return Err(CodecError::BadHex(hex.to_owned()));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0]).ok_or_else(|| CodecError::BadHex(hex.to_owned()))?;
        let lo = hex_nibble(chunk[1]).ok_or_else(|| CodecError::BadHex(hex.to_owned()))?;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(IntroId::from_raw(bytes))
}

/// Decode a single ASCII hex nibble.
#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        // Uppercase: accept for robustness in the parser, though IntroId::to_hex
        // always emits lowercase.
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        body::{
            BodyCall, BodyEmbed, Language, LocalBind, LocalKind,
            OracleBody, OracleCall, TreesitterBody,
        },
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        entry::{AttrTok, CfgExpr, Deprecation, DocLink, Entry, Node, Symbol, Visibility},
        index::Ref,
        kind::Kind,
        kinds::Module,
        vocab::{Confidence, ReferenceKind, RelSpan},
    };

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    fn make_stable_ref(name: &str) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo")),
            IntroId::from_domain("test.intro", name.as_bytes()),
        )
    }

    /// A minimal but non-trivial `Entry` whose all refs are `Intro` (post-seal
    /// state: no `Local` refs present).
    fn make_sealed_entry() -> Entry {
        let sym = Symbol {
            name: "my_module".to_owned(),
            visibility: Visibility::Public,
            documentation: "A test module.".to_owned(),
            source: PathBuf::from("src/lib.rs"),
            span: 0..42,
            aliases: Box::new(["alias_one".to_owned()]),
            deprecation: Some(Deprecation {
                note: Some("use new_module".to_owned()),
                since: Some("2.0.0".to_owned()),
            }),
            doc_links: Box::new([DocLink {
                target: "crate::new_module".to_owned(),
                label: Some("new_module".to_owned()),
            }]),
            attrs: Box::new([AttrTok {
                token: "must_use".to_owned(),
                arg: None,
            }]),
            cfg: Some(CfgExpr::Feature("alloc".to_owned())),
        };

        // Post-seal node: parent is an Intro ref, no children.
        let parent_ref: crate::index::RawRef = Ref::Intro(make_intro(0xaa));
        let node = Node::build(Some(parent_ref), std::iter::empty());

        Entry::new(sym, node, Kind::Module(Module))
    }

    fn make_body_embed() -> BodyEmbed {
        use crate::body::{BodyMergeNote, OracleTypeMention, merge_body};
        merge_body(
            Language::Rust,
            TreesitterBody {
                locals: vec![LocalBind {
                    name: "x".to_owned(),
                    kind: LocalKind::Let,
                    rel_span: RelSpan::new(0, 10),
                }],
                calls: vec![BodyCall {
                    name: "foo".to_owned(),
                    receiver: None,
                    rel_span: RelSpan::new(10, 20),
                }],
                control: vec![],
                ..Default::default()
            },
            OracleBody {
                calls: vec![OracleCall {
                    target: Some(make_stable_ref("foo")),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Oracle,
                    rel_span: RelSpan::new(10, 20),
                }],
                type_mentions: vec![OracleTypeMention {
                    ty: make_stable_ref("Bar"),
                    rel_span: RelSpan::new(5, 8),
                }],
                reads_writes: vec![],
            },
            BodyMergeNote::both_ran(),
        )
    }

    // ── 1. Entry round-trip ───────────────────────────────────────────────────

    #[test]
    fn entry_encode_decode_roundtrip() {
        let original = make_sealed_entry();
        let bytes = encode_entry(&original).expect("encode_entry must succeed");
        let recovered = decode_entry(&bytes).expect("decode_entry must succeed");
        assert_eq!(original, recovered, "Entry round-trip must be identity");
    }

    // ── 2. Canonicity: same value → same bytes ────────────────────────────────

    #[test]
    fn entry_encode_is_canonical() {
        let entry = make_sealed_entry();
        let bytes_a = encode_entry(&entry).expect("first encode");
        let bytes_b = encode_entry(&entry).expect("second encode");
        assert_eq!(
            bytes_a, bytes_b,
            "encoding the same Entry twice must produce byte-identical output"
        );
    }

    // ── 3a. Rejection: corrupted magic ────────────────────────────────────────

    #[test]
    fn decode_entry_rejects_bad_magic() {
        let mut bytes = encode_entry(&make_sealed_entry()).expect("encode");
        // Corrupt the first byte of the magic tag.
        bytes[0] = b'X';
        let err = decode_entry(&bytes).expect_err("must reject corrupted magic");
        assert!(
            matches!(err, CodecError::BadMagic(_)),
            "expected BadMagic, got: {:?}",
            err
        );
    }

    // ── 3b. Rejection: bumped version ─────────────────────────────────────────

    #[test]
    fn decode_entry_rejects_unsupported_version() {
        let mut bytes = encode_entry(&make_sealed_entry()).expect("encode");
        // The version is at bytes [MAGIC_LEN..MAGIC_LEN+2].
        // Write a version that is definitely not FORMAT_VERSION.
        let bad_ver: u16 = FORMAT_VERSION.wrapping_add(1);
        bytes[MAGIC_LEN] = bad_ver as u8;
        bytes[MAGIC_LEN + 1] = (bad_ver >> 8) as u8;
        let err = decode_entry(&bytes).expect_err("must reject bumped version");
        assert!(
            matches!(err, CodecError::UnsupportedVersion { .. }),
            "expected UnsupportedVersion, got: {:?}",
            err
        );
    }

    // ── 4. Path helpers round-trip ────────────────────────────────────────────

    #[test]
    fn path_roundtrip_declaration_plane() {
        let intro = make_intro(0xab);
        let path = ir_path(intro, Plane::Declaration);
        assert!(
            is_ir_path(&path, Plane::Declaration),
            "generated declaration path must be recognized"
        );
        let recovered =
            intro_id_of(&path, Plane::Declaration).expect("must parse declaration path");
        assert_eq!(
            intro, recovered,
            "IntroId must survive declaration path round-trip"
        );
    }

    #[test]
    fn path_roundtrip_body_plane() {
        let intro = make_intro(0xcd);
        let path = ir_path(intro, Plane::Body);
        assert!(
            is_ir_path(&path, Plane::Body),
            "generated body path must be recognized"
        );
        let recovered = intro_id_of(&path, Plane::Body).expect("must parse body path");
        assert_eq!(
            intro, recovered,
            "IntroId must survive body path round-trip"
        );
    }

    #[test]
    fn path_pairing_same_intro_id() {
        // Both planes constructed from the same IntroId share the same hex prefix.
        let intro = make_intro(0x42);
        let decl_path = ir_path(intro, Plane::Declaration);
        let body_path = ir_path(intro, Plane::Body);
        let hex = intro.to_hex();
        assert!(
            decl_path.starts_with(&hex),
            "declaration path must start with intro hex"
        );
        assert!(
            body_path.starts_with(&hex),
            "body path must start with intro hex"
        );
        assert_ne!(decl_path, body_path, "paths must differ by extension");
    }

    #[test]
    fn path_negative_cases() {
        // A subdirectory path is not an IR path.
        let intro = make_intro(0xff);
        let good = ir_path(intro, Plane::Declaration);
        assert!(!is_ir_path(
            &format!("symbols/{}", good),
            Plane::Declaration
        ));
        // Wrong extension.
        assert!(!is_ir_path(
            &format!("{}.txt", intro.to_hex()),
            Plane::Declaration
        ));
        // Too short (not a full hex).
        assert!(!is_ir_path("deadbeef.nir", Plane::Declaration));
        // Mixing planes: a .nb path is not a valid declaration path.
        let body = ir_path(intro, Plane::Body);
        assert!(!is_ir_path(&body, Plane::Declaration));
        // And vice versa.
        assert!(!is_ir_path(&good, Plane::Body));
        // intro_id_of returns MalformedPath for these.
        assert!(matches!(
            intro_id_of("deadbeef.nir", Plane::Declaration),
            Err(CodecError::MalformedPath)
        ));
    }

    // ── 5. BodyEmbed round-trip ───────────────────────────────────────────────

    #[test]
    fn body_embed_roundtrip() {
        let original = make_body_embed();
        let bytes = encode_body(&original).expect("encode_body must succeed");
        let recovered = decode_body(&bytes).expect("decode_body must succeed");
        assert_eq!(original, recovered, "BodyEmbed round-trip must be identity");
    }

    #[test]
    fn body_embed_absent_roundtrip() {
        let original = BodyEmbed::Absent;
        let bytes = encode_body(&original).expect("encode Absent");
        let recovered = decode_body(&bytes).expect("decode Absent");
        assert_eq!(original, recovered);
    }

    #[test]
    fn body_encode_is_canonical() {
        let body = make_body_embed();
        let a = encode_body(&body).expect("first encode");
        let b = encode_body(&body).expect("second encode");
        assert_eq!(
            a, b,
            "BodyEmbed encoding must be byte-identical across calls"
        );
    }

    #[test]
    fn decode_body_rejects_bad_magic() {
        let mut bytes = encode_body(&BodyEmbed::Absent).expect("encode");
        bytes[0] = b'X';
        let err = decode_body(&bytes).expect_err("must reject corrupted magic");
        assert!(matches!(err, CodecError::BadMagic(_)));
    }

    #[test]
    fn cross_plane_rejection() {
        // A declaration blob must not decode as a body blob, and vice versa.
        let decl_bytes = encode_entry(&make_sealed_entry()).expect("encode entry");
        let body_bytes = encode_body(&make_body_embed()).expect("encode body");
        assert!(
            matches!(decode_body(&decl_bytes), Err(CodecError::BadMagic(_))),
            "declaration bytes must be rejected by decode_body"
        );
        assert!(
            matches!(decode_entry(&body_bytes), Err(CodecError::BadMagic(_))),
            "body bytes must be rejected by decode_entry"
        );
    }

    // ── Envelope layout golden ────────────────────────────────────────────────

    #[test]
    fn envelope_header_layout() {
        // Verify the header is exactly HEADER_LEN bytes and carries the right magic.
        let bytes = encode_entry(&make_sealed_entry()).expect("encode");
        assert!(
            bytes.len() >= HEADER_LEN,
            "encoded blob must be at least HEADER_LEN bytes"
        );
        assert_eq!(
            &bytes[..MAGIC_LEN],
            Plane::Declaration.magic().as_slice(),
            "first MAGIC_LEN bytes must be the declaration magic"
        );
        let ver = u16::from_le_bytes([bytes[MAGIC_LEN], bytes[MAGIC_LEN + 1]]);
        assert_eq!(
            ver, FORMAT_VERSION,
            "version field must equal FORMAT_VERSION"
        );
    }
}
