//! Static, loss-aware rendering over [`crate::SemanticCoreReader`].
//!
//! Neutral rendering owns an unambiguous declaration descriptor and name.
//! Language declaration syntax, type suffixes, and C-family declarators are
//! deliberately rejected here: their token placement belongs to a selected
//! dialect, never to a fallback that guesses a boundary.

use core::{fmt, marker::PhantomData, ops::Deref, str};

use thiserror::Error;

use crate::{
    CoreSemanticEntity, EntityId, FactAvailability, ItemKind, LanguageProfile, SemanticCoreReader,
    Visibility,
};

#[path = "semantic_render/canonical.rs"]
mod canonical;
#[path = "semantic_render/document.rs"]
mod document;

pub use canonical::{
    CanonicalTypeRenderError, CanonicalTypeRenderLimits, CanonicalTypeRenderReference,
    PreparedCanonicalType, PreparedCanonicalTypeView, prepare_canonical_type,
};
pub use document::{
    CFamilySemanticDocumentDialect, CSharpSemanticDocumentDialect, GoSemanticDocumentDialect,
    JavaSemanticDocumentDialect, PreparedSemanticDocument, PreparedSemanticDocumentView,
    PythonSemanticDocumentDialect, RustSemanticDocumentDialect, SemanticDocumentDialect,
    SemanticDocumentError, SemanticDocumentFact, SemanticDocumentReference,
    SemanticImageSourceSyntaxDialect, SourceSyntaxDialect, SourceSyntaxError,
    TypeScriptSemanticDocumentDialect, prepare_semantic_document, prepare_source_syntax,
};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// The precise semantic stage which has no lossless renderer yet.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum UnsupportedSemanticStage {
    #[error(
        "neutral declaration rendering cannot place a captured semantic type for entity {entity:?}"
    )]
    NeutralTypeSuffix { entity: EntityId },
    #[error(
        "profile {profile:?} requires a language declaration syntax renderer for entity {entity:?}"
    )]
    LanguageDeclarationSyntax {
        profile: LanguageProfile,
        entity: EntityId,
    },
    #[error(
        "C-family profile {profile:?} requires prefix/declarator/suffix ownership for entity {entity:?}"
    )]
    CFamilyDeclarator {
        profile: LanguageProfile,
        entity: EntityId,
    },
}

/// Exact render admission or caller-buffer failure.
#[derive(Debug, Error)]
pub enum RenderFailure {
    #[error("entity {entity:?} is not present in this semantic image")]
    MissingEntity { entity: EntityId },
    #[error("entity {entity:?} names missing atom {atom:?}")]
    MissingAtom {
        entity: EntityId,
        atom: crate::AtomId,
    },
    #[error(transparent)]
    Unsupported(#[from] UnsupportedSemanticStage),
    #[error(
        "rendering entity {entity:?} needs at least {required_at_least} bytes but the caller supplied {available}"
    )]
    OutputTooSmall {
        entity: EntityId,
        available: usize,
        required_at_least: usize,
    },
    #[error("neutral rendering length overflowed for entity {entity:?}")]
    OutputLengthOverflow { entity: EntityId },
    #[error(
        "prepared neutral rendering for entity {entity:?} wrote {written} bytes despite promised length {promised}"
    )]
    PreparedLengthMismatch {
        entity: EntityId,
        promised: usize,
        written: usize,
    },
    #[error("the caller formatter rejected the rendered declaration")]
    OutputWrite,
    #[error("the renderer produced invalid UTF-8 after {valid_up_to} bytes")]
    OutputEncoding {
        valid_up_to: usize,
        #[source]
        source: str::Utf8Error,
    },
}

/// Public immutable facts promised by one prepared neutral rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedNeutralView {
    pub entity: CoreSemanticEntity,
    pub encoded_len: usize,
}

/// A prepared neutral declaration whose semantic admission has completed.
///
/// Preparing does not mutate caller storage. Writing is a separate linear
/// step into a caller-owned byte buffer or formatter. The borrowed atom proof
/// stays private; public inspection uses the immutable [`PreparedNeutralView`].
pub struct PreparedNeutral<'image, R: SemanticCoreReader + ?Sized> {
    view: PreparedNeutralView,
    name: &'image [u8],
    _reader: PhantomData<&'image R>,
}

impl<R: SemanticCoreReader + ?Sized> Deref for PreparedNeutral<'_, R> {
    type Target = PreparedNeutralView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'image, R: SemanticCoreReader + ?Sized> PreparedNeutral<'image, R> {
    /// Writes the prepared neutral prefix/name rendering into caller storage.
    pub fn write_into<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output str, RenderFailure> {
        if output.len() < self.encoded_len {
            return Err(RenderFailure::OutputTooSmall {
                entity: self.entity.id,
                available: output.len(),
                required_at_least: self.encoded_len,
            });
        }
        let written = {
            let mut writer = SliceWriter::new(output);
            if write_neutral(&mut writer, self.entity, self.name).is_err() {
                return Err(RenderFailure::PreparedLengthMismatch {
                    entity: self.entity.id,
                    promised: self.encoded_len,
                    written: writer.written_len(),
                });
            }
            writer.written_len()
        };
        str::from_utf8(&output[..written]).map_err(|source| RenderFailure::OutputEncoding {
            valid_up_to: source.valid_up_to(),
            source,
        })
    }

    /// Streams into a caller formatter without an intermediate allocation.
    pub fn write_to(&self, output: &mut impl fmt::Write) -> Result<(), RenderFailure> {
        write_neutral(output, self.entity, self.name).map_err(|_| RenderFailure::OutputWrite)
    }
}

/// Prepares lossless neutral prefix/name rendering through any static reader.
pub fn prepare_neutral<'image, R: SemanticCoreReader + ?Sized>(
    reader: &'image R,
    entity: EntityId,
) -> Result<PreparedNeutral<'image, R>, RenderFailure> {
    let entity = reader
        .core_entity(entity)
        .ok_or(RenderFailure::MissingEntity { entity })?;
    if entity.authority.semantic_type == FactAvailability::Captured {
        return Err(UnsupportedSemanticStage::NeutralTypeSuffix { entity: entity.id }.into());
    }
    let name = reader.atom(entity.name).ok_or(RenderFailure::MissingAtom {
        entity: entity.id,
        atom: entity.name,
    })?;
    let encoded_len = neutral_len(entity, name)
        .ok_or(RenderFailure::OutputLengthOverflow { entity: entity.id })?;
    Ok(PreparedNeutral {
        view: PreparedNeutralView {
            entity,
            encoded_len,
        },
        name,
        _reader: PhantomData,
    })
}

/// Sealed static language-rendering policy. No runtime trait object can hide
/// which dialect owns a declarator or type suffix.
pub trait RenderDialect: sealed::Sealed {
    fn prepare<'image, R: SemanticCoreReader + ?Sized>(
        profile: LanguageProfile,
        reader: &'image R,
        entity: EntityId,
    ) -> Result<PreparedNeutral<'image, R>, RenderFailure>;
}

/// Explicit common neutral rendering policy.
pub struct NeutralDialect;
impl sealed::Sealed for NeutralDialect {}
impl RenderDialect for NeutralDialect {
    fn prepare<'image, R: SemanticCoreReader + ?Sized>(
        _profile: LanguageProfile,
        reader: &'image R,
        entity: EntityId,
    ) -> Result<PreparedNeutral<'image, R>, RenderFailure> {
        prepare_neutral(reader, entity)
    }
}

struct LanguageDialect;
impl sealed::Sealed for LanguageDialect {}
impl RenderDialect for LanguageDialect {
    fn prepare<'image, R: SemanticCoreReader + ?Sized>(
        profile: LanguageProfile,
        _reader: &'image R,
        entity: EntityId,
    ) -> Result<PreparedNeutral<'image, R>, RenderFailure> {
        Err(UnsupportedSemanticStage::LanguageDeclarationSyntax { profile, entity }.into())
    }
}

struct CFamilyDialect;
impl sealed::Sealed for CFamilyDialect {}
impl RenderDialect for CFamilyDialect {
    fn prepare<'image, R: SemanticCoreReader + ?Sized>(
        profile: LanguageProfile,
        _reader: &'image R,
        entity: EntityId,
    ) -> Result<PreparedNeutral<'image, R>, RenderFailure> {
        Err(UnsupportedSemanticStage::CFamilyDeclarator { profile, entity }.into())
    }
}

/// Statically dispatches the selected profile before any output is touched.
///
/// Existing language renderers intentionally do not route through neutral
/// suffix syntax; until they own the full declaration grammar this returns an
/// exact unsupported terminal.
pub fn prepare_profile<'image, R: SemanticCoreReader + ?Sized>(
    profile: LanguageProfile,
    reader: &'image R,
    entity: EntityId,
) -> Result<PreparedNeutral<'image, R>, RenderFailure> {
    match profile {
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) => {
            CFamilyDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::Rust(_)
        | LanguageProfile::TypeScript(_)
        | LanguageProfile::Python(_)
        | LanguageProfile::Go(_)
        | LanguageProfile::Java(_)
        | LanguageProfile::CSharp(_) => LanguageDialect::prepare(profile, reader, entity),
    }
}

fn write_neutral(
    output: &mut impl fmt::Write,
    entity: CoreSemanticEntity,
    name: &[u8],
) -> fmt::Result {
    output.write_str("entity(kind=")?;
    output.write_str(kind_name(entity.kind))?;
    output.write_str(",visibility=")?;
    output.write_str(visibility_name(entity.visibility))?;
    output.write_str(",semantic-type=")?;
    output.write_str(availability_name(entity.authority.semantic_type))?;
    output.write_str(") ")?;
    write_atom(output, name)
}

fn neutral_len(entity: CoreSemanticEntity, name: &[u8]) -> Option<usize> {
    "entity(kind="
        .len()
        .checked_add(kind_name(entity.kind).len())?
        .checked_add(",visibility=".len())?
        .checked_add(visibility_name(entity.visibility).len())?
        .checked_add(",semantic-type=".len())?
        .checked_add(availability_name(entity.authority.semantic_type).len())?
        .checked_add(") ".len())?
        .checked_add(atom_len(name)?)
}

const fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Module => "module",
        ItemKind::Record => "record",
        ItemKind::Field => "field",
        ItemKind::Parameter => "parameter",
        ItemKind::Variant => "variant",
        ItemKind::Function => "function",
        ItemKind::TypeAlias => "alias",
        ItemKind::Trait => "trait",
        ItemKind::Implementation => "implementation",
        ItemKind::Enum => "enum",
        ItemKind::Constant => "constant",
        ItemKind::Static => "static",
        ItemKind::Reexport => "reexport",
        ItemKind::Macro => "macro",
        ItemKind::Namespace => "namespace",
    }
}

const fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Unknown => "unknown",
        Visibility::Private => "private",
        Visibility::Restricted => "restricted",
        Visibility::Package => "package",
        Visibility::Public => "public",
    }
}

const fn availability_name(value: crate::FactAvailability) -> &'static str {
    match value {
        crate::FactAvailability::Captured => "captured",
        crate::FactAvailability::Unavailable => "unavailable",
    }
}

fn atom_len(bytes: &[u8]) -> Option<usize> {
    if str::from_utf8(bytes).is_ok() {
        return Some(bytes.len());
    }
    bytes.iter().try_fold(0_usize, |total, byte| {
        total.checked_add(if byte.is_ascii_graphic() && *byte != b'%' {
            1
        } else {
            3
        })
    })
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

struct SliceWriter<'output> {
    output: &'output mut [u8],
    written: usize,
}

impl<'output> SliceWriter<'output> {
    fn new(output: &'output mut [u8]) -> Self {
        Self { output, written: 0 }
    }
    const fn written_len(&self) -> usize {
        self.written
    }
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        if end > self.output.len() {
            return Err(fmt::Error);
        }
        self.output[self.written..end].copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}
