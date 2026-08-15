//! NdIrF1 — the canonical working-copy format for one `{intro_hex}.nir` file.
//!
//! # Format layout (§6.5)
//!
//! ```text
//! NdIrF1\t1\n
//! <key>\t<value>\n    ← scalar (0 or 1 line)
//! <key>\t<value>\n    ← set (0..n sorted ascending by raw bytes)
//! <key>\t<value>\n    ← seq (0..n in declaration order)
//! …
//! ```
//!
//! Keys are emitted in frozen registry order (§6.2).  The parser is STRICT:
//! unknown key → error; wrong section order → error; unsorted set → error.
//!
//! Escaping (§6.3): `\` → `\\`  TAB → `\t`  LF → `\n`  CR → `\r`
//! other C0 → `\xNN`; decode rejects unknown escapes and trailing `\`.

use crate::wire::{
    AttrTok, AutoFact, AutoState, AutoTrait, CfgExpr, ConstWire, DeprecationWire, DocLinkWire,
    EnumWire, FieldWire, FnSigFlags, FunctionWire, GenericParamWire, ImplFlags, ImplWire, KindWire,
    ModuleWire, OwnedEntryPayload, ParamWire, RecordForm, RecordWire, ReexportWire, Sealed,
    SelfKind, StaticWire, SymbolWire, TraitFlags, TraitWire, TriState, TypeAliasWire, TypeRefWire,
    TypeWire, VariantForm, VariantWire, WherePredWire,
};
use ir::change::{ContentBlake3, EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

use crate::ascii::{
    decode_typeexpr, decode_typeref, encode_typeexpr, encode_typeref, escape, hex_to_32, unescape,
};
use crate::serialize::LinkWire;

// ---------------------------------------------------------------------------
// Key registry constants — now inlined into the vendored libpijul fork as
// `libpijul::nudox_f1::registry` (DEPTH1 plan §3.2).  The re-export keeps
// this crate’s public surface unchanged.
// ---------------------------------------------------------------------------

pub use libpijul::nudox_f1::registry::{
    KEY_ALIAS, KEY_ATTR, KEY_AUTO, KEY_CFG, KEY_CTY, KEY_CVAL, KEY_DEPRECATED, KEY_DLINK, KEY_DOC,
    KEY_FIELDTY, KEY_FNSIG, KEY_GPARAM, KEY_IFLAGS, KEY_IFOR, KEY_IN, KEY_IOF, KEY_KIND, KEY_LFACT,
    KEY_LINK, KEY_MAGIC, KEY_NAME, KEY_OUT, KEY_PARENT, KEY_RECFIELD, KEY_RECFORM, KEY_RETGT,
    KEY_SPAN, KEY_SRC, KEY_SUPER, KEY_TFLAGS, KEY_TYPE, KEY_VDISCR, KEY_VFORM, KEY_VIS, KEY_WHERE,
};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("bad magic: expected NdIrF1")]
    BadMagic,
    #[error("unsupported format version: {0}")]
    UnsupportedVersion(u16),
    #[error("unknown key in F1 file: '{0}' (strict canonical form required)")]
    UnknownKey(String),
    #[error("key '{0}' appeared out of registry order")]
    OutOfOrder(String),
    #[error("set key '{0}' lines not sorted: '{1}' followed by '{2}'")]
    UnsortedSet(String, String, String),
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("malformed line value: {0}")]
    Malformed(String),
    #[error("unsupported kind for F1 format: {0:?}")]
    UnsupportedKind(KindDiscriminant),
    #[error("invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("ascii encoding error: {0}")]
    Ascii(#[from] crate::ascii::Error),
}

// ---------------------------------------------------------------------------
// Continuity summary types (used by FinishReport)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuityOp {
    Introduced {
        id: IntroId,
    },
    Resurrected {
        id: IntroId,
    },
    Deleted {
        id: IntroId,
    },
    Renamed {
        id: IntroId,
        old_name: String,
        new_name: String,
    },
    Moved {
        id: IntroId,
        old_parent: Option<IntroId>,
        new_parent: Option<IntroId>,
    },
    SignatureEvolved {
        id: IntroId,
        old_sigkey: ContentBlake3,
        new_sigkey: ContentBlake3,
    },
}

#[derive(Debug, Clone)]
pub struct RenameEdge {
    pub deleted_id: IntroId,
    pub added_id: IntroId,
    pub score: i32,
}

#[derive(Debug, Default, Clone)]
pub struct ContinuitySummary {
    pub ops: Vec<ContinuityOp>,
    pub rename_edges: Vec<RenameEdge>,
}

// ---------------------------------------------------------------------------
// Visibility encoding
// ---------------------------------------------------------------------------

fn encode_vis(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Protected => "protected",
        Visibility::Internal => "internal",
        Visibility::Package => "package",
        Visibility::Crate => "crate",
    }
}

fn decode_vis(s: &str) -> Result<Visibility, Error> {
    match s {
        "public" => Ok(Visibility::Public),
        "private" => Ok(Visibility::Private),
        "protected" => Ok(Visibility::Protected),
        "internal" => Ok(Visibility::Internal),
        "package" => Ok(Visibility::Package),
        "crate" => Ok(Visibility::Crate),
        _ => Err(Error::Malformed(format!("unknown visibility: {s}"))),
    }
}

// ---------------------------------------------------------------------------
// Kind token encoding
// ---------------------------------------------------------------------------

fn encode_kind_token(k: KindDiscriminant) -> &'static str {
    match k {
        KindDiscriminant::Module => "module",
        KindDiscriminant::Record => "record",
        KindDiscriminant::Field => "field",
        KindDiscriminant::Param => "param",
        KindDiscriminant::Function => "function",
        KindDiscriminant::Alias => "type",
        KindDiscriminant::Trait => "trait",
        KindDiscriminant::Impl => "impl",
        KindDiscriminant::Enum => "enum",
        KindDiscriminant::Variant => "variant",
        KindDiscriminant::Const => "const",
        KindDiscriminant::Static => "static",
        KindDiscriminant::Reexport => "reexport",
    }
}

fn decode_kind_token(s: &str) -> Result<KindDiscriminant, Error> {
    match s {
        "module" => Ok(KindDiscriminant::Module),
        "record" => Ok(KindDiscriminant::Record),
        "field" => Ok(KindDiscriminant::Field),
        "function" => Ok(KindDiscriminant::Function),
        "type" => Ok(KindDiscriminant::Alias),
        "trait" => Ok(KindDiscriminant::Trait),
        "impl" => Ok(KindDiscriminant::Impl),
        "enum" => Ok(KindDiscriminant::Enum),
        "variant" => Ok(KindDiscriminant::Variant),
        "const" => Ok(KindDiscriminant::Const),
        "static" => Ok(KindDiscriminant::Static),
        "reexport" => Ok(KindDiscriminant::Reexport),
        "param" => Ok(KindDiscriminant::Param),
        _ => Err(Error::Malformed(format!("unknown kind token: {s}"))),
    }
}

// ---------------------------------------------------------------------------
// FnSig encoding
// ---------------------------------------------------------------------------

fn encode_fnsig(sig: &FnSigFlags) -> String {
    // Fixed token order: self:<...> [async] [const] [unsafe] [abi:<esc>] [variadic] [defaulted]
    let mut parts = Vec::new();
    let self_str = match &sig.self_kind {
        SelfKind::None => "self:none".to_owned(),
        SelfKind::Value => "self:value".to_owned(),
        SelfKind::Ref => "self:ref".to_owned(),
        SelfKind::RefMut => "self:refmut".to_owned(),
        SelfKind::Arbitrary(tr) => format!("self:arb:{}", encode_typeref(tr)),
    };
    parts.push(self_str);
    if sig.is_async {
        parts.push("async".to_owned());
    }
    if sig.is_const {
        parts.push("const".to_owned());
    }
    if sig.is_unsafe {
        parts.push("unsafe".to_owned());
    }
    if let Some(abi) = &sig.abi {
        parts.push(format!("abi:{}", escape(abi)));
    }
    if sig.variadic {
        parts.push("variadic".to_owned());
    }
    if sig.defaulted {
        parts.push("defaulted".to_owned());
    }
    parts.join("\t")
}

fn decode_fnsig(tokens: &[&str]) -> Result<FnSigFlags, Error> {
    if tokens.is_empty() {
        return Err(Error::Malformed("empty fnsig".into()));
    }
    // First token is always self:<...>
    let self_kind = if let Some(rest) = tokens[0].strip_prefix("self:") {
        match rest {
            "none" => SelfKind::None,
            "value" => SelfKind::Value,
            "ref" => SelfKind::Ref,
            "refmut" => SelfKind::RefMut,
            _ => {
                if let Some(tr_str) = rest.strip_prefix("arb:") {
                    SelfKind::Arbitrary(decode_typeref(tr_str)?)
                } else {
                    return Err(Error::Malformed(format!("bad fnsig self: {}", tokens[0])));
                }
            }
        }
    } else {
        return Err(Error::Malformed(format!(
            "fnsig must start with self:, got: {}",
            tokens[0]
        )));
    };

    let mut is_async = false;
    let mut is_const = false;
    let mut is_unsafe = false;
    let mut abi = None;
    let mut variadic = false;
    let mut defaulted = false;

    for tok in &tokens[1..] {
        match *tok {
            "async" => is_async = true,
            "const" => is_const = true,
            "unsafe" => is_unsafe = true,
            "variadic" => variadic = true,
            "defaulted" => defaulted = true,
            _ => {
                if let Some(rest) = tok.strip_prefix("abi:") {
                    abi = Some(unescape(rest)?);
                } else {
                    return Err(Error::Malformed(format!("unknown fnsig token: {tok}")));
                }
            }
        }
    }
    Ok(FnSigFlags {
        self_kind,
        is_async,
        is_const,
        is_unsafe,
        abi,
        variadic,
        defaulted,
    })
}

// ---------------------------------------------------------------------------
// GenericParam encoding (gparam lines)
// ---------------------------------------------------------------------------

fn encode_gparam(gp: &GenericParamWire) -> String {
    match gp {
        GenericParamWire::Lifetime { name } => format!("life:{}", escape(name)),
        GenericParamWire::Type {
            name,
            bounds,
            default,
        } => {
            let mut s = format!("type:{}", escape(name));
            if !bounds.is_empty() {
                let bound_list = encode_bound_list(bounds);
                s.push('\t');
                s.push_str("bounds:");
                s.push_str(&bound_list);
            }
            if let Some(def) = default {
                s.push('\t');
                s.push_str("default:");
                s.push_str(&encode_typeexpr(def));
            }
            s
        }
        GenericParamWire::Const { name, ty, default } => {
            let mut s = format!("const:{}\t{}", escape(name), encode_typeref(ty));
            if let Some(def) = default {
                s.push('\t');
                s.push_str("default:");
                s.push_str(&escape(def));
            }
            s
        }
    }
}

fn decode_gparam(s: &str) -> Result<GenericParamWire, Error> {
    if let Some(rest) = s.strip_prefix("life:") {
        return Ok(GenericParamWire::Lifetime {
            name: unescape(rest)?,
        });
    }
    if let Some(rest) = s.strip_prefix("type:") {
        // format: <name>[TABbounds:<bound-list>][TABdefault:<typeexpr>]
        let parts: Vec<&str> = rest.splitn(3, '\t').collect();
        let name = unescape(parts[0])?;
        let mut bounds = Box::new([]) as Box<[TypeRefWire]>;
        let mut default = None;
        for part in &parts[1..] {
            if let Some(b) = part.strip_prefix("bounds:") {
                bounds = decode_bound_list(b)?;
            } else if let Some(d) = part.strip_prefix("default:") {
                default = Some(decode_typeexpr(d)?);
            }
        }
        return Ok(GenericParamWire::Type {
            name,
            bounds,
            default,
        });
    }
    if let Some(rest) = s.strip_prefix("const:") {
        let parts: Vec<&str> = rest.splitn(3, '\t').collect();
        if parts.len() < 2 {
            return Err(Error::Malformed(format!("bad const gparam: {s}")));
        }
        let name = unescape(parts[0])?;
        let ty = decode_typeref(parts[1])?;
        let default = if let Some(p) = parts.get(2) {
            p.strip_prefix("default:")
                .map(|d| -> Result<_, Error> { Ok(unescape(d)?) })
                .transpose()?
        } else {
            None
        };
        return Ok(GenericParamWire::Const { name, ty, default });
    }
    Err(Error::Malformed(format!("unknown gparam prefix: {s}")))
}

// ---------------------------------------------------------------------------
// Where predicate encoding (where lines)
// ---------------------------------------------------------------------------

fn encode_where(wp: &WherePredWire) -> String {
    // <typeexpr>TAB<bound-list>
    format!(
        "{}\t{}",
        encode_typeexpr(&wp.target),
        encode_bound_list(&wp.bounds)
    )
}

fn decode_where(s: &str) -> Result<WherePredWire, Error> {
    let tab = s
        .find('\t')
        .ok_or_else(|| Error::Malformed(format!("where missing TAB: {s}")))?;
    let target = decode_typeexpr(&s[..tab])?;
    let bounds = decode_bound_list(&s[tab + 1..])?;
    Ok(WherePredWire { target, bounds })
}

/// Bound list = `+`-joined sorted typerefs.
fn encode_bound_list(bounds: &[TypeRefWire]) -> String {
    let mut parts: Vec<String> = bounds.iter().map(encode_typeref).collect();
    parts.sort(); // canonical sorted order by raw string
    parts.join("+")
}

fn decode_bound_list(s: &str) -> Result<Box<[TypeRefWire]>, Error> {
    if s.is_empty() {
        return Ok(Box::new([]));
    }
    let parts: Vec<_> = s.split('+').collect();
    let refs: Result<Vec<_>, _> = parts.iter().map(|p| decode_typeref(p)).collect();
    Ok(refs?.into_boxed_slice())
}

// ---------------------------------------------------------------------------
// TraitFlags encoding
// ---------------------------------------------------------------------------

fn encode_tflags(f: &TraitFlags) -> String {
    let mut parts = Vec::new();
    if f.is_auto {
        parts.push("auto".to_owned());
    }
    if f.is_unsafe {
        parts.push("unsafe".to_owned());
    }
    let dyn_str = match f.dyn_compat {
        TriState::Yes => "dyn:yes",
        TriState::No => "dyn:no",
        TriState::Unknown => "dyn:unk",
    };
    parts.push(dyn_str.to_owned());
    let sealed_str = match f.sealed {
        Sealed::None => "sealed:none",
        Sealed::PubApi => "sealed:pubapi",
        Sealed::Full => "sealed:full",
    };
    parts.push(sealed_str.to_owned());
    parts.join("\t")
}

fn decode_tflags(tokens: &[&str]) -> Result<TraitFlags, Error> {
    let mut flags = TraitFlags::default();
    for tok in tokens {
        match *tok {
            "auto" => flags.is_auto = true,
            "unsafe" => flags.is_unsafe = true,
            "dyn:yes" => flags.dyn_compat = TriState::Yes,
            "dyn:no" => flags.dyn_compat = TriState::No,
            "dyn:unk" => flags.dyn_compat = TriState::Unknown,
            "sealed:none" => flags.sealed = Sealed::None,
            "sealed:pubapi" => flags.sealed = Sealed::PubApi,
            "sealed:full" => flags.sealed = Sealed::Full,
            _ => return Err(Error::Malformed(format!("unknown tflags token: {tok}"))),
        }
    }
    Ok(flags)
}

// ---------------------------------------------------------------------------
// ImplFlags encoding
// ---------------------------------------------------------------------------

fn encode_iflags(f: &ImplFlags) -> String {
    let mut parts = Vec::new();
    if f.negative {
        parts.push("negative");
    }
    if f.blanket {
        parts.push("blanket");
    }
    parts.join("\t")
}

fn decode_iflags(tokens: &[&str]) -> Result<ImplFlags, Error> {
    let mut flags = ImplFlags::default();
    for tok in tokens {
        match *tok {
            "negative" => flags.negative = true,
            "blanket" => flags.blanket = true,
            "" => {}
            _ => return Err(Error::Malformed(format!("unknown iflags token: {tok}"))),
        }
    }
    Ok(flags)
}

// ---------------------------------------------------------------------------
// RecordForm encoding
// ---------------------------------------------------------------------------

fn encode_recform(f: &RecordForm) -> &'static str {
    match f {
        RecordForm::Struct => "struct",
        RecordForm::Tuple => "tuple",
        RecordForm::Unit => "unit",
        RecordForm::Union => "union",
    }
}

fn decode_recform(s: &str) -> Result<RecordForm, Error> {
    match s {
        "struct" => Ok(RecordForm::Struct),
        "tuple" => Ok(RecordForm::Tuple),
        "unit" => Ok(RecordForm::Unit),
        "union" => Ok(RecordForm::Union),
        _ => Err(Error::Malformed(format!("unknown recform: {s}"))),
    }
}

// ---------------------------------------------------------------------------
// VariantForm encoding
// ---------------------------------------------------------------------------

fn encode_vform(f: &VariantForm) -> &'static str {
    match f {
        VariantForm::Unit => "unit",
        VariantForm::Tuple => "tuple",
        VariantForm::Struct => "struct",
    }
}

fn decode_vform(s: &str) -> Result<VariantForm, Error> {
    match s {
        "unit" => Ok(VariantForm::Unit),
        "tuple" => Ok(VariantForm::Tuple),
        "struct" => Ok(VariantForm::Struct),
        _ => Err(Error::Malformed(format!("unknown vform: {s}"))),
    }
}

// ---------------------------------------------------------------------------
// AutoFact encoding
// ---------------------------------------------------------------------------

fn encode_auto(facts: &[AutoFact]) -> Vec<String> {
    facts
        .iter()
        .map(|f| {
            let trait_str = match f.trait_ {
                AutoTrait::Send => "send",
                AutoTrait::Sync => "sync",
                AutoTrait::Unpin => "unpin",
                AutoTrait::UnwindSafe => "unwindsafe",
                AutoTrait::RefUnwindSafe => "refunwindsafe",
            };
            let state_str = match f.state {
                AutoState::Yes => "yes",
                AutoState::No => "no",
                AutoState::Cond => "cond",
            };
            format!("{trait_str}:{state_str}")
        })
        .collect()
}

fn decode_auto_line(s: &str) -> Result<AutoFact, Error> {
    let colon = s
        .find(':')
        .ok_or_else(|| Error::Malformed(format!("bad auto line: {s}")))?;
    let trait_ = match &s[..colon] {
        "send" => AutoTrait::Send,
        "sync" => AutoTrait::Sync,
        "unpin" => AutoTrait::Unpin,
        "unwindsafe" => AutoTrait::UnwindSafe,
        "refunwindsafe" => AutoTrait::RefUnwindSafe,
        other => return Err(Error::Malformed(format!("unknown auto trait: {other}"))),
    };
    let state = match &s[colon + 1..] {
        "yes" => AutoState::Yes,
        "no" => AutoState::No,
        "cond" => AutoState::Cond,
        other => return Err(Error::Malformed(format!("unknown auto state: {other}"))),
    };
    Ok(AutoFact { trait_, state })
}

// ---------------------------------------------------------------------------
// CfgExpr encoding
// ---------------------------------------------------------------------------

fn encode_cfg(e: &CfgExpr) -> String {
    match e {
        CfgExpr::All(children) => {
            let parts: Vec<_> = children.iter().map(encode_cfg).collect();
            format!("all({})", parts.join(","))
        }
        CfgExpr::Any(children) => {
            let parts: Vec<_> = children.iter().map(encode_cfg).collect();
            format!("any({})", parts.join(","))
        }
        CfgExpr::Not(child) => format!("not({})", encode_cfg(child)),
        CfgExpr::Feature(s) => format!("feature={}", escape(s)),
        CfgExpr::TargetOs(s) => format!("target_os={}", escape(s)),
        CfgExpr::TargetArch(s) => format!("target_arch={}", escape(s)),
        CfgExpr::Other(s) => format!("other:{}", escape(s)),
    }
}

fn decode_cfg(s: &str) -> Result<CfgExpr, Error> {
    if let Some(inner) = s.strip_prefix("all(").and_then(|s| s.strip_suffix(')')) {
        let children = split_cfg_args(inner)?;
        return Ok(CfgExpr::All(children.into_boxed_slice()));
    }
    if let Some(inner) = s.strip_prefix("any(").and_then(|s| s.strip_suffix(')')) {
        let children = split_cfg_args(inner)?;
        return Ok(CfgExpr::Any(children.into_boxed_slice()));
    }
    if let Some(inner) = s.strip_prefix("not(").and_then(|s| s.strip_suffix(')')) {
        return Ok(CfgExpr::Not(Box::new(decode_cfg(inner)?)));
    }
    if let Some(rest) = s.strip_prefix("feature=") {
        return Ok(CfgExpr::Feature(unescape(rest)?));
    }
    if let Some(rest) = s.strip_prefix("target_os=") {
        return Ok(CfgExpr::TargetOs(unescape(rest)?));
    }
    if let Some(rest) = s.strip_prefix("target_arch=") {
        return Ok(CfgExpr::TargetArch(unescape(rest)?));
    }
    if let Some(rest) = s.strip_prefix("other:") {
        return Ok(CfgExpr::Other(unescape(rest)?));
    }
    Err(Error::Malformed(format!("unknown cfg expr: {s}")))
}

fn split_cfg_args(s: &str) -> Result<Vec<CfgExpr>, Error> {
    // Naive split on ',' that respects balanced parens
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(decode_cfg(&s[start..i])?);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(decode_cfg(&s[start..])?);
    Ok(parts)
}

// ---------------------------------------------------------------------------
// AttrTok encoding
// ---------------------------------------------------------------------------

fn encode_attr(a: &AttrTok) -> String {
    a.arg
        .as_ref()
        .map_or_else(|| a.token.clone(), |arg| format!("{}\t{}", a.token, escape(arg)))
}

fn decode_attr(s: &str) -> Result<AttrTok, Error> {
    if let Some(tab) = s.find('\t') {
        let token = s[..tab].to_owned();
        let arg = Some(unescape(&s[tab + 1..])?);
        Ok(AttrTok { token, arg })
    } else {
        Ok(AttrTok {
            token: s.to_owned(),
            arg: None,
        })
    }
}

// ---------------------------------------------------------------------------
// StableRef encoding (for retgt and dlink)
// ---------------------------------------------------------------------------

fn decode_stable_ref(s: &str) -> Result<StableRef, Error> {
    // format: <eco><pkg>#<64hex>  (note: no '/' between eco and pkg, use '#' as anchor)
    // Actually it's F:<eco>/<pkg>#<hex> in typeref; for stable-ref in retgt/dlink
    // the wire contract reuses encode_typeref format for the Foreign variant.
    // Per §6.2: `stable-ref` = `F:<eco>/<pkg>#<64hex>` (same as typeref foreign).
    let rest = s
        .strip_prefix("F:")
        .ok_or_else(|| Error::Malformed(format!("stable-ref must start with F:: {s}")))?;
    let hash_pos = rest
        .rfind('#')
        .ok_or_else(|| Error::Malformed(format!("no '#' in stable-ref: {s}")))?;
    let intro_hex = &rest[hash_pos + 1..];
    let eco_pkg = &rest[..hash_pos];
    let slash = eco_pkg
        .find('/')
        .ok_or_else(|| Error::Malformed(format!("no '/' in stable-ref: {s}")))?;
    let eco = &eco_pkg[..slash];
    let pkg = &eco_pkg[slash + 1..];
    Ok(StableRef::new(
        PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
        IntroId::from_raw(hex_to_32(intro_hex)?),
    ))
}

fn encode_stable_ref_f(sr: &StableRef) -> String {
    format!(
        "F:{}/{}#{}",
        sr.package.ecosystem.as_str(),
        sr.package.name.as_str(),
        sr.intro.to_hex()
    )
}

// ---------------------------------------------------------------------------
// serialize_f1
// ---------------------------------------------------------------------------

/// Serialize an [`OwnedEntryPayload`] plus parent and link set to NdIrF1 bytes.
///
/// Output is deterministic: same inputs → same bytes.  Frames are emitted in
/// frozen registry order; set-class frames are sorted ascending by raw line bytes.
pub fn serialize_f1(
    payload: &OwnedEntryPayload,
    parent: Option<IntroId>,
    links: &[LinkWire],
) -> Vec<u8> {
    let sym = &payload.symbol;
    let mut out = String::new();

    // Key 0 – magic
    out.push_str(libpijul::nudox_f1::registry::MAGIC_STR);

    // Key 1 – name (E, scalar)
    out.push_str(KEY_NAME);
    out.push('\t');
    out.push_str(&escape(&sym.name));
    out.push('\n');

    // Key 2 – vis (S, scalar)
    out.push_str(KEY_VIS);
    out.push('\t');
    out.push_str(encode_vis(sym.visibility));
    out.push('\n');

    // Key 3 – kind (S, scalar)
    out.push_str(KEY_KIND);
    out.push('\t');
    out.push_str(encode_kind_token(payload.kind_disc));
    out.push('\n');

    // Key 4 – span (scalar)
    out.push_str(KEY_SPAN);
    out.push('\t');
    out.push_str(&sym.span_start.to_string());
    out.push('\t');
    out.push_str(&sym.span_end.to_string());
    out.push('\n');

    // Key 5 – src (scalar, optional)
    if !sym.source_path.is_empty() {
        out.push_str(KEY_SRC);
        out.push('\t');
        out.push_str(&escape(&sym.source_path));
        out.push('\n');
    }

    // Key 6 – parent (scalar, optional)
    if let Some(p) = parent {
        out.push_str(KEY_PARENT);
        out.push('\t');
        out.push_str(&p.to_hex());
        out.push('\n');
    }

    // Key 7 – cfg (S, scalar, optional)
    if let Some(cfg) = &sym.cfg {
        out.push_str(KEY_CFG);
        out.push('\t');
        out.push_str(&encode_cfg(cfg));
        out.push('\n');
    }

    // Key 8 – attr (S, set)
    if !sym.attrs.is_empty() {
        let mut attr_lines: Vec<String> = sym
            .attrs
            .iter()
            .map(|a| format!("{}\t{}\n", KEY_ATTR, encode_attr(a)))
            .collect();
        attr_lines.sort();
        for line in attr_lines {
            out.push_str(&line);
        }
    }

    // Key 9 – deprecated (S, scalar, optional)
    if let Some(dep) = &sym.deprecation {
        let since = dep.since.as_deref().unwrap_or("");
        let note = dep.note.as_deref().unwrap_or("");
        out.push_str(KEY_DEPRECATED);
        out.push('\t');
        out.push_str(&escape(since));
        out.push('\t');
        out.push_str(&escape(note));
        out.push('\n');
    }

    // Key 10 – alias (set, E)
    if !sym.aliases.is_empty() {
        let mut alias_lines: Vec<String> = sym
            .aliases
            .iter()
            .map(|a| format!("{}\t{}\n", KEY_ALIAS, escape(a)))
            .collect();
        alias_lines.sort();
        for line in alias_lines {
            out.push_str(&line);
        }
    }

    // Key 11 – doc (seq, E) — one paragraph per line
    if let Some(doc) = &sym.documentation {
        for para in doc.split("\n\n") {
            out.push_str(KEY_DOC);
            out.push('\t');
            out.push_str(&escape(para));
            out.push('\n');
        }
    }

    // Key 12 – dlink (set)
    if !sym.doc_links.is_empty() {
        let mut dlink_lines: Vec<String> = sym
            .doc_links
            .iter()
            .map(|dl| {
                let label = dl.label.as_deref().unwrap_or("");
                format!(
                    "{}\t{}\t{}\n",
                    KEY_DLINK,
                    encode_stable_ref_f(&dl.target),
                    escape(label)
                )
            })
            .collect();
        dlink_lines.sort();
        for line in dlink_lines {
            out.push_str(&line);
        }
    }

    // Key 13 – retgt (S, scalar) — reexport kind only
    if let KindWire::Reexport(r) = &payload.kind {
        out.push_str(KEY_RETGT);
        out.push('\t');
        out.push_str(&encode_stable_ref_f(&r.target));
        out.push('\n');
    }

    // Key 14 – fnsig (S, scalar) — functions only
    // Key 15 – gparam (S, seq) — functions, records, enums, traits, impls, type aliases
    // Key 16 – where (S, set) — same
    // Key 17 – in (S E, seq) — functions
    // Key 18 – out (S E, seq) — functions

    match &payload.kind {
        KindWire::Param(p) => {
            // A first-class Param entry encodes its (name, type) as a single
            // `in` line — same token used for function input params, so the
            // line parser is shared.
            out.push_str(KEY_IN);
            out.push('\t');
            out.push_str(&escape(p.name.as_deref().unwrap_or("")));
            out.push('\t');
            out.push_str(&encode_typeref(&p.ty));
            out.push('\n');
        }
        KindWire::Function(f) => {
            // fnsig
            out.push_str(KEY_FNSIG);
            out.push('\t');
            out.push_str(&encode_fnsig(&f.sig));
            out.push('\n');
            // gparam
            for gp in &f.generics {
                out.push_str(KEY_GPARAM);
                out.push('\t');
                out.push_str(&encode_gparam(gp));
                out.push('\n');
            }
            // where (set → sort)
            emit_where_set(&mut out, &f.wheres);
            // in
            for param in &f.input_params {
                out.push_str(KEY_IN);
                out.push('\t');
                out.push_str(&escape(param.name.as_deref().unwrap_or("")));
                out.push('\t');
                out.push_str(&encode_typeref(&param.ty));
                out.push('\n');
            }
            // out
            for param in &f.output_params {
                out.push_str(KEY_OUT);
                out.push('\t');
                out.push_str(&escape(param.name.as_deref().unwrap_or("")));
                out.push('\t');
                out.push_str(&encode_typeref(&param.ty));
                out.push('\n');
            }
        }
        KindWire::Record(r) => {
            // gparam
            for gp in &r.generics {
                out.push_str(KEY_GPARAM);
                out.push('\t');
                out.push_str(&encode_gparam(gp));
                out.push('\n');
            }
            // where
            emit_where_set(&mut out, &r.wheres);
        }
        KindWire::Trait(t) => {
            // gparam
            for gp in &t.generics {
                out.push_str(KEY_GPARAM);
                out.push('\t');
                out.push_str(&encode_gparam(gp));
                out.push('\n');
            }
            // where
            emit_where_set(&mut out, &t.wheres);
        }
        KindWire::Impl(im) => {
            // gparam
            for gp in &im.generics {
                out.push_str(KEY_GPARAM);
                out.push('\t');
                out.push_str(&encode_gparam(gp));
                out.push('\n');
            }
            // where
            emit_where_set(&mut out, &im.wheres);
        }
        KindWire::Enum(e) => {
            // gparam
            for gp in &e.generics {
                out.push_str(KEY_GPARAM);
                out.push('\t');
                out.push_str(&encode_gparam(gp));
                out.push('\n');
            }
            // where
            emit_where_set(&mut out, &e.wheres);
        }
        KindWire::Type(ta) => {
            // gparam
            for gp in &ta.generics {
                out.push_str(KEY_GPARAM);
                out.push('\t');
                out.push_str(&encode_gparam(gp));
                out.push('\n');
            }
            // where
            emit_where_set(&mut out, &ta.wheres);
        }
        _ => {}
    }

    // Key 19 – fieldty (S E, scalar) — fields only
    if let KindWire::Field(f) = &payload.kind
        && let Some(ty) = &f.ty
    {
        out.push_str(KEY_FIELDTY);
        out.push('\t');
        out.push_str(&encode_typeref(ty));
        out.push('\n');
    }

    // Key 20 – recform (S, scalar) — records
    // Key 21 – recfield (S, seq) — records
    if let KindWire::Record(r) = &payload.kind {
        out.push_str(KEY_RECFORM);
        out.push('\t');
        out.push_str(encode_recform(&r.form));
        out.push('\n');
        for field_id in &r.fields {
            out.push_str(KEY_RECFIELD);
            out.push('\t');
            out.push_str(&field_id.to_hex());
            out.push('\n');
        }
    }
    // Variant child fields are an ordered IntroId list too (§8.2 doesn't
    // distinguish them from record fields); they share key 21 and MUST be
    // emitted here in canonical order, before vform/super/link.
    if let KindWire::Variant(v) = &payload.kind {
        for field_id in &v.fields {
            out.push_str(KEY_RECFIELD);
            out.push('\t');
            out.push_str(&field_id.to_hex());
            out.push('\n');
        }
    }

    // Key 22 – vform (S, scalar) — variants
    // Key 23 – vdiscr (S, scalar, optional) — variants
    if let KindWire::Variant(v) = &payload.kind {
        out.push_str(KEY_VFORM);
        out.push('\t');
        out.push_str(encode_vform(&v.form));
        out.push('\n');
        if let Some(discr) = &v.discr {
            out.push_str(KEY_VDISCR);
            out.push('\t');
            out.push_str(&escape(discr));
            out.push('\n');
        }
    }

    // Key 24 – super (S, set) — traits
    // Key 25 – tflags (S, scalar) — traits
    if let KindWire::Trait(t) = &payload.kind {
        if !t.supers.is_empty() {
            let mut super_lines: Vec<String> = t
                .supers
                .iter()
                .map(|s| format!("{}\t{}\n", KEY_SUPER, encode_typeref(s)))
                .collect();
            super_lines.sort();
            for line in super_lines {
                out.push_str(&line);
            }
        }
        out.push_str(KEY_TFLAGS);
        out.push('\t');
        out.push_str(&encode_tflags(&t.flags));
        out.push('\n');
    }

    // Key 26 – iof (S, scalar, optional) — impls
    // Key 27 – ifor (S, scalar) — impls
    // Key 28 – iflags (S, scalar) — impls
    if let KindWire::Impl(im) = &payload.kind {
        if let Some(of) = &im.of {
            out.push_str(KEY_IOF);
            out.push('\t');
            out.push_str(&encode_typeref(of));
            out.push('\n');
        }
        out.push_str(KEY_IFOR);
        out.push('\t');
        out.push_str(&encode_typeexpr(&im.self_ty));
        out.push('\n');
        let iflags_str = encode_iflags(&im.flags);
        if !iflags_str.is_empty() {
            out.push_str(KEY_IFLAGS);
            out.push('\t');
            out.push_str(&iflags_str);
            out.push('\n');
        }
    }

    // Key 29 – cty (S, scalar) — consts/statics
    // Key 30 – cval (scalar, optional) — consts
    match &payload.kind {
        KindWire::Const(c) => {
            out.push_str(KEY_CTY);
            out.push('\t');
            out.push_str(&encode_typeref(&c.ty));
            out.push('\n');
            if let Some(val) = &c.value {
                out.push_str(KEY_CVAL);
                out.push('\t');
                out.push_str(&escape(val));
                out.push('\n');
            }
        }
        KindWire::Static(s) => {
            out.push_str(KEY_CTY);
            out.push('\t');
            out.push_str(&encode_typeref(&s.ty));
            out.push('\n');
            // statics use lfact for mutability flag (not a core key)
        }
        _ => {}
    }

    // Key 31 – auto (S, set) — records/enums/types
    match &payload.kind {
        KindWire::Record(r) => emit_auto_set(&mut out, &r.auto),
        KindWire::Enum(e) => emit_auto_set(&mut out, &e.auto),
        KindWire::Type(ta) => emit_auto_set(&mut out, &ta.auto),
        _ => {}
    }

    // Key 32 – type (S E, scalar) — type aliases
    if let KindWire::Type(ta) = &payload.kind {
        out.push_str(KEY_TYPE);
        out.push('\t');
        out.push_str(&encode_typeexpr(&ta.ty));
        out.push('\n');
    }

    // Key 33 – link (set)
    if !links.is_empty() {
        let link_lines: Vec<String> = links
            .iter()
            .map(|l| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    KEY_LINK,
                    l.other.package.ecosystem.as_str(),
                    l.other.package.name.as_str(),
                    l.other.intro.to_hex(),
                    encode_kind_token(l.kind_self),
                    encode_kind_token(l.kind_other),
                )
            })
            .collect();
        let mut sorted_links = link_lines;
        sorted_links.sort();
        for line in sorted_links {
            out.push_str(&line);
        }
    }

    // Key 34 – lfact (S, set) — ecosystem-scoped language facts
    // For static mutability we emit it here
    if let KindWire::Static(s) = &payload.kind
        && s.mutable
    {
        out.push_str(KEY_LFACT);
        out.push_str("\tmutable\n");
    }
    // (Variant child fields are emitted with the record fields at key 21 above.)

    // Enum variants list
    if let KindWire::Enum(e) = &payload.kind {
        for var_id in &e.variants {
            // Per §8.2, enum variant order stored as vdiscr's sibling.
            // The plan lists `recfield` for records and has no separate "variantfield".
            // Using lfact with a special prefix to convey ordered variant list.
            out.push_str(KEY_LFACT);
            out.push_str("\tvariant:");
            out.push_str(&var_id.to_hex());
            out.push('\n');
        }
    }

    out.into_bytes()
}

fn emit_where_set(out: &mut String, wheres: &[WherePredWire]) {
    if wheres.is_empty() {
        return;
    }
    let mut where_lines: Vec<String> = wheres
        .iter()
        .map(|wp| format!("{}\t{}\n", KEY_WHERE, encode_where(wp)))
        .collect();
    where_lines.sort();
    for line in where_lines {
        out.push_str(&line);
    }
}

fn emit_auto_set(out: &mut String, facts: &[AutoFact]) {
    if facts.is_empty() {
        return;
    }
    let mut auto_lines: Vec<String> = encode_auto(facts)
        .into_iter()
        .map(|s| format!("{KEY_AUTO}\t{s}\n"))
        .collect();
    auto_lines.sort();
    for line in auto_lines {
        out.push_str(&line);
    }
}

// ---------------------------------------------------------------------------
// F1View — borrowed parse result
// ---------------------------------------------------------------------------

/// Borrowed view into F1-format bytes. No content is copied during parsing;
/// strings are borrowed slices from the original buffer.
pub struct F1View<'a> {
    bytes: &'a [u8],
    // Header scalars
    name: &'a str,
    vis: Visibility,
    kind_disc: KindDiscriminant,
    span_start: u32,
    span_end: u32,
    // Optional scalars
    src: Option<&'a str>,
    cfg_raw: Option<&'a str>,
    deprecated_raw: Option<&'a str>,
    retgt_raw: Option<&'a str>,
    fnsig_raw: Option<&'a str>,
    fieldty_raw: Option<&'a str>,
    recform_raw: Option<&'a str>,
    vform_raw: Option<&'a str>,
    vdiscr_raw: Option<&'a str>,
    tflags_raw: Option<&'a str>,
    iof_raw: Option<&'a str>,
    ifor_raw: Option<&'a str>,
    iflags_raw: Option<&'a str>,
    cty_raw: Option<&'a str>,
    cval_raw: Option<&'a str>,
    type_raw: Option<&'a str>,
    // Sets / seqs (raw value slices)
    attr_lines: Vec<&'a str>,
    alias_lines: Vec<&'a str>,
    doc_lines: Vec<&'a str>,
    dlink_lines: Vec<&'a str>,
    gparam_lines: Vec<&'a str>,
    where_lines: Vec<&'a str>,
    in_lines: Vec<&'a str>,
    out_lines: Vec<&'a str>,
    recfield_lines: Vec<&'a str>,
    super_lines: Vec<&'a str>,
    auto_lines: Vec<&'a str>,
    lfact_lines: Vec<&'a str>,
    // Parsed parent + links for quick access
    parent_id: Option<IntroId>,
    parsed_links: Vec<LinkWire>,
}

/// Registry key order index (for out-of-order detection).
fn key_order_index(key: &str) -> Option<u8> {
    match key {
        KEY_NAME => Some(1),
        KEY_VIS => Some(2),
        KEY_KIND => Some(3),
        KEY_SPAN => Some(4),
        KEY_SRC => Some(5),
        KEY_PARENT => Some(6),
        KEY_CFG => Some(7),
        KEY_ATTR => Some(8),
        KEY_DEPRECATED => Some(9),
        KEY_ALIAS => Some(10),
        KEY_DOC => Some(11),
        KEY_DLINK => Some(12),
        KEY_RETGT => Some(13),
        KEY_FNSIG => Some(14),
        KEY_GPARAM => Some(15),
        KEY_WHERE => Some(16),
        KEY_IN => Some(17),
        KEY_OUT => Some(18),
        KEY_FIELDTY => Some(19),
        KEY_RECFORM => Some(20),
        KEY_RECFIELD => Some(21),
        KEY_VFORM => Some(22),
        KEY_VDISCR => Some(23),
        KEY_SUPER => Some(24),
        KEY_TFLAGS => Some(25),
        KEY_IOF => Some(26),
        KEY_IFOR => Some(27),
        KEY_IFLAGS => Some(28),
        KEY_CTY => Some(29),
        KEY_CVAL => Some(30),
        KEY_AUTO => Some(31),
        KEY_TYPE => Some(32),
        KEY_LINK => Some(33),
        KEY_LFACT => Some(34),
        _ => None,
    }
}

impl<'a> F1View<'a> {
    /// Parse F1 bytes strictly.  Unknown key → error.  Wrong order → error.
    /// Unsorted set → error.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<F1View<'a>, Error> {
        let text = std::str::from_utf8(bytes)?;
        let mut lines = text.split('\n');

        // Magic line
        let first = lines.next().ok_or(Error::BadMagic)?;
        {
            let mut parts = first.splitn(2, '\t');
            if parts.next() != Some("NdIrF1") {
                return Err(Error::BadMagic);
            }
            let ver_str = parts.next().unwrap_or("");
            let ver: u16 = ver_str
                .parse()
                .map_err(|_| Error::Malformed(format!("bad version: {ver_str}")))?;
            if ver != 1 {
                return Err(Error::UnsupportedVersion(ver));
            }
        }

        // Parse remaining lines
        let mut name: Option<&'a str> = None;
        let mut vis: Option<Visibility> = None;
        let mut kind_disc: Option<KindDiscriminant> = None;
        let mut span_start: Option<u32> = None;
        let mut span_end: Option<u32> = None;
        let mut src: Option<&'a str> = None;
        let mut parent_hex: Option<&'a str> = None;
        let mut cfg_raw: Option<&'a str> = None;
        let mut deprecated_raw: Option<&'a str> = None;
        let mut retgt_raw: Option<&'a str> = None;
        let mut fnsig_raw: Option<&'a str> = None;
        let mut fieldty_raw: Option<&'a str> = None;
        let mut recform_raw: Option<&'a str> = None;
        let mut vform_raw: Option<&'a str> = None;
        let mut vdiscr_raw: Option<&'a str> = None;
        let mut tflags_raw: Option<&'a str> = None;
        let mut iof_raw: Option<&'a str> = None;
        let mut ifor_raw: Option<&'a str> = None;
        let mut iflags_raw: Option<&'a str> = None;
        let mut cty_raw: Option<&'a str> = None;
        let mut cval_raw: Option<&'a str> = None;
        let mut type_raw: Option<&'a str> = None;

        let mut attr_lines: Vec<&'a str> = Vec::new();
        let mut alias_lines: Vec<&'a str> = Vec::new();
        let mut doc_lines: Vec<&'a str> = Vec::new();
        let mut dlink_lines: Vec<&'a str> = Vec::new();
        let mut gparam_lines: Vec<&'a str> = Vec::new();
        let mut where_lines: Vec<&'a str> = Vec::new();
        let mut in_lines: Vec<&'a str> = Vec::new();
        let mut out_lines: Vec<&'a str> = Vec::new();
        let mut recfield_lines: Vec<&'a str> = Vec::new();
        let mut super_lines: Vec<&'a str> = Vec::new();
        let mut auto_lines: Vec<&'a str> = Vec::new();
        let mut link_lines: Vec<&'a str> = Vec::new();
        let mut lfact_lines: Vec<&'a str> = Vec::new();

        let mut last_key_order: u8 = 0;
        // Track last value for each set key for sorted-set validation
        let mut last_attr: Option<&'a str> = None;
        let mut last_alias: Option<&'a str> = None;
        let mut last_dlink: Option<&'a str> = None;
        let mut last_where: Option<&'a str> = None;
        let mut last_super: Option<&'a str> = None;
        let mut last_auto: Option<&'a str> = None;
        let mut last_link: Option<&'a str> = None;
        let mut last_lfact: Option<&'a str> = None;

        for raw_line in lines {
            if raw_line.is_empty() {
                continue; // trailing newline
            }
            let tab = raw_line.find('\t').ok_or_else(|| {
                Error::Malformed(format!(
                    "line has no TAB: {}",
                    &raw_line[..raw_line.len().min(40)]
                ))
            })?;
            let key = &raw_line[..tab];
            let val = &raw_line[tab + 1..];

            let order = key_order_index(key).ok_or_else(|| Error::UnknownKey(key.to_owned()))?;

            // Section order: key order index must be ≥ last (same key repeated is fine for sets/seqs)
            if order < last_key_order {
                return Err(Error::OutOfOrder(key.to_owned()));
            }
            last_key_order = order;

            // Helper macro for sorted-set validation
            macro_rules! check_sorted {
                ($last:expr, $val:expr, $key:expr) => {
                    if let Some(prev) = $last {
                        if $val < prev {
                            return Err(Error::UnsortedSet(
                                $key.to_owned(),
                                prev.to_owned(),
                                $val.to_owned(),
                            ));
                        }
                    }
                    $last = Some($val);
                };
            }

            match key {
                KEY_NAME => name = Some(val),
                KEY_VIS => vis = Some(decode_vis(val)?),
                KEY_KIND => kind_disc = Some(decode_kind_token(val)?),
                KEY_SPAN => {
                    let mut parts = val.splitn(2, '\t');
                    let s_str = parts
                        .next()
                        .ok_or_else(|| Error::Malformed("span missing start".into()))?;
                    let e_str = parts
                        .next()
                        .ok_or_else(|| Error::Malformed("span missing end".into()))?;
                    span_start = Some(
                        s_str
                            .parse()
                            .map_err(|_| Error::Malformed(format!("bad span: {val}")))?,
                    );
                    span_end = Some(
                        e_str
                            .parse()
                            .map_err(|_| Error::Malformed(format!("bad span: {val}")))?,
                    );
                }
                KEY_SRC => src = Some(val),
                KEY_PARENT => parent_hex = Some(val),
                KEY_CFG => cfg_raw = Some(val),
                KEY_ATTR => {
                    check_sorted!(last_attr, val, KEY_ATTR);
                    attr_lines.push(val);
                }
                KEY_DEPRECATED => deprecated_raw = Some(val),
                KEY_ALIAS => {
                    check_sorted!(last_alias, val, KEY_ALIAS);
                    alias_lines.push(val);
                }
                KEY_DOC => doc_lines.push(val),
                KEY_DLINK => {
                    check_sorted!(last_dlink, val, KEY_DLINK);
                    dlink_lines.push(val);
                }
                KEY_RETGT => retgt_raw = Some(val),
                KEY_FNSIG => fnsig_raw = Some(val),
                KEY_GPARAM => gparam_lines.push(val),
                KEY_WHERE => {
                    check_sorted!(last_where, val, KEY_WHERE);
                    where_lines.push(val);
                }
                KEY_IN => in_lines.push(val),
                KEY_OUT => out_lines.push(val),
                KEY_FIELDTY => fieldty_raw = Some(val),
                KEY_RECFORM => recform_raw = Some(val),
                KEY_RECFIELD => recfield_lines.push(val),
                KEY_VFORM => vform_raw = Some(val),
                KEY_VDISCR => vdiscr_raw = Some(val),
                KEY_SUPER => {
                    check_sorted!(last_super, val, KEY_SUPER);
                    super_lines.push(val);
                }
                KEY_TFLAGS => tflags_raw = Some(val),
                KEY_IOF => iof_raw = Some(val),
                KEY_IFOR => ifor_raw = Some(val),
                KEY_IFLAGS => iflags_raw = Some(val),
                KEY_CTY => cty_raw = Some(val),
                KEY_CVAL => cval_raw = Some(val),
                KEY_AUTO => {
                    check_sorted!(last_auto, val, KEY_AUTO);
                    auto_lines.push(val);
                }
                KEY_TYPE => type_raw = Some(val),
                KEY_LINK => {
                    check_sorted!(last_link, val, KEY_LINK);
                    link_lines.push(val);
                }
                KEY_LFACT => {
                    check_sorted!(last_lfact, val, KEY_LFACT);
                    lfact_lines.push(val);
                }
                _ => return Err(Error::UnknownKey(key.to_owned())),
            }
        }

        // Required fields
        let name = name.ok_or_else(|| Error::MissingField("name".into()))?;
        let vis = vis.ok_or_else(|| Error::MissingField("vis".into()))?;
        let kind_disc = kind_disc.ok_or_else(|| Error::MissingField("kind".into()))?;
        let span_start = span_start.ok_or_else(|| Error::MissingField("span".into()))?;
        let span_end = span_end.ok_or_else(|| Error::MissingField("span".into()))?;

        // Parse parent
        let parent_id = parent_hex
            .map(|h| Ok::<_, Error>(IntroId::from_raw(hex_to_32(h)?)))
            .transpose()?;

        // Parse links
        let parsed_links = link_lines
            .iter()
            .map(|line| parse_link_line(line))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(F1View {
            bytes,
            name,
            vis,
            kind_disc,
            span_start,
            span_end,
            src,
            cfg_raw,
            deprecated_raw,
            retgt_raw,
            fnsig_raw,
            fieldty_raw,
            recform_raw,
            vform_raw,
            vdiscr_raw,
            tflags_raw,
            iof_raw,
            ifor_raw,
            iflags_raw,
            cty_raw,
            cval_raw,
            type_raw,
            attr_lines,
            alias_lines,
            doc_lines,
            dlink_lines,
            gparam_lines,
            where_lines,
            in_lines,
            out_lines,
            recfield_lines,
            super_lines,
            auto_lines,
            lfact_lines,
            parent_id,
            parsed_links,
        })
    }

    pub fn parent(&self) -> Option<IntroId> {
        self.parent_id
    }

    /// The raw (still-escaped) symbol name frame. For canonical simple names this
    /// equals the decoded name; call [`F1View::to_owned_payload`] for the fully
    /// unescaped form.
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn links(&self) -> &[LinkWire] {
        &self.parsed_links
    }

    pub fn kind_disc(&self) -> KindDiscriminant {
        self.kind_disc
    }

    pub fn raw_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Reconstruct a full [`OwnedEntryPayload`] from the view.
    pub fn to_owned_payload(&self) -> Result<OwnedEntryPayload, Error> {
        let name = unescape(self.name)?;
        let source_path = self.src.map(unescape).transpose()?.unwrap_or_default();

        // Documentation: rejoin paragraphs with double newline
        let documentation = if self.doc_lines.is_empty() {
            None
        } else {
            let paras: Result<Vec<_>, _> = self.doc_lines.iter().map(|s| unescape(s)).collect();
            Some(paras?.join("\n\n"))
        };

        let mut aliases: Vec<String> = self
            .alias_lines
            .iter()
            .map(|s| unescape(s))
            .collect::<Result<_, _>>()?;
        aliases.sort();

        let deprecation = if let Some(dep_raw) = self.deprecated_raw {
            let mut parts = dep_raw.splitn(2, '\t');
            let since_raw = parts.next().unwrap_or("");
            let note_raw = parts.next().unwrap_or("");
            let since = unescape(since_raw)?;
            let note = unescape(note_raw)?;
            Some(DeprecationWire {
                since: if since.is_empty() { None } else { Some(since) },
                note: if note.is_empty() { None } else { Some(note) },
            })
        } else {
            None
        };

        let doc_links: Result<Vec<_>, _> = self
            .dlink_lines
            .iter()
            .map(|s| {
                // format: <stable-ref>TAB<escaped label>
                let tab = s.find('\t').unwrap_or(s.len());
                let target = decode_stable_ref(&s[..tab])?;
                let label_raw = if tab < s.len() { &s[tab + 1..] } else { "" };
                let label = unescape(label_raw)?;
                Ok::<_, Error>(DocLinkWire {
                    target,
                    label: if label.is_empty() { None } else { Some(label) },
                })
            })
            .collect();
        let doc_links = doc_links?;

        let attrs: Result<Vec<_>, _> = self.attr_lines.iter().map(|s| decode_attr(s)).collect();
        let attrs = attrs?;

        let cfg = self.cfg_raw.map(decode_cfg).transpose()?;

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
            attrs,
            cfg,
        };

        let kind = self.reconstruct_kind()?;
        let flags = crate::wire::EntryPayloadFlags::default();

        let payload = OwnedEntryPayload::sealed(sym, self.kind_disc, kind, flags);
        Ok(payload)
    }

    fn reconstruct_kind(&self) -> Result<KindWire, Error> {
        match self.kind_disc {
            KindDiscriminant::Module => Ok(KindWire::Module(ModuleWire {})),

            KindDiscriminant::Record => {
                let form = self
                    .recform_raw
                    .map(decode_recform)
                    .transpose()?
                    .unwrap_or(RecordForm::Struct);
                let fields: Result<Vec<_>, _> = self
                    .recfield_lines
                    .iter()
                    .map(|h| Ok::<_, Error>(IntroId::from_raw(hex_to_32(h)?)))
                    .collect();
                let generics = self.parse_gparams()?;
                let wheres = self.parse_wheres()?;
                let auto = self.parse_auto()?;
                Ok(KindWire::Record(RecordWire {
                    form,
                    fields: fields?.into_boxed_slice(),
                    generics,
                    wheres,
                    auto,
                }))
            }

            KindDiscriminant::Field => {
                let ty = self.fieldty_raw.map(decode_typeref).transpose()?;
                Ok(KindWire::Field(FieldWire { ty }))
            }

            KindDiscriminant::Function => {
                let sig = self
                    .fnsig_raw
                    .map(|s| {
                        let tokens: Vec<&str> = s.split('\t').collect();
                        decode_fnsig(&tokens)
                    })
                    .transpose()?
                    .unwrap_or_default();
                let generics = self.parse_gparams()?;
                let wheres = self.parse_wheres()?;
                let input_params: Result<Vec<_>, _> =
                    self.in_lines.iter().map(|s| parse_param_line(s)).collect();
                let output_params: Result<Vec<_>, _> =
                    self.out_lines.iter().map(|s| parse_param_line(s)).collect();
                Ok(KindWire::Function(FunctionWire {
                    input_params: input_params?.into_boxed_slice(),
                    output_params: output_params?.into_boxed_slice(),
                    sig,
                    generics,
                    wheres,
                }))
            }

            KindDiscriminant::Alias => {
                let ty = self
                    .type_raw
                    .map(decode_typeexpr)
                    .transpose()?
                    .unwrap_or(TypeWire::Any);
                let generics = self.parse_gparams()?;
                let wheres = self.parse_wheres()?;
                let auto = self.parse_auto()?;
                Ok(KindWire::Type(TypeAliasWire {
                    ty,
                    generics,
                    wheres,
                    auto,
                }))
            }

            KindDiscriminant::Trait => {
                let supers: Result<Vec<_>, _> =
                    self.super_lines.iter().map(|s| decode_typeref(s)).collect();
                let flags = self
                    .tflags_raw
                    .map(|s| {
                        let tokens: Vec<&str> = s.split('\t').collect();
                        decode_tflags(&tokens)
                    })
                    .transpose()?
                    .unwrap_or_default();
                let generics = self.parse_gparams()?;
                let wheres = self.parse_wheres()?;
                Ok(KindWire::Trait(TraitWire {
                    supers: supers?.into_boxed_slice(),
                    flags,
                    generics,
                    wheres,
                }))
            }

            KindDiscriminant::Impl => {
                let of = self.iof_raw.map(decode_typeref).transpose()?;
                let self_ty = self
                    .ifor_raw
                    .map(decode_typeexpr)
                    .transpose()?
                    .unwrap_or(TypeWire::Any);
                let flags = self
                    .iflags_raw
                    .map(|s| {
                        let tokens: Vec<&str> = s.split('\t').collect();
                        decode_iflags(&tokens)
                    })
                    .transpose()?
                    .unwrap_or_default();
                let generics = self.parse_gparams()?;
                let wheres = self.parse_wheres()?;
                Ok(KindWire::Impl(ImplWire {
                    of,
                    self_ty,
                    flags,
                    generics,
                    wheres,
                }))
            }

            KindDiscriminant::Enum => {
                // Variants are stored as lfact lines: "variant:<hex>"
                let variants: Result<Vec<_>, _> = self
                    .lfact_lines
                    .iter()
                    .filter_map(|s| s.strip_prefix("variant:"))
                    .map(|h| Ok::<_, Error>(IntroId::from_raw(hex_to_32(h)?)))
                    .collect();
                let generics = self.parse_gparams()?;
                let wheres = self.parse_wheres()?;
                let auto = self.parse_auto()?;
                Ok(KindWire::Enum(EnumWire {
                    variants: variants?.into_boxed_slice(),
                    generics,
                    wheres,
                    auto,
                }))
            }

            KindDiscriminant::Variant => {
                let form = self
                    .vform_raw
                    .map(decode_vform)
                    .transpose()?
                    .unwrap_or(VariantForm::Unit);
                let discr = self.vdiscr_raw.map(unescape).transpose()?;
                // Variant fields stored as recfield lines
                let fields: Result<Vec<_>, _> = self
                    .recfield_lines
                    .iter()
                    .map(|h| Ok::<_, Error>(IntroId::from_raw(hex_to_32(h)?)))
                    .collect();
                Ok(KindWire::Variant(VariantWire {
                    form,
                    discr,
                    fields: fields?.into_boxed_slice(),
                }))
            }

            KindDiscriminant::Const => {
                let ty = self
                    .cty_raw
                    .map(decode_typeref)
                    .transpose()?
                    .ok_or_else(|| Error::MissingField("cty".into()))?;
                let value = self.cval_raw.map(unescape).transpose()?;
                Ok(KindWire::Const(ConstWire { ty, value }))
            }

            KindDiscriminant::Static => {
                let ty = self
                    .cty_raw
                    .map(decode_typeref)
                    .transpose()?
                    .ok_or_else(|| Error::MissingField("cty (static)".into()))?;
                let mutable = self.lfact_lines.contains(&"mutable");
                Ok(KindWire::Static(StaticWire { ty, mutable }))
            }

            KindDiscriminant::Reexport => {
                let target = self
                    .retgt_raw
                    .map(decode_stable_ref)
                    .transpose()?
                    .ok_or_else(|| Error::MissingField("retgt".into()))?;
                Ok(KindWire::Reexport(ReexportWire { target }))
            }

            KindDiscriminant::Param => {
                // A first-class Param entry stores its type in the first `in`
                // line (same format as function param lines: <name_esc>TAB<typeref>).
                let pw = self
                    .in_lines
                    .first()
                    .ok_or_else(|| Error::MissingField("in (param)".into()))
                    .and_then(|s| parse_param_line(s))?;
                Ok(KindWire::Param(pw))
            }
        }
    }

    fn parse_gparams(&self) -> Result<Box<[GenericParamWire]>, Error> {
        self.gparam_lines
            .iter()
            .map(|s| decode_gparam(s))
            .collect::<Result<Vec<_>, _>>()
            .map(std::vec::Vec::into_boxed_slice)
    }

    fn parse_wheres(&self) -> Result<Box<[WherePredWire]>, Error> {
        self.where_lines
            .iter()
            .map(|s| decode_where(s))
            .collect::<Result<Vec<_>, _>>()
            .map(std::vec::Vec::into_boxed_slice)
    }

    fn parse_auto(&self) -> Result<Box<[AutoFact]>, Error> {
        self.auto_lines
            .iter()
            .map(|s| decode_auto_line(s))
            .collect::<Result<Vec<_>, _>>()
            .map(std::vec::Vec::into_boxed_slice)
    }
}

// ---------------------------------------------------------------------------
// Link line parser
// ---------------------------------------------------------------------------

fn parse_link_line(rest: &str) -> Result<LinkWire, Error> {
    // format: <eco>TAB<pkg>TAB<64hex>TAB<kind_self>TAB<kind_other>
    let mut parts = rest.splitn(6, '\t');
    let eco = parts
        .next()
        .ok_or_else(|| Error::Malformed(format!("link missing eco: {rest}")))?;
    let pkg = parts
        .next()
        .ok_or_else(|| Error::Malformed(format!("link missing pkg: {rest}")))?;
    let intro_hex = parts
        .next()
        .ok_or_else(|| Error::Malformed(format!("link missing intro: {rest}")))?;
    let ks_str = parts
        .next()
        .ok_or_else(|| Error::Malformed(format!("link missing kind_self: {rest}")))?;
    let ko_str = parts
        .next()
        .ok_or_else(|| Error::Malformed(format!("link missing kind_other: {rest}")))?;
    Ok(LinkWire {
        other: StableRef::new(
            PackageLineageId::new(EcosystemId::new(eco), PackageName::new(pkg)),
            IntroId::from_raw(hex_to_32(intro_hex)?),
        ),
        kind_self: decode_kind_token(ks_str)?,
        kind_other: decode_kind_token(ko_str)?,
    })
}

// ---------------------------------------------------------------------------
// Param line parser
// ---------------------------------------------------------------------------

fn parse_param_line(rest: &str) -> Result<ParamWire, Error> {
    // format: <escaped_name>TAB<typeref>
    let tab = rest
        .find('\t')
        .ok_or_else(|| Error::Malformed(format!("param line missing TAB: {rest}")))?;
    let name_raw = &rest[..tab];
    let tr_str = &rest[tab + 1..];
    let name = unescape(name_raw)?;
    let ty = decode_typeref(tr_str)?;
    Ok(ParamWire {
        name: if name.is_empty() { None } else { Some(name) },
        ty,
    })
}

// ---------------------------------------------------------------------------
// Shape hash (for continuity matcher candidate gen — §5.3 index 2)
//
// api_surface_hash domain: "nudox.apisurface.v1"
// Preimage: the F1 S-frames (§3.3) in canonical order, EXCLUDING name (key 1)
// and parent (key 6), because those are moniker-class identifiers.
// We compute this by serializing only the S-column frames.
// ---------------------------------------------------------------------------

/// Compute the `api_surface_hash` for a payload.
///
/// This is blake3("nudox.apisurface.v1" ‖ canonical_s_frames) where S-frames
/// are keys 2,3,7,8,9,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,31,32,34
/// in registry order, with name and parent excluded.
pub fn compute_api_surface_hash(payload: &OwnedEntryPayload) -> ContentBlake3 {
    // Serialize only S-frame lines into a temporary buffer (no parent, no name, no links)
    let full = serialize_f1(payload, None, &[]);
    let text = std::str::from_utf8(&full).expect("f1 is always valid utf-8");
    // Filter to only S-column keys (skip name=1, parent=6, span=4, src=5, doc=11, dlink=12, cval=30, link=33)
    const NON_S_KEYS: &[&str] = &[
        "NdIrF1", KEY_NAME, KEY_SPAN, KEY_SRC, KEY_PARENT, KEY_DOC, KEY_DLINK, KEY_CVAL, KEY_LINK,
    ];
    let mut s_frames = String::new();
    for line in text.lines() {
        let key = line.split('\t').next().unwrap_or("");
        if !NON_S_KEYS.contains(&key) {
            s_frames.push_str(line);
            s_frames.push('\n');
        }
    }
    ContentBlake3::from_domain("nudox.apisurface.v1", s_frames.as_bytes())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
