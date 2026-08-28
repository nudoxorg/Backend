//! Shared ASCII encoding/decoding helpers for `typeref` and `typeexpr` values.
//!
//! Moved from workspace/ir. Frozen wire-v2 contract (§6.2 note).

use crate::wire::{PrimitiveWire, TypeRefWire, TypeWire, WidthWire};
use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
use ir::foreign::{ForeignKey, ForeignOrigin};
use ir::kind::KindDiscriminant;

// ---------------------------------------------------------------------------
// Escaping (§6.3)
// ---------------------------------------------------------------------------

pub fn escape(s: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(s.len() + 2);
    for &b in s.as_bytes() {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x00..=0x1F => {
                out.push(b'\\');
                out.push(b'x');
                out.push(hex_nibble(b >> 4));
                out.push(hex_nibble(b & 0xF));
            }
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

pub fn unescape(s: &str) -> Result<String, Error> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i >= bytes.len() {
                return Err(Error::TrailingBackslash);
            }
            match bytes[i] {
                b'\\' => out.push(b'\\'),
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b'x' => {
                    if i + 2 >= bytes.len() {
                        return Err(Error::BadEscape("\\x at end of string".to_owned()));
                    }
                    let hi = hex_val(bytes[i + 1]).ok_or_else(|| {
                        Error::BadEscape(format!("bad \\x hex digit: {}", bytes[i + 1] as char))
                    })?;
                    let lo = hex_val(bytes[i + 2]).ok_or_else(|| {
                        Error::BadEscape(format!("bad \\x hex digit: {}", bytes[i + 2] as char))
                    })?;
                    out.push(hi << 4 | lo);
                    i += 2;
                }
                other => {
                    return Err(Error::BadEscape(format!(
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
    String::from_utf8(out).map_err(|e| Error::BadEscape(format!("invalid UTF-8: {e}")))
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

pub fn hex_to_32(s: &str) -> Result<[u8; 32], Error> {
    if s.len() != 64 {
        return Err(Error::HexLen(s.len()));
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_val(b[i * 2]).ok_or(Error::HexDigit(b[i * 2]))?;
        let lo = hex_val(b[i * 2 + 1]).ok_or(Error::HexDigit(b[i * 2 + 1]))?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// TypeRef encoding / decoding (frozen, §6.2 wire contract)
// ---------------------------------------------------------------------------

/// `origin`'s ASCII encoding for [`encode_typeref`]'s `U:` (`ForeignUnlinked`)
/// form. Own tag namespace from the `S:`/`F:`/`U:`/`E:` typeref tags — a `P:`/
/// `N:`/`V:` prefix picks the `ForeignOrigin` variant.
fn encode_foreign_origin(o: &ForeignOrigin) -> String {
    match o {
        ForeignOrigin::Package(l) => format!("P:{}/{}", l.ecosystem.as_str(), l.name.as_str()),
        ForeignOrigin::Namespace {
            ecosystem,
            namespace,
        } => format!("N:{}/{}", ecosystem.as_str(), escape(namespace)),
        ForeignOrigin::Universe { ecosystem } => format!("V:{}", ecosystem.as_str()),
    }
}

fn decode_foreign_origin(s: &str) -> Result<ForeignOrigin, Error> {
    if let Some(rest) = s.strip_prefix("P:") {
        let slash = rest
            .find('/')
            .ok_or_else(|| Error::Malformed(format!("no '/' in origin: {s}")))?;
        Ok(ForeignOrigin::Package(PackageLineageId::new(
            EcosystemId::new(&rest[..slash]),
            PackageName::new(&rest[slash + 1..]),
        )))
    } else if let Some(rest) = s.strip_prefix("N:") {
        let slash = rest
            .find('/')
            .ok_or_else(|| Error::Malformed(format!("no '/' in origin: {s}")))?;
        Ok(ForeignOrigin::Namespace {
            ecosystem: EcosystemId::new(&rest[..slash]),
            namespace: unescape(&rest[slash + 1..])?.into_boxed_str(),
        })
    } else if let Some(rest) = s.strip_prefix("V:") {
        Ok(ForeignOrigin::Universe {
            ecosystem: EcosystemId::new(rest),
        })
    } else {
        Err(Error::Malformed(format!("unknown origin prefix: {s}")))
    }
}

pub fn encode_typeref(tr: &TypeRefWire) -> String {
    match tr {
        TypeRefWire::Same(id) => format!("S:{}", id.to_hex()),
        TypeRefWire::Foreign(sr) => format!(
            "F:{}/{}#{}",
            sr.package.ecosystem.as_str(),
            sr.package.name.as_str(),
            sr.intro.to_hex()
        ),
        TypeRefWire::ForeignUnlinked(key) => format!(
            "U:{}|{}|{}|{}",
            encode_foreign_origin(&key.origin),
            escape(&key.path),
            escape(&key.display),
            key.kind.map(|k| k.as_u16().to_string()).unwrap_or_default()
        ),
        TypeRefWire::UnresolvedExternal(name) => format!("E:{}", escape(name)),
    }
}

pub fn decode_typeref(s: &str) -> Result<TypeRefWire, Error> {
    if let Some(rest) = s.strip_prefix("S:") {
        Ok(TypeRefWire::Same(IntroId::from_raw(hex_to_32(rest)?)))
    } else if let Some(rest) = s.strip_prefix("F:") {
        let hash_pos = rest
            .rfind('#')
            .ok_or_else(|| Error::Malformed(format!("no '#' in typeref: {s}")))?;
        let intro_hex = &rest[hash_pos + 1..];
        let eco_pkg = &rest[..hash_pos];
        let slash_pos = eco_pkg
            .find('/')
            .ok_or_else(|| Error::Malformed(format!("no '/' in typeref: {s}")))?;
        let eco = &eco_pkg[..slash_pos];
        let pkg = &eco_pkg[slash_pos + 1..];
        Ok(TypeRefWire::Foreign(StableRef::new(
            PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
            IntroId::from_raw(hex_to_32(intro_hex)?),
        )))
    } else if let Some(rest) = s.strip_prefix("U:") {
        let parts: Vec<&str> = rest.splitn(4, '|').collect();
        let [origin, path, display, kind] = parts.as_slice() else {
            return Err(Error::Malformed(format!(
                "expected 4 '|'-separated fields in unlinked typeref: {s}"
            )));
        };
        let kind = if kind.is_empty() {
            None
        } else {
            let raw: u16 = kind
                .parse()
                .map_err(|_| Error::Malformed(format!("bad kind discriminant: {s}")))?;
            Some(
                KindDiscriminant::from_u16(raw)
                    .ok_or_else(|| Error::Malformed(format!("unknown kind discriminant: {s}")))?,
            )
        };
        Ok(TypeRefWire::ForeignUnlinked(ForeignKey {
            origin: decode_foreign_origin(origin)?,
            path: unescape(path)?.into_boxed_str(),
            display: unescape(display)?.into_boxed_str(),
            kind,
        }))
    } else if let Some(rest) = s.strip_prefix("E:") {
        Ok(TypeRefWire::UnresolvedExternal(unescape(rest)?))
    } else {
        Err(Error::Malformed(format!("unknown typeref prefix: {s}")))
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

pub fn decode_width(s: &str) -> Result<WidthWire, Error> {
    if s == "arch" {
        Ok(WidthWire::Arch)
    } else {
        let n: u32 = s
            .parse()
            .map_err(|_| Error::Malformed(format!("invalid width: {s}")))?;
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
        TypeWire::UnresolvedExternal(name) => format!("unresolved-external:{}", escape(name)),
    }
}

pub fn decode_typeexpr(s: &str) -> Result<TypeWire, Error> {
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
            let colon = r
                .find(':')
                .ok_or_else(|| Error::Malformed(format!("bad prim:int: {s}")))?;
            let signed = match &r[..colon] {
                "s" => true,
                "u" => false,
                other => {
                    return Err(Error::Malformed(format!("bad int sign '{other}': {s}")));
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
            return Ok(TypeWire::Primitive(PrimitiveWire::MutPointer(Box::new(
                decode_typeref(r)?,
            ))));
        }
        if let Some(r) = rest.strip_prefix("constptr:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::ConstPointer(Box::new(
                decode_typeref(r)?,
            ))));
        }
        if let Some(r) = rest.strip_prefix("ref:") {
            let colon = r
                .find(':')
                .ok_or_else(|| Error::Malformed(format!("bad prim:ref: {s}")))?;
            let mutable = match &r[..colon] {
                "mut" => true,
                "shared" => false,
                other => {
                    return Err(Error::Malformed(format!(
                        "bad ref mutability '{other}': {s}"
                    )));
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
        return Err(Error::Malformed(format!("unknown prim: {s}")));
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
            .ok_or_else(|| Error::Malformed(format!("bad array: {s}")))?;
        let length: u64 = r[colon + 1..]
            .parse()
            .map_err(|_| Error::Malformed(format!("bad array length in: {s}")))?;
        return Ok(TypeWire::Array {
            ty: Box::new(decode_typeref(&r[..colon])?),
            length,
        });
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
    if let Some(r) = s.strip_prefix("unresolved-external:") {
        return Ok(TypeWire::UnresolvedExternal(unescape(r)?));
    }
    Err(Error::Malformed(format!("unknown typeexpr: {s}")))
}

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
pub enum Error {
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
