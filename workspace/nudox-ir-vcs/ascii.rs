//! Shared ASCII encoding/decoding helpers for `typeref` and `typeexpr` values.
//!
//! Both `blob.rs` (NdIrSym debug export) and `f1.rs` (NdIrF1 canonical store)
//! use these encodings.  They are **frozen** — wire-v2 contract, §6.2 note.
//!
//! `typeref`: `S:<64hex>` (Same) | `F:<eco>/<pkg>#<64hex>` (Foreign)
//! `typeexpr`: `self|never|any|prim:…|tuple:…|slice:…|array:…|union:…|intersection:…`

use nudox_change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
use nudox_ir::wire::{PrimitiveWire, TypeRefWire, TypeWire, WidthWire};

// ---------------------------------------------------------------------------
// Escaping (§6.3, frozen superset of blob.rs original)
// ---------------------------------------------------------------------------

/// Encode a string value for use inside a `\t`-delimited F1 frame field.
///
/// `\` → `\\`, TAB → `\t`, LF → `\n`, CR → `\r`,
/// other C0 (0x00–0x1F except those four) → `\xNN` (lowercase hex).
pub fn escape(s: &str) -> String {
    // Operate on bytes so multi-byte UTF-8 (e.g. 日本語) passes through unaltered;
    // every escape we emit is pure ASCII, so the result is always valid UTF-8.
    let mut out: Vec<u8> = Vec::with_capacity(s.len() + 2);
    for &b in s.as_bytes() {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x00..=0x1F => {
                // Other C0 controls → \xNN
                out.push(b'\\');
                out.push(b'x');
                out.push(hex_nibble(b >> 4));
                out.push(hex_nibble(b & 0xF));
            }
            // >= 0x20: ASCII graphic or a UTF-8 lead/continuation byte — pass through.
            _ => out.push(b),
        }
    }
    String::from_utf8(out).expect("escape emits only ASCII escapes over valid UTF-8")
}

fn hex_nibble(n: u8) -> u8 {
    match n {
        0..=9 => b'0' + n,
        10..=15 => b'a' + n - 10,
        _ => unreachable!(),
    }
}

/// Decode an escape sequence string. Rejects unknown escapes and trailing `\`.
pub fn unescape(s: &str) -> Result<String, AsciiError> {
    // Build bytes so multi-byte UTF-8 pass-through is preserved, then validate.
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i >= bytes.len() {
                return Err(AsciiError::TrailingBackslash);
            }
            match bytes[i] {
                b'\\' => out.push(b'\\'),
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b'x' => {
                    // \xNN — exactly two lowercase hex digits
                    if i + 2 >= bytes.len() {
                        return Err(AsciiError::BadEscape("\\x at end of string".to_owned()));
                    }
                    let hi = hex_val(bytes[i + 1]).ok_or_else(|| {
                        AsciiError::BadEscape(format!("bad \\x hex digit: {}", bytes[i + 1] as char))
                    })?;
                    let lo = hex_val(bytes[i + 2]).ok_or_else(|| {
                        AsciiError::BadEscape(format!("bad \\x hex digit: {}", bytes[i + 2] as char))
                    })?;
                    out.push(hi << 4 | lo);
                    i += 2; // extra advance for the two hex digits
                }
                other => {
                    return Err(AsciiError::BadEscape(format!(
                        "unknown escape \\{}",
                        other as char
                    )));
                }
            }
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|e| AsciiError::BadEscape(format!("invalid UTF-8: {e}")))
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        // Uppercase rejected: only lowercase hex emitted by escape(), so
        // incoming uppercase means it was not produced by us → bad escape.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

pub fn hex_to_32(s: &str) -> Result<[u8; 32], AsciiError> {
    if s.len() != 64 {
        return Err(AsciiError::HexLen(s.len()));
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_val(b[i * 2]).ok_or(AsciiError::HexDigit(b[i * 2]))?;
        let lo = hex_val(b[i * 2 + 1]).ok_or(AsciiError::HexDigit(b[i * 2 + 1]))?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// TypeRef encoding / decoding (frozen, §6.2 wire contract)
// ---------------------------------------------------------------------------

pub fn encode_typeref(tr: &TypeRefWire) -> String {
    match tr {
        TypeRefWire::Same(id) => format!("S:{}", id.to_hex()),
        TypeRefWire::Foreign(sr) => format!(
            "F:{}/{}#{}",
            sr.package.ecosystem.as_str(),
            sr.package.name.as_str(),
            sr.intro.to_hex()
        ),
    }
}

pub fn decode_typeref(s: &str) -> Result<TypeRefWire, AsciiError> {
    if let Some(rest) = s.strip_prefix("S:") {
        Ok(TypeRefWire::Same(IntroId::from_raw(hex_to_32(rest)?)))
    } else if let Some(rest) = s.strip_prefix("F:") {
        let hash_pos = rest
            .rfind('#')
            .ok_or_else(|| AsciiError::Malformed(format!("no '#' in typeref: {}", s)))?;
        let intro_hex = &rest[hash_pos + 1..];
        let eco_pkg = &rest[..hash_pos];
        let slash_pos = eco_pkg
            .find('/')
            .ok_or_else(|| AsciiError::Malformed(format!("no '/' in typeref: {}", s)))?;
        let eco = &eco_pkg[..slash_pos];
        let pkg = &eco_pkg[slash_pos + 1..];
        Ok(TypeRefWire::Foreign(StableRef::new(
            PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
            IntroId::from_raw(hex_to_32(intro_hex)?),
        )))
    } else {
        Err(AsciiError::Malformed(format!("unknown typeref prefix: {}", s)))
    }
}

// ---------------------------------------------------------------------------
// TypeExpr encoding / decoding (frozen)
// ---------------------------------------------------------------------------

pub fn encode_width(w: &WidthWire) -> String {
    match w {
        WidthWire::Arch => "arch".to_owned(),
        WidthWire::Fixed(n) => n.to_string(),
    }
}

pub fn decode_width(s: &str) -> Result<WidthWire, AsciiError> {
    if s == "arch" {
        Ok(WidthWire::Arch)
    } else {
        let n: u32 = s
            .parse()
            .map_err(|_| AsciiError::Malformed(format!("invalid width: {}", s)))?;
        Ok(WidthWire::Fixed(n))
    }
}

pub fn encode_typeexpr(tw: &TypeWire) -> String {
    match tw {
        TypeWire::SelfType => "self".to_owned(),
        TypeWire::Never => "never".to_owned(),
        TypeWire::Any => "any".to_owned(),
        TypeWire::Primitive(p) => match p {
            PrimitiveWire::Bool => "prim:bool".to_owned(),
            PrimitiveWire::Char => "prim:char".to_owned(),
            PrimitiveWire::Str => "prim:str".to_owned(),
            PrimitiveWire::Integer { signed, width } => format!(
                "prim:int:{}:{}",
                if *signed { "s" } else { "u" },
                encode_width(width)
            ),
            PrimitiveWire::Float(w) => format!("prim:float:{}", encode_width(w)),
            PrimitiveWire::MutPointer(tr) => format!("prim:mutptr:{}", encode_typeref(tr)),
            PrimitiveWire::ConstPointer(tr) => format!("prim:constptr:{}", encode_typeref(tr)),
            PrimitiveWire::Reference { mutable, ty, .. } => format!(
                "prim:ref:{}:{}",
                if *mutable { "mut" } else { "shared" },
                encode_typeref(ty)
            ),
            PrimitiveWire::Builtin(name) => format!("prim:builtin:{}", escape(name)),
        },
        TypeWire::Tuple(refs) => {
            let parts: Vec<_> = refs.iter().map(encode_typeref).collect();
            format!("tuple:{}", parts.join(","))
        }
        TypeWire::Slice(tr) => format!("slice:{}", encode_typeref(tr)),
        TypeWire::Array { ty, length } => format!("array:{}:{}", encode_typeref(ty), length),
        TypeWire::Union(refs) => {
            let parts: Vec<_> = refs.iter().map(encode_typeref).collect();
            format!("union:{}", parts.join(","))
        }
        TypeWire::Intersection(refs) => {
            let parts: Vec<_> = refs.iter().map(encode_typeref).collect();
            format!("intersection:{}", parts.join(","))
        }
    }
}

pub fn decode_typeexpr(s: &str) -> Result<TypeWire, AsciiError> {
    match s {
        "self" => return Ok(TypeWire::SelfType),
        "never" => return Ok(TypeWire::Never),
        "any" => return Ok(TypeWire::Any),
        _ => {}
    }
    if let Some(rest) = s.strip_prefix("prim:") {
        if rest == "bool" {
            return Ok(TypeWire::Primitive(PrimitiveWire::Bool));
        }
        if rest == "char" {
            return Ok(TypeWire::Primitive(PrimitiveWire::Char));
        }
        if rest == "str" {
            return Ok(TypeWire::Primitive(PrimitiveWire::Str));
        }
        if let Some(r) = rest.strip_prefix("int:") {
            let colon =
                r.find(':').ok_or_else(|| AsciiError::Malformed(format!("bad prim:int: {}", s)))?;
            let signed = match &r[..colon] {
                "s" => true,
                "u" => false,
                other => {
                    return Err(AsciiError::Malformed(format!("bad int sign '{}': {}", other, s)))
                }
            };
            return Ok(TypeWire::Primitive(PrimitiveWire::Integer {
                signed,
                width: decode_width(&r[colon + 1..])?,
            }));
        }
        if let Some(r) = rest.strip_prefix("float:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::Float(decode_width(r)?)));
        }
        if let Some(r) = rest.strip_prefix("mutptr:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::MutPointer(Box::new(decode_typeref(r)?))));
        }
        if let Some(r) = rest.strip_prefix("constptr:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::ConstPointer(Box::new(
                decode_typeref(r)?,
            ))));
        }
        if let Some(r) = rest.strip_prefix("ref:") {
            let colon =
                r.find(':').ok_or_else(|| AsciiError::Malformed(format!("bad prim:ref: {}", s)))?;
            let mutable = match &r[..colon] {
                "mut" => true,
                "shared" => false,
                other => {
                    return Err(AsciiError::Malformed(format!(
                        "bad ref mutability '{}': {}",
                        other, s
                    )))
                }
            };
            return Ok(TypeWire::Primitive(PrimitiveWire::Reference {
                lifetime: None,
                mutable,
                ty: Box::new(decode_typeref(&r[colon + 1..])?),
            }));
        }
        if let Some(r) = rest.strip_prefix("builtin:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::Builtin(unescape(r)?)));
        }
        return Err(AsciiError::Malformed(format!("unknown prim: {}", s)));
    }
    if let Some(r) = s.strip_prefix("tuple:") {
        let parts = split_comma(r);
        let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
        return Ok(TypeWire::Tuple(refs?.into_boxed_slice()));
    }
    if let Some(r) = s.strip_prefix("slice:") {
        return Ok(TypeWire::Slice(Box::new(decode_typeref(r)?)));
    }
    if let Some(r) = s.strip_prefix("array:") {
        let colon = r
            .rfind(':')
            .ok_or_else(|| AsciiError::Malformed(format!("bad array: {}", s)))?;
        let length: u64 = r[colon + 1..]
            .parse()
            .map_err(|_| AsciiError::Malformed(format!("bad array length in: {}", s)))?;
        return Ok(TypeWire::Array { ty: Box::new(decode_typeref(&r[..colon])?), length });
    }
    if let Some(r) = s.strip_prefix("union:") {
        let parts = split_comma(r);
        let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
        return Ok(TypeWire::Union(refs?.into_boxed_slice()));
    }
    if let Some(r) = s.strip_prefix("intersection:") {
        let parts = split_comma(r);
        let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
        return Ok(TypeWire::Intersection(refs?.into_boxed_slice()));
    }
    Err(AsciiError::Malformed(format!("unknown typeexpr: {}", s)))
}

/// Split a comma-separated list of typerefs (typerefs don't contain commas).
pub fn split_comma(s: &str) -> Vec<&str> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(',').collect()
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, thiserror::Error)]
pub enum AsciiError {
    #[error("trailing backslash in escape sequence")]
    TrailingBackslash,
    #[error("bad escape: {0}")]
    BadEscape(String),
    #[error("hex must be 64 chars, got {0}")]
    HexLen(usize),
    #[error("invalid hex digit: 0x{0:02x}")]
    HexDigit(u8),
    #[error("malformed encoding: {0}")]
    Malformed(String),
    #[error("integer parse error: {0}")]
    IntParse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_round_trips() {
        for input in &[
            "hello world",
            "has\ttab",
            "has\nnewline",
            "has\rcarriage",
            "has\\backslash",
            "mixed\t\n\r\\end",
            "\x00\x01\x1Fcontrol",
            "日本語 UTF-8",
        ] {
            let encoded = escape(input);
            // Encoded must not contain control bytes other than (none — we strip them all)
            for b in encoded.bytes() {
                assert!(b >= 0x20 || b == b'\\', "encoded should be printable: 0x{b:02x}");
            }
            let decoded = unescape(&encoded).expect("should decode");
            assert_eq!(&decoded, input, "round-trip failed for: {:?}", input);
        }
    }

    #[test]
    fn unescape_rejects_bad() {
        assert!(unescape("\\").is_err(), "trailing backslash");
        assert!(unescape("\\q").is_err(), "unknown escape");
        assert!(unescape("\\xGG").is_err(), "bad hex in \\x");
        assert!(unescape("\\x1").is_err(), "truncated \\x");
    }

    #[test]
    fn typeref_round_trips() {
        let cases = vec![
            TypeRefWire::Same(IntroId::from_raw([0xAB; 32])),
            TypeRefWire::Foreign(StableRef::new(
                PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("serde")),
                IntroId::from_raw([0x12; 32]),
            )),
        ];
        for tr in &cases {
            let enc = encode_typeref(tr);
            let dec = decode_typeref(&enc).expect("decode_typeref");
            assert_eq!(&dec, tr);
        }
    }

    #[test]
    fn typeexpr_round_trips() {
        let cases = vec![
            TypeWire::SelfType,
            TypeWire::Never,
            TypeWire::Any,
            TypeWire::Primitive(PrimitiveWire::Bool),
            TypeWire::Primitive(PrimitiveWire::Str),
            TypeWire::Primitive(PrimitiveWire::Integer { signed: true, width: WidthWire::Fixed(64) }),
            TypeWire::Primitive(PrimitiveWire::Float(WidthWire::Arch)),
            TypeWire::Tuple(Box::new([
                TypeRefWire::Same(IntroId::from_raw([1; 32])),
                TypeRefWire::Same(IntroId::from_raw([2; 32])),
            ])),
            TypeWire::Slice(Box::new(TypeRefWire::Same(IntroId::from_raw([3; 32])))),
            TypeWire::Array {
                ty: Box::new(TypeRefWire::Same(IntroId::from_raw([4; 32]))),
                length: 42,
            },
        ];
        for tw in &cases {
            let enc = encode_typeexpr(tw);
            let dec = decode_typeexpr(&enc).expect("decode_typeexpr");
            assert_eq!(&dec, tw, "round-trip failed for typeexpr: {}", enc);
        }
    }
}
