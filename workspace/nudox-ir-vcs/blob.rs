//! Line-oriented textual per-symbol blob format for `{intro_hex}.nir` files.
//!
//! # Format
//!
//! UTF-8 text, `\n`-terminated lines; fields within a line TAB-separated.
//! Values that may contain `\`, TAB, or newline are escaped: `\`→`\\`,
//! TAB→`\t`, newline→`\n`.
//!
//! ```text
//! NdIrSym\t1
//! name\t<name>
//! vis\t<Public|Private|Protected|Internal|Package|Crate>
//! kind\t<Module|Record|Field|Function|Type>
//! span\t<start>\t<end>
//! hash\t<64-hex>
//! [parent\t<64-hex>]          # only if parent present
//! [ref]                        # bare line; only if IS_REFERENCE flag set
//! [src\t<path>]                # only if non-empty
//! [doc\t<text>]                # only if Some
//! [alias\t<name>] ...          # 0+, sorted ascending
//! [deprecated\t<since>\t<note>]  # only if Some; empty string for None sub-field
//! [link\t<eco>\t<pkg>\t<64-hex>\t<kind_self>\t<kind_other>] ... # 0+, sorted
//! # ── kind body ──
//! # Module: no extra lines
//! # Record: [recfield\t<64-hex>] ...  in field order
//! # Field:  [fieldty\t<typeref>]      only if type present
//! # Function: [in\t<pname>\t<typeref>] ... [out\t<pname>\t<typeref>] ...
//! # Type:   type\t<typeexpr>
//! ```

use nudox_ir::change::{ContentBlake3, EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::symbol::Visibility;
use nudox_ir::wire::{
    DeprecationWire, DocLinkWire, EntryPayloadFlags, FieldWire, FunctionWire, KindWire, ModuleWire,
    OwnedEntryPayload, ParamWire, PrimitiveWire, RecordWire, SymbolWire, TypeRefWire, TypeWire,
    WidthWire,
};
use thiserror::Error;

use crate::serialize::LinkWire;

// ---------------------------------------------------------------------------
// BlobError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum BlobError {
    #[error("bad magic (expected NdIrSym header)")]
    BadMagic,

    #[error("unsupported version: {0}")]
    UnsupportedVersion(u16),

    #[error("truncated or malformed blob line: {0}")]
    MalformedLine(String),

    #[error("blob contains invalid UTF-8: {0}")]
    Utf8(#[from] core::str::Utf8Error),

    #[error("integer parse error: {0}")]
    IntParse(String),

    #[error("hex decode error: {0}")]
    HexParse(String),

    /// Wire-v2: kind discriminant not handled by the V1 debug-export path.
    #[error("unsupported kind for V1 blob format: {0:?}")]
    UnsupportedKind(KindDiscriminant),
}

// ---------------------------------------------------------------------------
// Escaping helpers
// ---------------------------------------------------------------------------

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> Result<String, BlobError> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(x) => {
                    return Err(BlobError::MalformedLine(format!("bad escape \\{}", x)));
                }
                None => {
                    return Err(BlobError::MalformedLine("trailing backslash".into()));
                }
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn hex_to_32(s: &str) -> Result<[u8; 32], BlobError> {
    if s.len() != 64 {
        return Err(BlobError::HexParse(format!("expected 64 hex chars, got {}", s.len())));
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        let hi = hex_digit(s.as_bytes()[i * 2])
            .ok_or_else(|| BlobError::HexParse(format!("invalid hex char at {}", i * 2)))?;
        let lo = hex_digit(s.as_bytes()[i * 2 + 1])
            .ok_or_else(|| BlobError::HexParse(format!("invalid hex char at {}", i * 2 + 1)))?;
        *b = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// TypeRef encoding / decoding
// ---------------------------------------------------------------------------

fn encode_typeref(tr: &TypeRefWire) -> String {
    match tr {
        TypeRefWire::Same(id) => format!("S:{}", id.to_hex()),
        TypeRefWire::Foreign(sr) => {
            format!(
                "F:{}/{}#{}",
                sr.package.ecosystem.as_str(),
                sr.package.name.as_str(),
                sr.intro.to_hex()
            )
        }
    }
}

fn decode_typeref(s: &str) -> Result<TypeRefWire, BlobError> {
    if let Some(rest) = s.strip_prefix("S:") {
        let bytes = hex_to_32(rest)?;
        Ok(TypeRefWire::Same(IntroId::from_raw(bytes)))
    } else if let Some(rest) = s.strip_prefix("F:") {
        // format: <eco>/<pkgname>#<64-hex>
        let hash_pos = rest
            .rfind('#')
            .ok_or_else(|| BlobError::MalformedLine(format!("no '#' in foreign typeref: {}", s)))?;
        let intro_hex = &rest[hash_pos + 1..];
        let eco_pkg = &rest[..hash_pos];
        let slash_pos = eco_pkg
            .find('/')
            .ok_or_else(|| BlobError::MalformedLine(format!("no '/' in foreign typeref: {}", s)))?;
        let eco = &eco_pkg[..slash_pos];
        let pkg = &eco_pkg[slash_pos + 1..];
        let intro_bytes = hex_to_32(intro_hex)?;
        Ok(TypeRefWire::Foreign(StableRef::new(
            PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
            IntroId::from_raw(intro_bytes),
        )))
    } else {
        Err(BlobError::MalformedLine(format!("unknown typeref prefix: {}", s)))
    }
}

// ---------------------------------------------------------------------------
// TypeExpr encoding / decoding (TypeWire → single-line string)
// ---------------------------------------------------------------------------

fn encode_width(w: &WidthWire) -> String {
    match w {
        WidthWire::Arch => "arch".to_owned(),
        WidthWire::Fixed(n) => n.to_string(),
    }
}

fn decode_width(s: &str) -> Result<WidthWire, BlobError> {
    if s == "arch" {
        Ok(WidthWire::Arch)
    } else {
        let n: u32 = s
            .parse()
            .map_err(|_| BlobError::IntParse(format!("invalid width: {}", s)))?;
        Ok(WidthWire::Fixed(n))
    }
}

fn encode_typeexpr(tw: &TypeWire) -> String {
    match tw {
        TypeWire::SelfType => "self".to_owned(),
        TypeWire::Never => "never".to_owned(),
        TypeWire::Any => "any".to_owned(),
        TypeWire::Primitive(p) => match p {
            PrimitiveWire::Bool => "prim:bool".to_owned(),
            PrimitiveWire::Char => "prim:char".to_owned(),
            PrimitiveWire::Str => "prim:str".to_owned(),
            PrimitiveWire::Integer { signed, width } => {
                format!(
                    "prim:int:{}:{}",
                    if *signed { "s" } else { "u" },
                    encode_width(width)
                )
            }
            PrimitiveWire::Float(width) => format!("prim:float:{}", encode_width(width)),
            PrimitiveWire::MutPointer(tr) => format!("prim:mutptr:{}", encode_typeref(tr)),
            PrimitiveWire::ConstPointer(tr) => format!("prim:constptr:{}", encode_typeref(tr)),
            PrimitiveWire::Reference { mutable, ty, .. } => {
                format!(
                    "prim:ref:{}:{}",
                    if *mutable { "mut" } else { "shared" },
                    encode_typeref(ty)
                )
            }
            PrimitiveWire::Builtin(name) => format!("prim:builtin:{}", escape(name)),
        },
        TypeWire::Tuple(refs) => {
            let parts: Vec<_> = refs.iter().map(encode_typeref).collect();
            format!("tuple:{}", parts.join(","))
        }
        TypeWire::Slice(tr) => format!("slice:{}", encode_typeref(tr)),
        TypeWire::Array { ty, length } => {
            format!("array:{}:{}", encode_typeref(ty), length)
        }
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

// Split a comma-separated list of typerefs, respecting that typerefs themselves
// don't contain commas (hex chars + ':', '/', '#' only).
fn split_typerefs(s: &str) -> Vec<&str> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(',').collect()
    }
}

fn decode_typeexpr(s: &str) -> Result<TypeWire, BlobError> {
    if s == "self" {
        return Ok(TypeWire::SelfType);
    }
    if s == "never" {
        return Ok(TypeWire::Never);
    }
    if s == "any" {
        return Ok(TypeWire::Any);
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
        if let Some(rest2) = rest.strip_prefix("int:") {
            // prim:int:<s|u>:<width>
            let colon = rest2
                .find(':')
                .ok_or_else(|| BlobError::MalformedLine(format!("bad prim:int: {}", s)))?;
            let sign = &rest2[..colon];
            let width_str = &rest2[colon + 1..];
            let signed = match sign {
                "s" => true,
                "u" => false,
                _ => {
                    return Err(BlobError::MalformedLine(format!(
                        "bad int sign '{}' in: {}",
                        sign, s
                    )));
                }
            };
            return Ok(TypeWire::Primitive(PrimitiveWire::Integer {
                signed,
                width: decode_width(width_str)?,
            }));
        }
        if let Some(rest2) = rest.strip_prefix("float:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::Float(decode_width(rest2)?)));
        }
        if let Some(rest2) = rest.strip_prefix("mutptr:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::MutPointer(Box::new(
                decode_typeref(rest2)?,
            ))));
        }
        if let Some(rest2) = rest.strip_prefix("constptr:") {
            return Ok(TypeWire::Primitive(PrimitiveWire::ConstPointer(Box::new(
                decode_typeref(rest2)?,
            ))));
        }
        if let Some(rest2) = rest.strip_prefix("ref:") {
            // prim:ref:<mut|shared>:<typeref>
            let colon = rest2
                .find(':')
                .ok_or_else(|| BlobError::MalformedLine(format!("bad prim:ref: {}", s)))?;
            let mutability = &rest2[..colon];
            let tr_str = &rest2[colon + 1..];
            let mutable = match mutability {
                "mut" => true,
                "shared" => false,
                _ => {
                    return Err(BlobError::MalformedLine(format!(
                        "bad ref mutability '{}' in: {}",
                        mutability, s
                    )));
                }
            };
            return Ok(TypeWire::Primitive(PrimitiveWire::Reference {
                lifetime: None,
                mutable,
                ty: Box::new(decode_typeref(tr_str)?),
            }));
        }
        if let Some(rest2) = rest.strip_prefix("builtin:") {
            let name = unescape(rest2)?;
            return Ok(TypeWire::Primitive(PrimitiveWire::Builtin(name)));
        }
        return Err(BlobError::MalformedLine(format!("unknown prim: {}", s)));
    }
    if let Some(rest) = s.strip_prefix("tuple:") {
        let parts = split_typerefs(rest);
        let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
        return Ok(TypeWire::Tuple(refs?.into_boxed_slice()));
    }
    if let Some(rest) = s.strip_prefix("slice:") {
        return Ok(TypeWire::Slice(Box::new(decode_typeref(rest)?)));
    }
    if let Some(rest) = s.strip_prefix("array:") {
        // array:<typeref>:<len>  — typeref ends just before the last ':'
        let colon = rest
            .rfind(':')
            .ok_or_else(|| BlobError::MalformedLine(format!("bad array: {}", s)))?;
        let tr_str = &rest[..colon];
        let len_str = &rest[colon + 1..];
        let length: u64 = len_str
            .parse()
            .map_err(|_| BlobError::IntParse(format!("bad array length: {}", len_str)))?;
        return Ok(TypeWire::Array { ty: Box::new(decode_typeref(tr_str)?), length });
    }
    if let Some(rest) = s.strip_prefix("union:") {
        let parts = split_typerefs(rest);
        let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
        return Ok(TypeWire::Union(refs?.into_boxed_slice()));
    }
    if let Some(rest) = s.strip_prefix("intersection:") {
        let parts = split_typerefs(rest);
        let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
        return Ok(TypeWire::Intersection(refs?.into_boxed_slice()));
    }
    Err(BlobError::MalformedLine(format!("unknown typeexpr: {}", s)))
}

// ---------------------------------------------------------------------------
// KindDiscriminant name ↔ string
// ---------------------------------------------------------------------------

fn kind_name(k: KindDiscriminant) -> Option<&'static str> {
    match k {
        KindDiscriminant::Module => Some("Module"),
        KindDiscriminant::Record => Some("Record"),
        KindDiscriminant::Field => Some("Field"),
        KindDiscriminant::Function => Some("Function"),
        KindDiscriminant::Type => Some("Type"),
        // Wire-v2 kinds not representable in V1 debug-export
        _ => None,
    }
}

fn kind_from_name(s: &str) -> Result<KindDiscriminant, BlobError> {
    match s {
        "Module" => Ok(KindDiscriminant::Module),
        "Record" => Ok(KindDiscriminant::Record),
        "Field" => Ok(KindDiscriminant::Field),
        "Function" => Ok(KindDiscriminant::Function),
        "Type" => Ok(KindDiscriminant::Type),
        _ => Err(BlobError::MalformedLine(format!("unknown kind: {}", s))),
    }
}

// ---------------------------------------------------------------------------
// Visibility name ↔ string
// ---------------------------------------------------------------------------

fn vis_name(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "Public",
        Visibility::Private => "Private",
        Visibility::Protected => "Protected",
        Visibility::Internal => "Internal",
        Visibility::Package => "Package",
        Visibility::Crate => "Crate",
    }
}

fn vis_from_name(s: &str) -> Result<Visibility, BlobError> {
    match s {
        "Public" => Ok(Visibility::Public),
        "Private" => Ok(Visibility::Private),
        "Protected" => Ok(Visibility::Protected),
        "Internal" => Ok(Visibility::Internal),
        "Package" => Ok(Visibility::Package),
        "Crate" => Ok(Visibility::Crate),
        _ => Err(BlobError::MalformedLine(format!("unknown visibility: {}", s))),
    }
}

// ---------------------------------------------------------------------------
// serialize_symbol_blob
// ---------------------------------------------------------------------------

/// Emit the V1 debug-export blob for a symbol (kinds 1–5 only).
///
/// For wire-v2 kinds (6–12) this returns `Err(BlobError::UnsupportedKind)`.
/// The canonical format for all 12 kinds is [`crate::f1::serialize_f1`].
pub fn serialize_symbol_blob(
    payload: &OwnedEntryPayload,
    parent: Option<IntroId>,
    links: &[LinkWire],
) -> Result<Vec<u8>, BlobError> {
    let kind_tok = kind_name(payload.kind_disc)
        .ok_or(BlobError::UnsupportedKind(payload.kind_disc))?;
    let sym = &payload.symbol;
    let mut out = String::new();

    // Header
    out.push_str("NdIrSym\t1\n");

    // name
    out.push_str("name\t");
    out.push_str(&escape(&sym.name));
    out.push('\n');

    // vis
    let vis = sym.visibility;
    out.push_str("vis\t");
    out.push_str(vis_name(vis));
    out.push('\n');

    // kind
    out.push_str("kind\t");
    out.push_str(kind_tok);
    out.push('\n');

    // span
    out.push_str(&format!("span\t{}\t{}\n", sym.span_start, sym.span_end));

    // hash
    out.push_str("hash\t");
    out.push_str(&payload.payload_hash.to_hex());
    out.push('\n');

    // parent (optional)
    if let Some(p) = parent {
        out.push_str("parent\t");
        out.push_str(&p.to_hex());
        out.push('\n');
    }

    // IS_REFERENCE bit was retired in wire-v2; omit the bare "ref" line.

    // src (optional, only if non-empty)
    if !sym.source_path.is_empty() {
        out.push_str("src\t");
        out.push_str(&escape(&sym.source_path));
        out.push('\n');
    }

    // doc (optional)
    if let Some(doc) = &sym.documentation {
        out.push_str("doc\t");
        out.push_str(&escape(doc));
        out.push('\n');
    }

    // aliases (sorted)
    let mut aliases = sym.aliases.clone();
    aliases.sort();
    for alias in &aliases {
        out.push_str("alias\t");
        out.push_str(&escape(alias));
        out.push('\n');
    }

    // doc_links (sorted): `doclink\t<eco>\t<pkg>\t<64-hex>\t<label>`
    let mut doclink_lines: Vec<String> = sym
        .doc_links
        .iter()
        .map(|dl| {
            format!(
                "doclink\t{}\t{}\t{}\t{}\n",
                dl.target.package.ecosystem.as_str(),
                dl.target.package.name.as_str(),
                dl.target.intro.to_hex(),
                escape(dl.label.as_deref().unwrap_or("")),
            )
        })
        .collect();
    doclink_lines.sort();
    for line in &doclink_lines {
        out.push_str(line);
    }

    // deprecated (optional)
    if let Some(dep) = &sym.deprecation {
        let since = dep.since.as_deref().unwrap_or("");
        let note = dep.note.as_deref().unwrap_or("");
        out.push_str(&format!("deprecated\t{}\t{}\n", escape(since), escape(note)));
    }

    // links (sorted by full tuple string for determinism; skip v2-only kind links)
    let mut link_lines: Vec<String> = links
        .iter()
        .filter_map(|l| {
            let ks = kind_name(l.kind_self)?;
            let ko = kind_name(l.kind_other)?;
            Some(format!(
                "link\t{}\t{}\t{}\t{}\t{}\n",
                l.other.package.ecosystem.as_str(),
                l.other.package.name.as_str(),
                l.other.intro.to_hex(),
                ks,
                ko
            ))
        })
        .collect();
    link_lines.sort();
    for line in &link_lines {
        out.push_str(line);
    }

    // kind body
    match &payload.kind {
        KindWire::Module(_) => {}
        KindWire::Record(r) => {
            for field_id in r.fields.iter() {
                out.push_str("recfield\t");
                out.push_str(&field_id.to_hex());
                out.push('\n');
            }
        }
        KindWire::Field(f) => {
            if let Some(ty) = &f.ty {
                out.push_str("fieldty\t");
                out.push_str(&encode_typeref(ty));
                out.push('\n');
            }
        }
        KindWire::Function(f) => {
            for param in f.input_params.iter() {
                let pname = param.name.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "in\t{}\t{}\n",
                    escape(pname),
                    encode_typeref(&param.ty)
                ));
            }
            for param in f.output_params.iter() {
                let pname = param.name.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "out\t{}\t{}\n",
                    escape(pname),
                    encode_typeref(&param.ty)
                ));
            }
        }
        KindWire::Type(ta) => {
            // wire-v2: TypeAliasWire.ty is the TypeWire
            out.push_str("type\t");
            out.push_str(&encode_typeexpr(&ta.ty));
            out.push('\n');
        }
        _ => {
            // wire-v2 kinds (6–12): caller should use serialize_f1 instead.
            // We already checked kind_name above, so this branch is unreachable,
            // but the exhaustive match requires it.
        }
    }

    Ok(out.into_bytes())
}

// ---------------------------------------------------------------------------
// LinkView
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkView<'a> {
    pub other_ecosystem: &'a str,
    pub other_name: &'a str,
    pub other_intro: IntroId,
    pub kind_self: KindDiscriminant,
    pub kind_other: KindDiscriminant,
}

impl<'a> LinkView<'a> {
    pub fn to_stable_ref(&self) -> StableRef {
        StableRef::new(
            PackageLineageId::new(
                EcosystemId::new(self.other_ecosystem),
                PackageName::new(self.other_name),
            ),
            self.other_intro,
        )
    }

    pub fn to_link_wire(&self) -> LinkWire {
        LinkWire {
            other: self.to_stable_ref(),
            kind_self: self.kind_self,
            kind_other: self.kind_other,
        }
    }
}

// ---------------------------------------------------------------------------
// SymbolView — the borrowed text view
// ---------------------------------------------------------------------------

/// Borrowed, line-scan view into a symbol blob byte slice.
pub struct SymbolView<'a> {
    // These fields are pointers into the original buffer — no content is cloned.
    name: &'a str,
    vis: Visibility,
    kind_disc: KindDiscriminant,
    span_start: u32,
    span_end: u32,
    payload_hash: ContentBlake3,
    flags: EntryPayloadFlags,
    parent: Option<IntroId>,
    // Borrowed str slices for multi-value fields parsed during the single scan.
    link_lines: Vec<&'a str>,
    // Everything from "src" onward, needed by to_owned_payload().
    src: &'a str,
    doc: Option<&'a str>,
    alias_lines: Vec<&'a str>,
    doclink_lines: Vec<&'a str>,
    deprecated_line: Option<&'a str>,
    // Kind-body lines, borrowed.
    kind_body_lines: Vec<&'a str>,
}

impl<'a> SymbolView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, BlobError> {
        let text = core::str::from_utf8(bytes)?;

        let mut lines = text.split('\n');

        // --- header line ---
        let first = lines.next().ok_or_else(|| BlobError::MalformedLine("empty".into()))?;
        let mut hdr = first.splitn(2, '\t');
        let magic = hdr.next().unwrap_or("");
        let ver_str = hdr.next().unwrap_or("");
        if magic != "NdIrSym" {
            return Err(BlobError::BadMagic);
        }
        let version: u16 = ver_str
            .parse()
            .map_err(|_| BlobError::MalformedLine(format!("bad version: {}", ver_str)))?;
        if version != 1 {
            return Err(BlobError::UnsupportedVersion(version));
        }

        // --- required fields (single scan, borrow slices) ---
        let mut name: Option<&'a str> = None;
        let mut vis: Option<Visibility> = None;
        let mut kind_disc: Option<KindDiscriminant> = None;
        let mut span_start: Option<u32> = None;
        let mut span_end: Option<u32> = None;
        let mut payload_hash: Option<ContentBlake3> = None;
        let flags = EntryPayloadFlags::default();
        let mut parent: Option<IntroId> = None;
        let mut src: &'a str = "";
        let mut doc: Option<&'a str> = None;
        let mut alias_lines: Vec<&'a str> = Vec::new();
        let mut doclink_lines: Vec<&'a str> = Vec::new();
        let mut deprecated_line: Option<&'a str> = None;
        let mut link_lines: Vec<&'a str> = Vec::new();
        let mut kind_body_lines: Vec<&'a str> = Vec::new();

        // State machine: metadata first, then kind body
        let mut in_kind_body = false;

        for raw_line in lines {
            // Skip trailing empty line after final '\n'
            if raw_line.is_empty() && !in_kind_body {
                continue;
            }

            if in_kind_body {
                if !raw_line.is_empty() {
                    kind_body_lines.push(raw_line);
                }
                continue;
            }

            // Parse metadata line by keyword
            if raw_line == "ref" {
                // IS_REFERENCE bit retired in wire-v2; silently ignore the bare "ref" line
                // so we can still read V1 blobs without error.
                continue;
            }

            let tab = raw_line.find('\t');

            if let Some(pos) = tab {
                let key = &raw_line[..pos];
                let rest = &raw_line[pos + 1..];

                match key {
                    "name" => {
                        name = Some(rest);
                    }
                    "vis" => {
                        vis = Some(vis_from_name(rest)?);
                    }
                    "kind" => {
                        kind_disc = Some(kind_from_name(rest)?);
                    }
                    "span" => {
                        let mut parts = rest.splitn(2, '\t');
                        let s_str = parts
                            .next()
                            .ok_or_else(|| BlobError::MalformedLine("span missing start".into()))?;
                        let e_str = parts
                            .next()
                            .ok_or_else(|| BlobError::MalformedLine("span missing end".into()))?;
                        span_start = Some(s_str.parse().map_err(|_| {
                            BlobError::IntParse(format!("bad span start: {}", s_str))
                        })?);
                        span_end = Some(e_str.parse().map_err(|_| {
                            BlobError::IntParse(format!("bad span end: {}", e_str))
                        })?);
                    }
                    "hash" => {
                        let bytes = hex_to_32(rest)?;
                        payload_hash = Some(ContentBlake3::from_raw(bytes));
                    }
                    "parent" => {
                        let bytes = hex_to_32(rest)?;
                        parent = Some(IntroId::from_raw(bytes));
                    }
                    "src" => {
                        src = rest;
                    }
                    "doc" => {
                        doc = Some(rest);
                    }
                    "alias" => {
                        alias_lines.push(rest);
                    }
                    "doclink" => {
                        doclink_lines.push(rest);
                    }
                    "deprecated" => {
                        deprecated_line = Some(rest);
                    }
                    "link" => {
                        link_lines.push(rest);
                    }
                    // Kind-body keys — transition into kind body mode
                    "recfield" | "fieldty" | "in" | "out" | "type" => {
                        in_kind_body = true;
                        kind_body_lines.push(raw_line);
                    }
                    _ => {
                        // Unknown key — ignore for forward compat
                    }
                }
            } else if raw_line.is_empty() {
                continue;
            }
            // bare "ref" handled above; any other bare line: ignore
        }

        let name = name.ok_or_else(|| BlobError::MalformedLine("missing name".into()))?;
        let vis = vis.ok_or_else(|| BlobError::MalformedLine("missing vis".into()))?;
        let kind_disc =
            kind_disc.ok_or_else(|| BlobError::MalformedLine("missing kind".into()))?;
        let span_start =
            span_start.ok_or_else(|| BlobError::MalformedLine("missing span".into()))?;
        let span_end =
            span_end.ok_or_else(|| BlobError::MalformedLine("missing span end".into()))?;
        let payload_hash =
            payload_hash.ok_or_else(|| BlobError::MalformedLine("missing hash".into()))?;

        Ok(SymbolView {
            name,
            vis,
            kind_disc,
            span_start,
            span_end,
            payload_hash,
            flags,
            parent,
            link_lines,
            src,
            doc,
            alias_lines,
            doclink_lines,
            deprecated_line,
            kind_body_lines,
        })
    }

    // -----------------------------------------------------------------------
    // Accessors — all borrow from the original buffer
    // -----------------------------------------------------------------------

    /// Raw (possibly escaped) name, borrowed. Identifiers never contain `\\`,
    /// TAB, or newline in practice, so the raw slice is normally the exact
    /// name; call [`SymbolView::unescape`] when that cannot be assumed.
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn visibility(&self) -> Visibility {
        self.vis
    }

    pub fn kind_disc(&self) -> KindDiscriminant {
        self.kind_disc
    }

    pub fn span(&self) -> (u32, u32) {
        (self.span_start, self.span_end)
    }

    pub fn payload_hash(&self) -> ContentBlake3 {
        self.payload_hash
    }

    pub fn flags(&self) -> EntryPayloadFlags {
        self.flags
    }

    pub fn parent(&self) -> Option<IntroId> {
        self.parent
    }

    pub fn links(&self) -> impl Iterator<Item = LinkView<'a>> + 'a {
        let link_lines = self.link_lines.clone();
        link_lines.into_iter().filter_map(|line| parse_link_line(line).ok())
    }

    /// Raw (possibly escaped) source path.
    pub fn source_path(&self) -> &'a str {
        self.src
    }

    /// Raw (possibly escaped) documentation, if present.
    pub fn documentation(&self) -> Option<&'a str> {
        self.doc
    }

    /// Raw (possibly escaped) alias names, borrowed.
    pub fn aliases(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.alias_lines.clone().into_iter()
    }

    /// The type-skeleton fingerprint of a `Type` entry (for the archive's
    /// TypeSkeletonIndex); `None` for other kinds. Reconstructs only the small
    /// one-level `TypeWire` (its children are leaf refs), not the whole payload.
    pub fn type_fingerprint(&self) -> Option<nudox_ir::index::TypeFingerprintId> {
        if self.kind_disc != KindDiscriminant::Type {
            return None;
        }
        let line = self.kind_body_lines.iter().find_map(|l| l.strip_prefix("type\t"))?;
        let tw = decode_typeexpr(line).ok()?;
        let mut skel = Vec::new();
        nudox_ir::skeleton::type_wire_skeleton(&tw, &mut skel);
        Some(nudox_ir::skeleton::type_fingerprint(&skel))
    }

    // -----------------------------------------------------------------------
    // Full reconstruction
    // -----------------------------------------------------------------------

    pub fn to_owned_payload(&self) -> Result<OwnedEntryPayload, BlobError> {
        // Unescape name
        let name = unescape(self.name)?;

        // src
        let source_path = unescape(self.src)?;

        // doc
        let documentation = self.doc.map(unescape).transpose()?;

        // aliases
        let mut aliases: Vec<String> = self
            .alias_lines
            .iter()
            .map(|s| unescape(s))
            .collect::<Result<_, _>>()?;
        aliases.sort(); // canonical order

        // deprecation
        let deprecation = if let Some(dep_rest) = self.deprecated_line {
            let mut parts = dep_rest.splitn(2, '\t');
            let since_raw = parts
                .next()
                .ok_or_else(|| BlobError::MalformedLine("deprecated missing since".into()))?;
            let note_raw = parts
                .next()
                .ok_or_else(|| BlobError::MalformedLine("deprecated missing note".into()))?;
            let since_s = unescape(since_raw)?;
            let note_s = unescape(note_raw)?;
            Some(DeprecationWire {
                since: if since_s.is_empty() { None } else { Some(since_s) },
                note: if note_s.is_empty() { None } else { Some(note_s) },
            })
        } else {
            None
        };

        // doc_links (parsed from the `doclink` lines; canonical sorted order to
        // match serialize).
        let mut doc_links: Vec<DocLinkWire> = self
            .doclink_lines
            .iter()
            .map(|s| parse_doclink_line(s))
            .collect::<Result<_, _>>()?;
        doc_links.sort_by(|a, b| {
            (a.target.package.ecosystem.as_str(), a.target.package.name.as_str(), a.target.intro.as_bytes())
                .cmp(&(b.target.package.ecosystem.as_str(), b.target.package.name.as_str(), b.target.intro.as_bytes()))
        });

        let sym = SymbolWire {
            name,
            visibility: self.vis,
            documentation,
            source_path,
            span_start: self.span_start,
            span_end: self.span_end,
            aliases,
            deprecation,
            doc_links,
            // wire-v2 fields: V1 debug-export has no attrs/cfg encoding
            attrs: vec![],
            cfg: None,
        };

        // Parse kind body
        let kind = parse_kind_body(self.kind_disc, &self.kind_body_lines)?;

        // IS_REFERENCE bit retired in wire-v2; reconstruct with default flags
        let flags = EntryPayloadFlags::default();

        Ok(OwnedEntryPayload {
            symbol: sym,
            kind_disc: self.kind_disc,
            kind,
            flags,
            payload_hash: self.payload_hash,
        })
    }
}

// ---------------------------------------------------------------------------
// Link line parser (returns owned LinkView since we need intermediate Strings)
// We borrow the line slice from the original buffer for eco/name fields.
// ---------------------------------------------------------------------------

fn parse_link_line<'a>(rest: &'a str) -> Result<LinkView<'a>, BlobError> {
    // rest = <eco>\t<pkg>\t<64-hex>\t<kind_self>\t<kind_other>
    let mut parts = rest.splitn(5, '\t');
    let eco = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("link missing eco: {}", rest)))?;
    let pkg = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("link missing pkg: {}", rest)))?;
    let intro_hex = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("link missing intro: {}", rest)))?;
    let ks_str = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("link missing kind_self: {}", rest)))?;
    let ko_str = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("link missing kind_other: {}", rest)))?;

    let intro_bytes = hex_to_32(intro_hex)?;
    let kind_self = kind_from_name(ks_str)?;
    let kind_other = kind_from_name(ko_str)?;

    Ok(LinkView {
        other_ecosystem: eco,
        other_name: pkg,
        other_intro: IntroId::from_raw(intro_bytes),
        kind_self,
        kind_other,
    })
}

fn parse_doclink_line(rest: &str) -> Result<DocLinkWire, BlobError> {
    // rest = <eco>\t<pkg>\t<64-hex>\t<escaped label> (label may be empty)
    let mut parts = rest.splitn(4, '\t');
    let eco = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("doclink missing eco: {}", rest)))?;
    let pkg = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("doclink missing pkg: {}", rest)))?;
    let intro_hex = parts
        .next()
        .ok_or_else(|| BlobError::MalformedLine(format!("doclink missing intro: {}", rest)))?;
    let label_raw = parts.next().unwrap_or("");
    let intro_bytes = hex_to_32(intro_hex)?;
    let label_s = unescape(label_raw)?;
    Ok(DocLinkWire {
        target: StableRef::new(
            PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
            IntroId::from_raw(intro_bytes),
        ),
        label: if label_s.is_empty() { None } else { Some(label_s) },
    })
}

fn parse_kind_body(disc: KindDiscriminant, lines: &[&str]) -> Result<KindWire, BlobError> {
    match disc {
        KindDiscriminant::Module => Ok(KindWire::Module(ModuleWire {})),
        KindDiscriminant::Record => {
            let mut fields: Vec<IntroId> = Vec::new();
            for line in lines {
                if let Some(rest) = line.strip_prefix("recfield\t") {
                    let bytes = hex_to_32(rest)?;
                    fields.push(IntroId::from_raw(bytes));
                }
            }
            Ok(KindWire::Record(RecordWire {
                fields: fields.into_boxed_slice(),
                // wire-v2 fields: not encoded in V1 debug-export
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
                form: nudox_ir::wire::RecordForm::Struct,
            }))
        }
        KindDiscriminant::Field => {
            let mut ty: Option<TypeRefWire> = None;
            for line in lines {
                if let Some(rest) = line.strip_prefix("fieldty\t") {
                    ty = Some(decode_typeref(rest)?);
                }
            }
            Ok(KindWire::Field(FieldWire { ty }))
        }
        KindDiscriminant::Function => {
            let mut input_params: Vec<ParamWire> = Vec::new();
            let mut output_params: Vec<ParamWire> = Vec::new();
            for line in lines {
                if let Some(rest) = line.strip_prefix("in\t") {
                    input_params.push(parse_param(rest)?);
                } else if let Some(rest) = line.strip_prefix("out\t") {
                    output_params.push(parse_param(rest)?);
                }
            }
            Ok(KindWire::Function(FunctionWire {
                input_params: input_params.into_boxed_slice(),
                output_params: output_params.into_boxed_slice(),
                // wire-v2 fields: not encoded in V1 debug-export
                sig: nudox_ir::wire::FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }))
        }
        KindDiscriminant::Type => {
            let type_line = lines
                .iter()
                .find(|l| l.starts_with("type\t"))
                .ok_or_else(|| BlobError::MalformedLine("Type kind missing 'type' line".into()))?;
            let rest = &type_line["type\t".len()..];
            // wire-v2: KindWire::Type wraps TypeAliasWire, not TypeWire directly
            Ok(KindWire::Type(nudox_ir::wire::TypeAliasWire {
                ty: decode_typeexpr(rest)?,
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }))
        }
        _ => Err(BlobError::UnsupportedKind(disc)),
    }
}

fn parse_param(rest: &str) -> Result<ParamWire, BlobError> {
    // rest = <escaped_pname>\t<typeref>
    let tab = rest
        .find('\t')
        .ok_or_else(|| BlobError::MalformedLine(format!("param missing typeref: {}", rest)))?;
    let pname_raw = &rest[..tab];
    let tr_str = &rest[tab + 1..];
    let pname = unescape(pname_raw)?;
    let ty = decode_typeref(tr_str)?;
    Ok(ParamWire {
        name: if pname.is_empty() { None } else { Some(pname) },
        ty,
    })
}

// ---------------------------------------------------------------------------
// Borrowed navigation (rkyv-style zero-copy graph access)
// ---------------------------------------------------------------------------
//
// These let a consumer walk a symbol's full structure — kind body, params,
// type refs — *without* building an owned `OwnedEntryPayload`. Scalars
// (`IntroId`, widths, lengths) are `Copy` and decoded on access; strings are
// borrowed `&'a str` slices (raw/escaped — the common no-escape case is the
// real value; call [`SymbolView::unescape`] when a value may contain escapes).
// Multi-element type lists are borrowed and iterated lazily (no `Vec` of owned
// content). This is the same "the buffer *is* the data" model as rkyv, adapted
// to the textual format.

/// A borrowed reference to a type (a leaf pointing at another symbol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRefView<'a> {
    /// Same-package intro.
    Same(IntroId),
    /// Cross-package reference: borrowed ecosystem/name + the intro.
    Foreign { ecosystem: &'a str, name: &'a str, intro: IntroId },
}

impl<'a> TypeRefView<'a> {
    fn parse(s: &'a str) -> Result<Self, BlobError> {
        if let Some(rest) = s.strip_prefix("S:") {
            Ok(TypeRefView::Same(IntroId::from_raw(hex_to_32(rest)?)))
        } else if let Some(rest) = s.strip_prefix("F:") {
            let hash_pos = rest
                .rfind('#')
                .ok_or_else(|| BlobError::MalformedLine(format!("no '#' in typeref: {}", s)))?;
            let intro = IntroId::from_raw(hex_to_32(&rest[hash_pos + 1..])?);
            let eco_pkg = &rest[..hash_pos];
            let slash = eco_pkg
                .find('/')
                .ok_or_else(|| BlobError::MalformedLine(format!("no '/' in typeref: {}", s)))?;
            Ok(TypeRefView::Foreign { ecosystem: &eco_pkg[..slash], name: &eco_pkg[slash + 1..], intro })
        } else {
            Err(BlobError::MalformedLine(format!("unknown typeref prefix: {}", s)))
        }
    }

    /// The intro this type reference points at (same for both variants).
    pub fn intro(&self) -> IntroId {
        match self {
            TypeRefView::Same(i) => *i,
            TypeRefView::Foreign { intro, .. } => *intro,
        }
    }

    /// Materialize an owned [`TypeRefWire`] (only when you need it).
    pub fn to_type_ref_wire(&self) -> TypeRefWire {
        match self {
            TypeRefView::Same(i) => TypeRefWire::Same(*i),
            TypeRefView::Foreign { ecosystem, name, intro } => TypeRefWire::Foreign(StableRef::new(
                PackageLineageId::new(EcosystemId::new(*ecosystem), PackageName::new(*name)),
                *intro,
            )),
        }
    }
}

/// A borrowed function parameter: raw name slice + a borrowed type ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamView<'a> {
    /// Raw (possibly escaped) parameter name; empty means unnamed.
    pub name: &'a str,
    pub ty: TypeRefView<'a>,
}

/// A borrowed, comma-separated list of type refs (Tuple/Union/Intersection).
/// Iterated lazily — no allocation of the elements.
#[derive(Debug, Clone, Copy)]
pub struct TypeRefList<'a>(&'a str);

impl<'a> TypeRefList<'a> {
    pub fn iter(&self) -> impl Iterator<Item = Result<TypeRefView<'a>, BlobError>> + 'a {
        let s = self.0;
        let inner = if s.is_empty() { "" } else { s };
        inner
            .split(',')
            .filter(|p| !p.is_empty())
            .map(TypeRefView::parse)
    }
}

/// A borrowed view of a type expression (a `Type` entry's body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExprView<'a> {
    SelfType,
    Never,
    Any,
    Bool,
    Char,
    Str,
    Integer { signed: bool, width: WidthWire },
    Float(WidthWire),
    MutPointer(TypeRefView<'a>),
    ConstPointer(TypeRefView<'a>),
    Reference { mutable: bool, ty: TypeRefView<'a> },
    /// Raw (possibly escaped) builtin name.
    Builtin(&'a str),
    Tuple(TypeRefList<'a>),
    Slice(TypeRefView<'a>),
    Array { ty: TypeRefView<'a>, length: u64 },
    Union(TypeRefList<'a>),
    Intersection(TypeRefList<'a>),
}

impl<'a> PartialEq for TypeRefList<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<'a> Eq for TypeRefList<'a> {}

fn parse_typeexpr_view(s: &str) -> Result<TypeExprView<'_>, BlobError> {
    match s {
        "self" => return Ok(TypeExprView::SelfType),
        "never" => return Ok(TypeExprView::Never),
        "any" => return Ok(TypeExprView::Any),
        _ => {}
    }
    if let Some(rest) = s.strip_prefix("prim:") {
        return match rest {
            "bool" => Ok(TypeExprView::Bool),
            "char" => Ok(TypeExprView::Char),
            "str" => Ok(TypeExprView::Str),
            _ => {
                if let Some(r) = rest.strip_prefix("int:") {
                    let colon = r.find(':').ok_or_else(|| BlobError::MalformedLine(format!("bad int: {}", s)))?;
                    let signed = match &r[..colon] { "s" => true, "u" => false, _ => return Err(BlobError::MalformedLine(format!("bad sign: {}", s))) };
                    Ok(TypeExprView::Integer { signed, width: decode_width(&r[colon + 1..])? })
                } else if let Some(r) = rest.strip_prefix("float:") {
                    Ok(TypeExprView::Float(decode_width(r)?))
                } else if let Some(r) = rest.strip_prefix("mutptr:") {
                    Ok(TypeExprView::MutPointer(TypeRefView::parse(r)?))
                } else if let Some(r) = rest.strip_prefix("constptr:") {
                    Ok(TypeExprView::ConstPointer(TypeRefView::parse(r)?))
                } else if let Some(r) = rest.strip_prefix("ref:") {
                    let colon = r.find(':').ok_or_else(|| BlobError::MalformedLine(format!("bad ref: {}", s)))?;
                    let mutable = match &r[..colon] { "mut" => true, "shared" => false, _ => return Err(BlobError::MalformedLine(format!("bad mut: {}", s))) };
                    Ok(TypeExprView::Reference { mutable, ty: TypeRefView::parse(&r[colon + 1..])? })
                } else if let Some(r) = rest.strip_prefix("builtin:") {
                    Ok(TypeExprView::Builtin(r))
                } else {
                    Err(BlobError::MalformedLine(format!("unknown prim: {}", s)))
                }
            }
        };
    }
    if let Some(r) = s.strip_prefix("tuple:") {
        return Ok(TypeExprView::Tuple(TypeRefList(r)));
    }
    if let Some(r) = s.strip_prefix("slice:") {
        return Ok(TypeExprView::Slice(TypeRefView::parse(r)?));
    }
    if let Some(r) = s.strip_prefix("array:") {
        let colon = r.rfind(':').ok_or_else(|| BlobError::MalformedLine(format!("bad array: {}", s)))?;
        let length: u64 = r[colon + 1..].parse().map_err(|_| BlobError::IntParse(format!("bad len: {}", s)))?;
        return Ok(TypeExprView::Array { ty: TypeRefView::parse(&r[..colon])?, length });
    }
    if let Some(r) = s.strip_prefix("union:") {
        return Ok(TypeExprView::Union(TypeRefList(r)));
    }
    if let Some(r) = s.strip_prefix("intersection:") {
        return Ok(TypeExprView::Intersection(TypeRefList(r)));
    }
    Err(BlobError::MalformedLine(format!("unknown typeexpr: {}", s)))
}

impl<'a> SymbolView<'a> {
    /// Unescape a raw borrowed field. Returns `Cow::Borrowed` (no allocation)
    /// when the slice contains no escape sequences — the common case.
    pub fn unescape(s: &str) -> Result<std::borrow::Cow<'_, str>, BlobError> {
        if s.contains('\\') {
            Ok(std::borrow::Cow::Owned(unescape(s)?))
        } else {
            Ok(std::borrow::Cow::Borrowed(s))
        }
    }

    /// `Record` body: the field intros, decoded lazily (empty for other kinds).
    pub fn record_fields(&self) -> impl Iterator<Item = IntroId> + '_ {
        self.kind_body_lines
            .iter()
            .filter_map(|l| l.strip_prefix("recfield\t"))
            .filter_map(|hex| hex_to_32(hex).ok().map(IntroId::from_raw))
    }

    /// `Field` body: the field's type ref, if any.
    pub fn field_type(&self) -> Option<Result<TypeRefView<'a>, BlobError>> {
        self.kind_body_lines
            .iter()
            .find_map(|l| l.strip_prefix("fieldty\t"))
            .map(TypeRefView::parse)
    }

    /// `Function` body: input parameters, borrowed + lazy.
    pub fn function_inputs(&self) -> impl Iterator<Item = Result<ParamView<'a>, BlobError>> + '_ {
        self.kind_body_lines
            .iter()
            .filter_map(|l| l.strip_prefix("in\t"))
            .map(parse_param_view)
    }

    /// `Function` body: output parameters, borrowed + lazy.
    pub fn function_outputs(&self) -> impl Iterator<Item = Result<ParamView<'a>, BlobError>> + '_ {
        self.kind_body_lines
            .iter()
            .filter_map(|l| l.strip_prefix("out\t"))
            .map(parse_param_view)
    }

    /// `Type` body: the type expression, borrowed.
    pub fn type_expr(&self) -> Option<Result<TypeExprView<'a>, BlobError>> {
        self.kind_body_lines
            .iter()
            .find_map(|l| l.strip_prefix("type\t"))
            .map(parse_typeexpr_view)
    }
}

fn parse_param_view(rest: &str) -> Result<ParamView<'_>, BlobError> {
    let tab = rest
        .find('\t')
        .ok_or_else(|| BlobError::MalformedLine(format!("param missing typeref: {}", rest)))?;
    Ok(ParamView { name: &rest[..tab], ty: TypeRefView::parse(&rest[tab + 1..])? })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
    use nudox_ir::kind::KindDiscriminant;
    use nudox_ir::wire::{
        DeprecationWire, DocLinkWire, EntryPayloadFlags, FieldWire, FnSigFlags, FunctionWire,
        KindWire, ModuleWire, OwnedEntryPayload, ParamWire, PrimitiveWire, RecordForm, RecordWire,
        SymbolWire, TypeAliasWire, TypeRefWire, TypeWire, WidthWire,
    };

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref(eco: &str, name: &str, n: u8) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new(eco), PackageName::new(name)),
            intro(n),
        )
    }

    fn two_links() -> Vec<LinkWire> {
        vec![
            LinkWire {
                other: sref("cargo", "lib", 2),
                kind_self: KindDiscriminant::Function,
                kind_other: KindDiscriminant::Module,
            },
            LinkWire {
                other: sref("npm", "pkg", 3),
                kind_self: KindDiscriminant::Function,
                kind_other: KindDiscriminant::Field,
            },
        ]
    }

    /// **Golden pin** of the blob text format. These bytes are what libpijul
    /// diffs and stores; format drift silently re-records every symbol as
    /// modified on the next generation and breaks old-blob decode. An
    /// intentional format change requires bumping the `NdIrSym` version, not
    /// editing this vector.
    #[test]
    fn blob_golden_pin() {
        let sym = SymbolWire {
            name: "golden".to_owned(),
            visibility: Visibility::Private,
            documentation: Some("line1\nline2".to_owned()),
            source_path: "src/lib.rs".to_owned(),
            span_start: 7,
            span_end: 21,
            aliases: vec!["b".to_owned(), "a".to_owned()],
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        };
        let payload = OwnedEntryPayload {
            kind_disc: KindDiscriminant::Function,
            payload_hash: nudox_ir::change::ContentBlake3::from_raw([0xCD; 32]),
            symbol: sym,
            kind: KindWire::Function(FunctionWire {
                input_params: Box::new([ParamWire {
                    name: Some("x".to_owned()),
                    ty: TypeRefWire::Same(intro(0x11)),
                }]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            flags: EntryPayloadFlags::default(),
        };
        let links = vec![LinkWire {
            other: sref("cargo", "lib", 2),
            kind_self: KindDiscriminant::Function,
            kind_other: KindDiscriminant::Module,
        }];
        let bytes = serialize_symbol_blob(&payload, Some(intro(0x01)), &links).unwrap();
        let expected = "NdIrSym\t1\n\
             name\tgolden\n\
             vis\tPrivate\n\
             kind\tFunction\n\
             span\t7\t21\n\
             hash\tcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\n\
             parent\t0101010101010101010101010101010101010101010101010101010101010101\n\
             src\tsrc/lib.rs\n\
             doc\tline1\\nline2\n\
             alias\ta\n\
             alias\tb\n\
             link\tcargo\tlib\t0202020202020202020202020202020202020202020202020202020202020202\tFunction\tModule\n\
             in\tx\tS:1111111111111111111111111111111111111111111111111111111111111111\n";
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            expected,
            "blob text format drifted — bump the NdIrSym version instead"
        );
    }

    fn rich_function_payload() -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: "my_func".to_owned(),
            visibility: Visibility::Private,
            documentation: Some("Has\ta\ttab and\nnewline".to_owned()),
            source_path: "src/lib.rs".to_owned(),
            span_start: 42,
            span_end: 99,
            aliases: vec!["z_alias".to_owned(), "a_alias".to_owned()],
            deprecation: Some(DeprecationWire {
                note: Some("Use new_func instead".to_owned()),
                since: Some("1.2.0".to_owned()),
            }),
            doc_links: vec![
                DocLinkWire { target: sref("cargo", "lib", 4), label: Some("see also\ttab".to_owned()) },
                DocLinkWire { target: sref("npm", "pkg", 5), label: None },
            ],
            attrs: vec![],
            cfg: None,
        };
        let fn_wire = || KindWire::Function(FunctionWire {
            input_params: Box::new([
                ParamWire { name: Some("x".into()), ty: TypeRefWire::Same(intro(9)) },
                ParamWire { name: None, ty: TypeRefWire::Foreign(sref("npm", "types", 3)) },
            ]),
            output_params: Box::new([ParamWire { name: None, ty: TypeRefWire::Same(intro(5)) }]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        });
        OwnedEntryPayload {
            kind_disc: KindDiscriminant::Function,
            payload_hash: OwnedEntryPayload::compute_payload_hash(
                &sym,
                &KindDiscriminant::Function,
                &fn_wire(),
                &EntryPayloadFlags::default(),
            ),
            kind: fn_wire(),
            symbol: sym,
            flags: EntryPayloadFlags::default(),
        }
    }

    // ---- Test 1: textual / diff-friendly proof ----------------------------

    #[test]
    fn textual_diff_friendly() {
        let payload = rich_function_payload();
        let bytes = serialize_symbol_blob(&payload, None, &[]).unwrap();
        let text = std::str::from_utf8(&bytes).expect("must be valid UTF-8");

        // Must contain newline-separated name line
        assert!(text.contains("name\tmy_func\n"), "must have name line");
        // Must have in\t lines
        assert!(text.contains("in\tx\t"), "must have named param line");

        // Now add one more input param and check exactly one new "in\t" line appears
        let payload2 = {
            let mut p = payload.clone();
            if let KindWire::Function(ref mut f) = p.kind {
                let mut params = f.input_params.to_vec();
                params.push(ParamWire {
                    name: Some("y".into()),
                    ty: TypeRefWire::Same(intro(7)),
                });
                f.input_params = params.into_boxed_slice();
            }
            p
        };
        let bytes2 = serialize_symbol_blob(&payload2, None, &[]).unwrap();
        let text2 = std::str::from_utf8(&bytes2).expect("must be valid UTF-8");

        let lines1: std::collections::HashSet<_> = text.lines().collect();
        let lines2: std::collections::HashSet<_> = text2.lines().collect();
        let added: Vec<_> = lines2.difference(&lines1).copied().collect();
        // Exactly one new line should appear and it should be the new `in\t` line
        assert_eq!(added.len(), 1, "only one line should differ");
        assert!(added[0].starts_with("in\t"), "new line must be an `in\\t` param line");
    }

    // ---- Test 2: full round-trip ------------------------------------------

    #[test]
    fn round_trip_full() {
        let payload = rich_function_payload();
        let parent = Some(intro(1));
        let links = two_links();

        let bytes = serialize_symbol_blob(&payload, parent, &links).unwrap();
        let view = SymbolView::from_bytes(&bytes).expect("from_bytes must succeed");

        // Borrow-based accessors
        // name() returns a borrowed &str that still has escape sequences (raw)
        // because the blob stores the escaped form and name() borrows it directly.
        // We unescape in to_owned_payload(); the accessor returns the raw escaped slice.
        assert_eq!(view.visibility(), Visibility::Private);
        assert_eq!(view.kind_disc(), KindDiscriminant::Function);
        let (ss, se) = view.span();
        assert_eq!(ss, 42);
        assert_eq!(se, 99);
        assert_eq!(view.payload_hash(), payload.payload_hash);
        assert_eq!(view.parent(), parent);
        // IS_REFERENCE retired in wire-v2 — flags are always default on read

        // Links
        let viewed_links: Vec<_> = view.links().collect();
        assert_eq!(viewed_links.len(), 2);
        // Links are sorted in output, so we just check all are present
        let link_wires: Vec<_> = viewed_links.iter().map(|lv| lv.to_link_wire()).collect();
        for lw in &links {
            assert!(link_wires.contains(lw), "link {:?} must be in output", lw);
        }

        // to_owned_payload round-trip
        let recon = view.to_owned_payload().expect("to_owned_payload");
        assert_eq!(recon.symbol.name, payload.symbol.name);
        assert_eq!(recon.symbol.visibility, payload.symbol.visibility);
        assert_eq!(recon.symbol.documentation, payload.symbol.documentation);
        assert_eq!(recon.symbol.source_path, payload.symbol.source_path);
        assert_eq!(recon.symbol.span_start, payload.symbol.span_start);
        assert_eq!(recon.symbol.span_end, payload.symbol.span_end);
        // Aliases are sorted on output; compare sorted-vs-sorted
        let mut expected_aliases = payload.symbol.aliases.clone();
        expected_aliases.sort();
        assert_eq!(recon.symbol.aliases, expected_aliases);
        assert_eq!(recon.symbol.deprecation, payload.symbol.deprecation);
        // doc_links must survive the round-trip (canonical sorted order).
        let mut expected_dl = payload.symbol.doc_links.clone();
        expected_dl.sort_by(|a, b| {
            (a.target.package.ecosystem.as_str(), a.target.package.name.as_str(), a.target.intro.as_bytes())
                .cmp(&(b.target.package.ecosystem.as_str(), b.target.package.name.as_str(), b.target.intro.as_bytes()))
        });
        assert_eq!(recon.symbol.doc_links, expected_dl, "doc_links must be preserved");
        assert!(!recon.symbol.doc_links.is_empty(), "test must actually exercise doc_links");
        assert_eq!(recon.kind_disc, payload.kind_disc);
        assert_eq!(recon.kind, payload.kind);
        assert_eq!(recon.flags, payload.flags);
        assert_eq!(recon.payload_hash, payload.payload_hash);
    }

    // ---- Test 3a: Record round-trip --------------------------------------

    #[test]
    fn round_trip_record() {
        let sym = SymbolWire {
            name: "MyRecord".to_owned(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/foo.rs".to_owned(),
            span_start: 10,
            span_end: 100,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        };
        let kind = KindWire::Record(RecordWire {
            fields: Box::new([intro(1), intro(2), intro(3)]),
            form: RecordForm::Struct,
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        });
        let payload = OwnedEntryPayload {
            kind_disc: KindDiscriminant::Record,
            payload_hash: OwnedEntryPayload::compute_payload_hash(
                &sym,
                &KindDiscriminant::Record,
                &kind,
                &EntryPayloadFlags::default(),
            ),
            kind,
            symbol: sym,
            flags: EntryPayloadFlags::default(),
        };
        let bytes = serialize_symbol_blob(&payload, None, &[]).unwrap();
        let view = SymbolView::from_bytes(&bytes).expect("parse");
        assert_eq!(view.kind_disc(), KindDiscriminant::Record);
        let recon = view.to_owned_payload().expect("to_owned_payload");
        assert_eq!(recon.kind, payload.kind);
    }

    // ---- Test 3b: Field round-trip ----------------------------------------

    #[test]
    fn round_trip_field() {
        let sym = SymbolWire {
            name: "my_field".to_owned(),
            visibility: Visibility::Protected,
            documentation: None,
            source_path: "src/foo.rs".to_owned(),
            span_start: 100,
            span_end: 120,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        };
        let kind =
            KindWire::Field(FieldWire { ty: Some(TypeRefWire::Same(intro(5))) });
        let payload = OwnedEntryPayload {
            kind_disc: KindDiscriminant::Field,
            payload_hash: OwnedEntryPayload::compute_payload_hash(
                &sym,
                &KindDiscriminant::Field,
                &kind,
                &EntryPayloadFlags::default(),
            ),
            kind,
            symbol: sym,
            flags: EntryPayloadFlags::default(),
        };
        let bytes = serialize_symbol_blob(&payload, Some(intro(10)), &[]).unwrap();
        let view = SymbolView::from_bytes(&bytes).expect("parse");
        assert_eq!(view.kind_disc(), KindDiscriminant::Field);
        assert_eq!(view.parent(), Some(intro(10)));
        let recon = view.to_owned_payload().expect("to_owned_payload");
        assert_eq!(recon.kind, payload.kind);
    }

    // ---- Test 3c: Type round-trip (all TypeWire variants) ----------------

    fn type_round_trip(tw: TypeWire) {
        let sym = SymbolWire {
            name: "T".to_owned(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "".to_owned(),
            span_start: 0,
            span_end: 1,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        };
        // wire-v2: KindWire::Type wraps TypeAliasWire
        let kind = KindWire::Type(TypeAliasWire {
            ty: tw.clone(),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        });
        let payload = OwnedEntryPayload {
            kind_disc: KindDiscriminant::Type,
            payload_hash: OwnedEntryPayload::compute_payload_hash(
                &sym,
                &KindDiscriminant::Type,
                &kind,
                &EntryPayloadFlags::default(),
            ),
            kind,
            symbol: sym,
            flags: EntryPayloadFlags::default(),
        };
        let bytes = serialize_symbol_blob(&payload, None, &[]).unwrap();
        let view = SymbolView::from_bytes(&bytes).expect("parse");
        let recon = view.to_owned_payload().expect("to_owned_payload");
        assert_eq!(recon.kind, payload.kind, "TypeWire {:?} failed round-trip", tw);
    }

    #[test]
    fn round_trip_type_variants() {
        type_round_trip(TypeWire::SelfType);
        type_round_trip(TypeWire::Never);
        type_round_trip(TypeWire::Any);
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Bool));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Char));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Str));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Integer {
            signed: true,
            width: WidthWire::Fixed(64),
        }));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Integer {
            signed: false,
            width: WidthWire::Arch,
        }));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Float(WidthWire::Fixed(32))));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::MutPointer(Box::new(
            TypeRefWire::Same(intro(1)),
        ))));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::ConstPointer(Box::new(
            TypeRefWire::Foreign(sref("cargo", "foo", 9)),
        ))));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Reference {
            lifetime: None,
            mutable: true,
            ty: Box::new(TypeRefWire::Same(intro(3))),
        }));
        type_round_trip(TypeWire::Primitive(PrimitiveWire::Builtin("void".to_owned())));
        type_round_trip(TypeWire::Tuple(Box::new([
            TypeRefWire::Same(intro(1)),
            TypeRefWire::Same(intro(2)),
        ])));
        type_round_trip(TypeWire::Slice(Box::new(TypeRefWire::Same(intro(4)))));
        type_round_trip(TypeWire::Array {
            ty: Box::new(TypeRefWire::Same(intro(5))),
            length: 42,
        });
        type_round_trip(TypeWire::Union(Box::new([
            TypeRefWire::Same(intro(6)),
            TypeRefWire::Foreign(sref("npm", "x", 7)),
        ])));
        type_round_trip(TypeWire::Intersection(Box::new([TypeRefWire::Same(intro(8))])));
    }

    // ---- Test 4: determinism + sorted aliases/links ----------------------

    #[test]
    fn determinism_and_sorted_output() {
        let payload = rich_function_payload();
        let links = two_links();
        let b1 = serialize_symbol_blob(&payload, Some(intro(1)), &links).unwrap();
        let b2 = serialize_symbol_blob(&payload, Some(intro(1)), &links).unwrap();
        assert_eq!(b1, b2, "must be deterministic");

        // Give links in reversed order — output must be the same
        let mut links_rev = links.clone();
        links_rev.reverse();
        let b3 = serialize_symbol_blob(&payload, Some(intro(1)), &links_rev).unwrap();
        assert_eq!(b1, b3, "link order must not affect output");

        // aliases in reversed order — output must be the same
        let mut payload2 = payload.clone();
        payload2.symbol.aliases = vec!["z_alias".to_owned(), "a_alias".to_owned()];
        let b4 = serialize_symbol_blob(&payload2, Some(intro(1)), &links).unwrap();
        payload2.symbol.aliases = vec!["a_alias".to_owned(), "z_alias".to_owned()];
        let b5 = serialize_symbol_blob(&payload2, Some(intro(1)), &links).unwrap();
        assert_eq!(b4, b5, "alias order must not affect output");

        // aliases appear sorted in text
        let text = std::str::from_utf8(&b1).unwrap();
        let alias_pos_a = text.find("alias\ta_alias").unwrap();
        let alias_pos_z = text.find("alias\tz_alias").unwrap();
        assert!(alias_pos_a < alias_pos_z, "aliases must appear in sorted order");
    }

    // ---- Test 5: borrow proof at odd offset ------------------------------

    #[test]
    fn borrow_at_odd_offset() {
        let payload = rich_function_payload();
        let links = two_links();
        let blob = serialize_symbol_blob(&payload, Some(intro(42)), &links).unwrap();

        let mut padded = Vec::with_capacity(blob.len() + 1);
        padded.push(0u8);
        padded.extend_from_slice(&blob);

        let view = SymbolView::from_bytes(&padded[1..]).expect("must parse at odd offset");

        // name() and links() must return borrows into the padded[1..] buffer
        // (no allocation of content). We verify correctness:
        let raw_name = view.name();
        // raw_name is the escaped form in the buffer; for "my_func" no escaping needed
        assert_eq!(raw_name, "my_func");

        let link_count = view.links().count();
        assert_eq!(link_count, 2, "must have 2 links at odd offset");

        // Verify the pointer actually points into padded[1..]
        let buf_start = padded[1..].as_ptr() as usize;
        let buf_end = buf_start + padded[1..].len();
        let name_ptr = raw_name.as_ptr() as usize;
        assert!(
            name_ptr >= buf_start && name_ptr < buf_end,
            "name() must point into the source buffer"
        );
    }

    // ---- Test 6: corrupt / truncated / bad UTF-8 → Err, never panic ------

    #[test]
    fn corrupt_input_errors() {
        // Empty
        assert!(SymbolView::from_bytes(&[]).is_err());

        // Bad magic
        assert!(matches!(
            SymbolView::from_bytes(b"BADMAGIC\t1\n"),
            Err(BlobError::BadMagic)
        ));

        // Wrong version
        assert!(matches!(
            SymbolView::from_bytes(b"NdIrSym\t99\n"),
            Err(BlobError::UnsupportedVersion(99))
        ));

        // Missing required fields → MalformedLine
        assert!(SymbolView::from_bytes(b"NdIrSym\t1\n").is_err());

        // Truncated in middle of a valid blob
        let payload = rich_function_payload();
        let blob = serialize_symbol_blob(&payload, None, &[]).unwrap();
        let truncated = &blob[..blob.len() / 2];
        // May succeed (partial parse) or fail — must not panic
        let _ = SymbolView::from_bytes(truncated);

        // Invalid UTF-8 → Utf8 error
        let bad_utf8: &[u8] = b"NdIrSym\t1\nname\t\xff\xff\n";
        assert!(matches!(SymbolView::from_bytes(bad_utf8), Err(BlobError::Utf8(_))));

        // All zeros
        let zeros = [0u8; 200];
        assert!(SymbolView::from_bytes(&zeros).is_err());

        // Random bytes must not panic
        let garbage: Vec<u8> = (0u8..=255).cycle().take(300).collect();
        let _ = SymbolView::from_bytes(&garbage);
    }

    // ---- Test 8: escaping round-trip in doc/name/src ----------------------

    #[test]
    fn escape_round_trip() {
        let sym = SymbolWire {
            name: "a\\b\tc\nd".to_owned(),
            visibility: Visibility::Public,
            documentation: Some("line1\nline2\ttabbed\\slash".to_owned()),
            source_path: "path/with\ttab".to_owned(),
            span_start: 0,
            span_end: 1,
            aliases: vec!["al\\ias".to_owned()],
            deprecation: Some(DeprecationWire {
                since: Some("v1\t2".to_owned()),
                note: Some("note\nwith\\newline".to_owned()),
            }),
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        };
        let kind = KindWire::Module(ModuleWire {});
        let flags = EntryPayloadFlags::default();
        let payload = OwnedEntryPayload {
            kind_disc: KindDiscriminant::Module,
            payload_hash: OwnedEntryPayload::compute_payload_hash(
                &sym,
                &KindDiscriminant::Module,
                &kind,
                &flags,
            ),
            kind,
            symbol: sym.clone(),
            flags,
        };
        let bytes = serialize_symbol_blob(&payload, None, &[]).unwrap();
        let view = SymbolView::from_bytes(&bytes).expect("parse");
        let recon = view.to_owned_payload().expect("to_owned_payload");
        assert_eq!(recon.symbol.name, sym.name);
        assert_eq!(recon.symbol.documentation, sym.documentation);
        assert_eq!(recon.symbol.source_path, sym.source_path);
        assert_eq!(recon.symbol.aliases, sym.aliases);
        assert_eq!(recon.symbol.deprecation, sym.deprecation);
    }

    // ---- Test 9: rkyv-style borrowed navigation (NO to_owned_payload) -----

    #[test]
    fn borrowed_graph_navigation() {
        // Function: walk params via borrowed views — no owned payload built.
        let payload = rich_function_payload();
        let bytes = serialize_symbol_blob(&payload, Some(intro(1)), &two_links()).unwrap();
        let view = SymbolView::from_bytes(&bytes).unwrap();

        let inputs: Vec<ParamView> = view.function_inputs().map(|r| r.unwrap()).collect();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].name, "x");
        assert_eq!(inputs[0].ty, TypeRefView::Same(intro(9)));
        assert_eq!(inputs[1].name, ""); // unnamed
        match &inputs[1].ty {
            TypeRefView::Foreign { ecosystem, name, intro: i } => {
                assert_eq!(*ecosystem, "npm");
                assert_eq!(*name, "types");
                assert_eq!(*i, intro(3));
            }
            other => panic!("expected foreign, got {:?}", other),
        }
        let outputs: Vec<_> = view.function_outputs().map(|r| r.unwrap()).collect();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].ty.intro(), intro(5));

        // Record: walk field intros lazily.
        let rec = record_payload();
        let rb = serialize_symbol_blob(&rec, None, &[]).unwrap();
        let rv = SymbolView::from_bytes(&rb).unwrap();
        let fields: Vec<IntroId> = rv.record_fields().collect();
        assert_eq!(fields, vec![intro(1), intro(2), intro(3)]);

        // Type: walk a Tuple's element list lazily via TypeRefList.
        let ty = type_payload(TypeWire::Tuple(Box::new([
            TypeRefWire::Same(intro(1)),
            TypeRefWire::Foreign(sref("cargo", "x", 2)),
        ])));
        let tb = serialize_symbol_blob(&ty, None, &[]).unwrap();
        let tv = SymbolView::from_bytes(&tb).unwrap();
        match tv.type_expr().unwrap().unwrap() {
            TypeExprView::Tuple(list) => {
                let els: Vec<IntroId> = list.iter().map(|r| r.unwrap().intro()).collect();
                assert_eq!(els, vec![intro(1), intro(2)]);
            }
            other => panic!("expected tuple, got {:?}", other),
        }
    }

    fn base_sym(name: &str) -> SymbolWire {
        SymbolWire {
            name: name.into(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "".into(),
            span_start: 0,
            span_end: 1,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: vec![],
            cfg: None,
        }
    }

    fn record_payload() -> OwnedEntryPayload {
        let sym = base_sym("R");
        let kind = KindWire::Record(RecordWire {
            fields: Box::new([intro(1), intro(2), intro(3)]),
            form: RecordForm::Struct,
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        });
        OwnedEntryPayload {
            kind_disc: KindDiscriminant::Record,
            payload_hash: OwnedEntryPayload::compute_payload_hash(&sym, &KindDiscriminant::Record, &kind, &EntryPayloadFlags::default()),
            kind, symbol: sym, flags: EntryPayloadFlags::default(),
        }
    }

    fn type_payload(tw: TypeWire) -> OwnedEntryPayload {
        let sym = base_sym("T");
        // wire-v2: KindWire::Type wraps TypeAliasWire
        let kind = KindWire::Type(TypeAliasWire {
            ty: tw,
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        });
        OwnedEntryPayload {
            kind_disc: KindDiscriminant::Type,
            payload_hash: OwnedEntryPayload::compute_payload_hash(&sym, &KindDiscriminant::Type, &kind, &EntryPayloadFlags::default()),
            kind, symbol: sym, flags: EntryPayloadFlags::default(),
        }
    }
}
