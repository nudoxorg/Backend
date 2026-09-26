//! Zero-allocation display lanes over the condensed semantic IR.
//!
//! These displays compose existing typed lanes at formatting time and never
//! create an intermediate `String`; they remain write-side projections with
//! no typed failure channel. Callers that need validated, failure-typed
//! output use the prepared semantic-document or canonical-type rendering in
//! this module, which validates every reference before caller output is
//! touched.
//!
//! The `Display` wrappers write directly into any formatter. Callers that want
//! an owned string may use the standard `ToString`; hot paths can stream into
//! an existing `fmt::Write` buffer without an intermediate allocation.

use core::{fmt, str};

#[path = "display/types.rs"]
mod types;

use types::{write_function_tail, write_type};

use crate::ir::{
    BuiltinType, ConcreteType, DocFragment, EntityId, Ir, ItemKind, ItemView, LinkKind, LinkTarget,
    TypeExpr, TypeId, Visibility,
};

const MAX_TYPE_DEPTH: u8 = 96;

/// A declaration signature rendered in familiar docs.rs/Rustdoc syntax.
pub struct SignatureDisplay<'ir> {
    ir: &'ir Ir,
    item: ItemView<'ir>,
}
/// A semantic type rendered in familiar source syntax.
pub struct TypeDisplay<'ir> {
    ir: &'ir Ir,
    ty: TypeId,
}
/// Interned documentation rendered as Markdown suitable for rustdoc/docs.rs.
pub struct DocsDisplay<'ir> {
    ir: &'ir Ir,
    item: ItemView<'ir>,
}
/// Selects semantic lanes streamed into an embedding tokenizer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmbeddingProfile(u8);

impl EmbeddingProfile {
    /// Signature only, useful for symbol-name embeddings.
    pub const SYMBOL: Self = Self(0);
    /// Signature plus documentation.
    pub const DOCUMENTED: Self = Self(1);
    /// Signature, documentation, and typed outgoing graph context.
    pub const CONTEXTUAL: Self = Self(3);

    const fn docs(self) -> bool {
        self.0 & 1 != 0
    }

    const fn graph(self) -> bool {
        self.0 & 2 != 0
    }
}

/// Allocation-free semantic text stream for vector embedding.
///
/// This composes existing typed lanes at formatting time and never creates an
/// intermediate `String`; tokenizers can implement `fmt::Write` and consume it
/// directly.
pub struct EmbeddingDisplay<'ir> {
    ir: &'ir Ir,
    item: ItemView<'ir>,
    profile: EmbeddingProfile,
}

impl Ir {
    /// Returns a zero-allocation signature renderer.
    #[must_use]
    #[doc(hidden)]
    pub fn signature(&self, item: EntityId) -> Option<SignatureDisplay<'_>> {
        self.item(item)
            .map(|item| SignatureDisplay { ir: self, item })
    }

    /// Returns a zero-allocation semantic-type renderer.
    #[must_use]
    #[doc(hidden)]
    pub fn display_type(&self, ty: TypeId) -> Option<TypeDisplay<'_>> {
        self.ty(ty).map(|_| TypeDisplay { ir: self, ty })
    }

    /// Returns a zero-allocation Markdown documentation renderer.
    #[must_use]
    #[doc(hidden)]
    pub fn display_docs(&self, item: EntityId) -> Option<DocsDisplay<'_>> {
        self.item(item).map(|item| DocsDisplay { ir: self, item })
    }

    /// Returns a direct semantic stream suitable for vector tokenization.
    #[must_use]
    #[doc(hidden)]
    pub fn embedding_text(
        &self,
        item: EntityId,
        profile: EmbeddingProfile,
    ) -> Option<EmbeddingDisplay<'_>> {
        self.item(item).map(|item| EmbeddingDisplay {
            ir: self,
            item,
            profile,
        })
    }
}

impl fmt::Display for SignatureDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_visibility(formatter, self.item.visibility())?;
        if self.item.kind() == ItemKind::Reexport {
            formatter.write_str("use ")?;
            write_reexport_target(formatter, self.ir, self.item)?;
            return Ok(());
        }
        match self.item.kind() {
            ItemKind::Module => formatter.write_str("mod ")?,
            ItemKind::Record => formatter.write_str("struct ")?,
            ItemKind::Field | ItemKind::Parameter | ItemKind::Variant => {}
            ItemKind::Function => formatter.write_str("fn ")?,
            ItemKind::TypeAlias => formatter.write_str("type ")?,
            ItemKind::Trait => formatter.write_str("trait ")?,
            ItemKind::Implementation => formatter.write_str("impl ")?,
            ItemKind::Enum => formatter.write_str("enum ")?,
            ItemKind::Constant => formatter.write_str("const ")?,
            ItemKind::Static => formatter.write_str("static ")?,
            ItemKind::Reexport => unreachable!("handled above"),
            ItemKind::Macro => formatter.write_str("macro ")?,
            ItemKind::Namespace => formatter.write_str("namespace ")?,
        }
        write_atom(formatter, self.item.name())?;
        if let Some(ty) = self.item.semantic_type() {
            match (self.item.kind(), self.ir.ty(ty)) {
                (
                    ItemKind::Function,
                    Some(TypeExpr::Concrete(ConcreteType::Function {
                        parameters,
                        results,
                        variadic,
                        unsafe_,
                        abi,
                    })),
                ) => {
                    write_function_tail(
                        formatter, self.ir, parameters, results, variadic, unsafe_, abi, 0,
                    )?;
                }
                (ItemKind::TypeAlias | ItemKind::Implementation, _) => {
                    formatter.write_str(" = ")?;
                    write_type(formatter, self.ir, ty, 0)?;
                }
                (
                    ItemKind::Record
                    | ItemKind::Enum
                    | ItemKind::Trait
                    | ItemKind::Module
                    | ItemKind::Namespace,
                    _,
                ) => {}
                _ => {
                    formatter.write_str(": ")?;
                    write_type(formatter, self.ir, ty, 0)?;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for EmbeddingDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        SignatureDisplay {
            ir: self.ir,
            item: self.item,
        }
        .fmt(formatter)?;
        if self.profile.docs() && !self.item.docs().is_empty() {
            formatter.write_str("\n\n")?;
            DocsDisplay {
                ir: self.ir,
                item: self.item,
            }
            .fmt(formatter)?;
        }
        if self.profile.graph() {
            for (_, link) in self.item.links_from() {
                formatter.write_str("\n")?;
                formatter.write_str(link_kind_name(link.kind))?;
                formatter.write_str(": ")?;
                write_embedding_target(formatter, self.ir, link.target)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_type(formatter, self.ir, self.ty, 0)
    }
}

impl fmt::Display for DocsDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for fragment in self.item.docs() {
            match *fragment {
                DocFragment::Text(text) => {
                    formatter.write_str(self.ir.text(text).unwrap_or("�"))?
                }
                DocFragment::Code(text) => {
                    formatter.write_str("`")?;
                    formatter.write_str(self.ir.text(text).unwrap_or("�"))?;
                    formatter.write_str("`")?;
                }
                DocFragment::Link { label, target } => {
                    formatter.write_str("[")?;
                    formatter.write_str(self.ir.text(label).unwrap_or("�"))?;
                    formatter.write_str("](")?;
                    write_link_target(formatter, self.ir, target)?;
                    formatter.write_str(")")?;
                }
                DocFragment::SoftBreak => formatter.write_str(" ")?,
                DocFragment::HardBreak => formatter.write_str("\n\n")?,
            }
        }
        Ok(())
    }
}

fn write_visibility(output: &mut impl fmt::Write, visibility: Visibility) -> fmt::Result {
    match visibility {
        Visibility::Unknown => Ok(()),
        Visibility::Private => Ok(()),
        Visibility::Restricted => output.write_str("pub(restricted) "),
        Visibility::Package => output.write_str("pub(crate) "),
        Visibility::Public => output.write_str("pub "),
    }
}


fn write_link_target(output: &mut impl fmt::Write, ir: &Ir, target: LinkTarget) -> fmt::Result {
    match target {
        LinkTarget::Local(entity) => match ir.item(entity) {
            Some(item) => {
                output.write_str(docs_prefix(item.kind()))?;
                write_atom(output, item.name())?;
                output.write_str(".html")
            }
            None => output.write_str("#dangling"),
        },
        LinkTarget::External(external) => match ir.external(external).and_then(external_path) {
            Some(path) => write_atom(output, ir.atom(path).unwrap_or(b"#unresolved")),
            None => output.write_str("#unresolved"),
        },
    }
}

/// A re-export's target is the fact the entry holds. A foreign path is that
/// target; a local alias keeps the name, which is the only path this entry has.
fn write_reexport_target(
    output: &mut impl fmt::Write,
    ir: &Ir,
    item: ItemView<'_>,
) -> fmt::Result {
    if let Some((_, link)) = item
        .links_from()
        .find(|(_, link)| link.kind == LinkKind::Reexports)
    {
        write_reexport_link_target(output, ir, link.target)
    } else {
        write_atom(output, item.name())
    }
}

fn write_reexport_link_target(
    output: &mut impl fmt::Write,
    ir: &Ir,
    target: LinkTarget,
) -> fmt::Result {
    match target {
        LinkTarget::Local(entity) => match ir.item(entity) {
            Some(item) => write_atom(output, item.name()),
            None => output.write_str("?dangling"),
        },
        LinkTarget::External(external) => match ir.external(external).and_then(external_path) {
            Some(path) => {
                let path_text = ir.atom(path).unwrap_or(b"?external");
                write_atom(output, source_path(path_text))
            }
            None => output.write_str("?unresolved"),
        },
    }
}

/// `!m` and `!v` are lowering namespace tags, not Rust syntax.
fn source_path(path: &[u8]) -> &[u8] {
    path.strip_suffix(b"!m")
        .or_else(|| path.strip_suffix(b"!v"))
        .unwrap_or(path)
}

fn write_embedding_target(
    output: &mut impl fmt::Write,
    ir: &Ir,
    target: LinkTarget,
) -> fmt::Result {
    match target {
        LinkTarget::Local(entity) => match ir.item(entity) {
            Some(item) => write_atom(output, item.name()),
            None => output.write_str("?dangling"),
        },
        LinkTarget::External(external) => match ir.external(external).and_then(external_display) {
            Some(display) => write_atom(output, ir.atom(display).unwrap_or(b"?unresolved")),
            None => output.write_str("?unresolved"),
        },
    }
}

fn external_path(target: &crate::ir::ExternalTarget) -> Option<crate::ir::AtomId> {
    match target {
        crate::ir::ExternalTarget::Foreign(target) => Some(target.path),
        crate::ir::ExternalTarget::FragmentEntity { display, .. } => Some(*display),
        crate::ir::ExternalTarget::Stable { .. } => None,
    }
}

fn external_display(target: &crate::ir::ExternalTarget) -> Option<crate::ir::AtomId> {
    match target {
        crate::ir::ExternalTarget::Foreign(target) => Some(target.display),
        crate::ir::ExternalTarget::FragmentEntity { display, .. } => Some(*display),
        crate::ir::ExternalTarget::Stable { .. } => None,
    }
}

const fn link_kind_name(kind: crate::ir::LinkKind) -> &'static str {
    match kind {
        crate::ir::LinkKind::Calls => "calls",
        crate::ir::LinkKind::MethodCall => "method-call",
        crate::ir::LinkKind::TypeReference => "type-reference",
        crate::ir::LinkKind::Reads => "reads",
        crate::ir::LinkKind::Writes => "writes",
        crate::ir::LinkKind::Imports => "imports",
        crate::ir::LinkKind::Implements => "implements",
        crate::ir::LinkKind::Overrides => "overrides",
        crate::ir::LinkKind::Reexports => "reexports",
        crate::ir::LinkKind::Inherits => "inherits",
        crate::ir::LinkKind::Documents => "documents",
    }
}

fn write_atom(output: &mut impl fmt::Write, bytes: &[u8]) -> fmt::Result {
    if let Ok(text) = str::from_utf8(bytes) {
        return output.write_str(text);
    }
    for byte in bytes {
        if byte.is_ascii_graphic() && *byte != b'%' {
            output.write_char(char::from(*byte))?;
        } else {
            write!(output, "%{byte:02X}")?;
        }
    }
    Ok(())
}

const fn docs_prefix(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Record => "struct.",
        ItemKind::Enum => "enum.",
        ItemKind::Trait => "trait.",
        ItemKind::Function => "fn.",
        ItemKind::TypeAlias => "type.",
        ItemKind::Constant => "constant.",
        ItemKind::Static => "static.",
        ItemKind::Macro => "macro.",
        _ => "",
    }
}

const fn builtin_name(builtin: BuiltinType) -> &'static str {
    match builtin {
        BuiltinType::Unit => "()",
        BuiltinType::Never => "!",
        BuiltinType::Bool => "bool",
        BuiltinType::LegacyChar => "char",
        BuiltinType::I8 => "i8",
        BuiltinType::I16 => "i16",
        BuiltinType::I32 => "i32",
        BuiltinType::I64 => "i64",
        BuiltinType::I128 => "i128",
        BuiltinType::U8 => "u8",
        BuiltinType::U16 => "u16",
        BuiltinType::U32 => "u32",
        BuiltinType::U64 => "u64",
        BuiltinType::U128 => "u128",
        BuiltinType::F16 => "f16",
        BuiltinType::F32 => "f32",
        BuiltinType::F64 => "f64",
        BuiltinType::String => "str",
        BuiltinType::Bytes => "bytes",
        BuiltinType::Object => "object",
        BuiltinType::Any => "any",
        BuiltinType::Unknown => "unknown",
        BuiltinType::None_ => "None",
        BuiltinType::List => "list",
        BuiltinType::Dict => "dict",
        BuiltinType::Set => "set",
        BuiltinType::FrozenSet => "frozenset",
        BuiltinType::Complex => "complex",
        BuiltinType::Decimal => "decimal",
        BuiltinType::Void => "void",
        BuiltinType::Number => "number",
        BuiltinType::BigInt => "bigint",
        BuiltinType::Symbol => "symbol",
        BuiltinType::UniqueSymbol => "unique symbol",
        BuiltinType::Null => "null",
        BuiltinType::Undefined => "undefined",
        BuiltinType::ArbitraryInteger => "integer",
        BuiltinType::NativeSignedInteger => "native-int",
        BuiltinType::NativeUnsignedInteger => "native-uint",
        BuiltinType::PointerAddressInteger => "pointer-uint",
    }
}

fn write_native_character(
    output: &mut impl fmt::Write,
    role: crate::ir::NativeCharacterRole,
    width: core::num::NonZeroU16,
) -> fmt::Result {
    let name = match role {
        crate::ir::NativeCharacterRole::UnicodeScalar => "unicode-scalar",
        crate::ir::NativeCharacterRole::Utf16CodeUnit => "utf16-code-unit",
        crate::ir::NativeCharacterRole::Utf32CodeUnit => "utf32-code-unit",
        crate::ir::NativeCharacterRole::CPlainSigned => "c-char-signed",
        crate::ir::NativeCharacterRole::CPlainUnsigned => "c-char-unsigned",
        crate::ir::NativeCharacterRole::CSigned => "signed-char",
        crate::ir::NativeCharacterRole::CUnsigned => "unsigned-char",
        crate::ir::NativeCharacterRole::CWideSigned => "wchar-signed",
        crate::ir::NativeCharacterRole::CWideUnsigned => "wchar-unsigned",
        crate::ir::NativeCharacterRole::CWideSignednessUnavailable => "wchar",
    };
    write!(output, "{name}[{}]", width.get())
}

fn write_c_qualifier_prefix(
    output: &mut impl fmt::Write,
    qualifiers: crate::ir::CvQualifiers,
) -> fmt::Result {
    let mut separator = "";
    if qualifiers.const_ {
        output.write_str(separator)?;
        output.write_str("const")?;
        separator = " ";
    }
    if qualifiers.volatile {
        output.write_str(separator)?;
        output.write_str("volatile")?;
        separator = " ";
    }
    if qualifiers.restrict {
        output.write_str(separator)?;
        output.write_str("restrict")?;
    }
    Ok(())
}

const fn unknown_name(reason: crate::ir::UnknownReason) -> &'static str {
    match reason {
        crate::ir::UnknownReason::Unannotated => "unannotated",
        crate::ir::UnknownReason::DynamicallyTyped => "dynamic",
        crate::ir::UnknownReason::UnresolvedLocalName => "unresolved-local",
        crate::ir::UnknownReason::UnresolvedExternal => "unresolved-external",
        crate::ir::UnknownReason::TruncatedAtDepthLimit => "truncated",
        crate::ir::UnknownReason::OracleGap => "oracle-gap",
        crate::ir::UnknownReason::NoIrRepresentation => "no-ir-representation",
        crate::ir::UnknownReason::Error => "error",
    }
}
