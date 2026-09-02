//! Canonical subordinate wire section for [`crate::LanguageExtensionsView`].
//!
//! This intentionally is not an `Ir` image format. A future full semantic-image
//! owner may place this validated, profile-bound section beside the common
//! columns it references.

use core::{fmt, marker::PhantomData, ops::Deref};

use crate::{LanguageExtensionsView, SemanticImageAuthority};

const MAGIC: [u8; 4] = *b"NXLE";
const SCHEMA: u16 = 1;
const PLANES: usize = 7;
const HEADER: usize = 16;
const DIRECTORY: usize = 20;
const NONE: u32 = u32::MAX;

/// Closed directory kinds; a plane cannot be substituted for another one.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionDirectoryKind {
    TypeScript = 1,
    CSharp = 2,
    Go = 3,
    Rust = 4,
    Python = 5,
    Java = 6,
    Clang = 7,
}

impl LanguageExtensionDirectoryKind {
    const ALL: [Self; PLANES] = [
        Self::TypeScript,
        Self::CSharp,
        Self::Go,
        Self::Rust,
        Self::Python,
        Self::Java,
        Self::Clang,
    ];

    const fn fact_bytes(self) -> usize {
        match self {
            Self::TypeScript => 12,
            Self::CSharp => 36,
            Self::Go => 28,
            Self::Rust => 16,
            Self::Python => 12,
            Self::Java => 16,
            Self::Clang => 24,
        }
    }
}

/// Exact capacities of already validated common semantic columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LanguageExtensionCommonBounds {
    pub atoms: u32,
    pub types: u32,
    pub entities: u32,
    pub type_lists: u32,
    pub entity_lists: u32,
    pub atom_lists: u32,
    pub type_parameters: u32,
}

/// Bounds borrowed from an already validated common semantic image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedLanguageExtensionCommonBounds {
    facts: LanguageExtensionCommonBounds,
}

impl Deref for ValidatedLanguageExtensionCommonBounds {
    type Target = LanguageExtensionCommonBounds;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl ValidatedLanguageExtensionCommonBounds {
    pub(crate) const fn from_validated(facts: LanguageExtensionCommonBounds) -> Self {
        Self { facts }
    }
}

/// Typed failure while writing the caller-owned extension-section buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionEncodeError {
    OutputTooShort { required: usize, actual: usize },
    RowCountMismatch { expected: usize, observed: usize },
    LengthOverflow,
}

impl fmt::Display for LanguageExtensionEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "language-extension section encoding failed: {self:?}"
        )
    }
}
impl core::error::Error for LanguageExtensionEncodeError {}

/// Typed failure while validating a borrowed extension section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionReopenError {
    Magic {
        observed: [u8; 4],
    },
    Schema {
        observed: u16,
    },
    Length {
        claimed: u32,
        actual: usize,
    },
    Authority {
        expected: SemanticImageAuthority,
        observed: [u8; 3],
    },
    ProfilePlane {
        authority: SemanticImageAuthority,
        kind: LanguageExtensionDirectoryKind,
        facts: u32,
    },
    DirectoryKind {
        expected: LanguageExtensionDirectoryKind,
        observed: u8,
    },
    DirectoryOffset {
        kind: LanguageExtensionDirectoryKind,
        expected: u32,
        observed: u32,
    },
    EntityRows {
        expected: u32,
        observed: u32,
    },
    Ordinal {
        kind: LanguageExtensionDirectoryKind,
        row: u32,
        raw: u32,
        facts: u32,
    },
    CanonicalFact {
        kind: LanguageExtensionDirectoryKind,
        fact: u32,
    },
    SharedReference {
        kind: LanguageExtensionDirectoryKind,
        fact: u32,
        raw: u32,
        limit: u32,
    },
    FactEncoding {
        kind: LanguageExtensionDirectoryKind,
        fact: u32,
        word: u8,
        observed: u32,
    },
    DecodedFact {
        row: u32,
        fact: u32,
    },
    Truncated,
}

impl fmt::Display for LanguageExtensionReopenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "language-extension section rejected: {self:?}")
    }
}
impl core::error::Error for LanguageExtensionReopenError {}

/// Once-validated borrowed subordinate extension section.
#[derive(Clone, Copy, Debug)]
pub struct ReopenedLanguageExtensionSection<'wire> {
    pub bytes: &'wire [u8],
    pub authority: SemanticImageAuthority,
    pub entity_rows: u32,
    pub typescript: ReopenedLanguageExtensionColumn<'wire, crate::TypeScriptFacts>,
    pub csharp: ReopenedLanguageExtensionColumn<'wire, crate::CSharpFacts>,
    pub go: ReopenedLanguageExtensionColumn<'wire, crate::GoFacts>,
    pub rust: ReopenedLanguageExtensionColumn<'wire, crate::RustFacts>,
    pub python: ReopenedLanguageExtensionColumn<'wire, crate::PythonFacts>,
    pub java: ReopenedLanguageExtensionColumn<'wire, crate::JavaFacts>,
    pub clang: ReopenedLanguageExtensionColumn<'wire, crate::ClangFacts>,
}

#[derive(Clone, Copy, Debug)]
struct PlaneLayout {
    offset: usize,
    rows: u32,
    facts: u32,
    ordinal_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct ReopenedLanguageExtensionColumn<'wire, Facts> {
    bytes: &'wire [u8],
    layout: PlaneLayout,
    marker: PhantomData<fn() -> Facts>,
}

mod wire_fact_sealed {
    pub trait Sealed {}
}

pub trait LanguageExtensionWireFact: Copy + wire_fact_sealed::Sealed {
    #[doc(hidden)]
    const WIDTH: usize;
    #[doc(hidden)]
    fn decode(bytes: &[u8], offset: usize) -> Option<Self>;
}

impl<'wire, Facts: LanguageExtensionWireFact> ReopenedLanguageExtensionColumn<'wire, Facts> {
    pub fn get(
        self,
        entity: crate::EntityId,
    ) -> Result<Option<Facts>, LanguageExtensionReopenError> {
        if entity.raw >= self.layout.rows || self.layout.facts == 0 {
            return Ok(None);
        }
        let ordinal = read_word(self.bytes, self.layout.offset + entity.index() * 4)
            .ok_or(LanguageExtensionReopenError::Truncated)?;
        if ordinal == NONE {
            return Ok(None);
        }
        let offset = self
            .layout
            .offset
            .checked_add(self.layout.ordinal_bytes)
            .and_then(|offset| offset.checked_add(ordinal as usize * Facts::WIDTH))
            .ok_or(LanguageExtensionReopenError::Truncated)?;
        Facts::decode(self.bytes, offset).map(Some).ok_or(
            LanguageExtensionReopenError::DecodedFact {
                row: entity.raw,
                fact: ordinal,
            },
        )
    }
}

/// Exact byte count needed to encode the seven language planes.
pub fn language_extension_section_len(
    extensions: LanguageExtensionsView<'_>,
) -> Result<usize, LanguageExtensionEncodeError> {
    let rows = extensions.typescript.ids.row_count();
    let views = [
        (
            LanguageExtensionDirectoryKind::TypeScript,
            extensions.typescript.ids.row_count(),
            extensions.typescript.facts.len(),
        ),
        (
            LanguageExtensionDirectoryKind::CSharp,
            extensions.csharp.ids.row_count(),
            extensions.csharp.facts.len(),
        ),
        (
            LanguageExtensionDirectoryKind::Go,
            extensions.go.ids.row_count(),
            extensions.go.facts.len(),
        ),
        (
            LanguageExtensionDirectoryKind::Rust,
            extensions.rust.ids.row_count(),
            extensions.rust.facts.len(),
        ),
        (
            LanguageExtensionDirectoryKind::Python,
            extensions.python.ids.row_count(),
            extensions.python.facts.len(),
        ),
        (
            LanguageExtensionDirectoryKind::Java,
            extensions.java.ids.row_count(),
            extensions.java.facts.len(),
        ),
        (
            LanguageExtensionDirectoryKind::Clang,
            extensions.clang.ids.row_count(),
            extensions.clang.facts.len(),
        ),
    ];
    let mut total = HEADER + DIRECTORY * PLANES;
    for (kind, observed, facts) in views {
        if observed != rows {
            return Err(LanguageExtensionEncodeError::RowCountMismatch {
                expected: rows,
                observed,
            });
        }
        total = total
            .checked_add(if facts == 0 {
                0
            } else {
                rows.checked_mul(4)
                    .ok_or(LanguageExtensionEncodeError::LengthOverflow)?
            })
            .and_then(|value| value.checked_add(facts.checked_mul(kind.fact_bytes())?))
            .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
    }
    Ok(total)
}

/// Encodes every typed sparse plane in canonical directory order into caller storage.
pub fn encode_language_extension_section(
    extensions: LanguageExtensionsView<'_>,
    output: &mut [u8],
) -> Result<usize, LanguageExtensionEncodeError> {
    let required = language_extension_section_len(extensions)?;
    if output.len() < required {
        return Err(LanguageExtensionEncodeError::OutputTooShort {
            required,
            actual: output.len(),
        });
    }
    output[..required].fill(0);
    output[..4].copy_from_slice(&MAGIC);
    put_u16(output, 4, SCHEMA);
    output[6] = PLANES as u8;
    write_authority(output, 7, extensions.authority);
    put_u32(
        output,
        12,
        u32::try_from(required).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
    );
    let rows = u32::try_from(extensions.typescript.ids.row_count())
        .map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?;
    let mut payload = HEADER + DIRECTORY * PLANES;
    macro_rules! plane {
        ($index:expr, $kind:expr, $view:expr, $encode:ident) => {{
            let view = $view;
            let directory = HEADER + DIRECTORY * $index;
            output[directory] = $kind as u8;
            put_u32(output, directory + 4, rows);
            put_u32(
                output,
                directory + 8,
                u32::try_from(view.facts.len())
                    .map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
            );
            put_u32(
                output,
                directory + 12,
                u32::try_from(payload).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
            );
            let ordinal_bytes = if view.facts.is_empty() {
                0
            } else {
                usize::try_from(rows).unwrap_or(usize::MAX) * 4
            };
            let length = ordinal_bytes + view.facts.len() * $kind.fact_bytes();
            put_u32(
                output,
                directory + 16,
                u32::try_from(length).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
            );
            for raw in 0..rows {
                if !view.facts.is_empty() {
                    let id = view
                        .ids
                        .get(crate::EntityId::new(raw))
                        .map_or(NONE, |id| id.raw);
                    put_u32(
                        output,
                        payload + usize::try_from(raw).unwrap_or(usize::MAX) * 4,
                        id,
                    );
                }
            }
            let facts = payload + ordinal_bytes;
            for (index, fact) in view.facts.iter().copied().enumerate() {
                $encode(output, facts + index * $kind.fact_bytes(), fact);
            }
            payload += length;
        }};
    }
    plane!(
        0,
        LanguageExtensionDirectoryKind::TypeScript,
        extensions.typescript,
        encode_typescript
    );
    plane!(
        1,
        LanguageExtensionDirectoryKind::CSharp,
        extensions.csharp,
        encode_csharp
    );
    plane!(
        2,
        LanguageExtensionDirectoryKind::Go,
        extensions.go,
        encode_go
    );
    plane!(
        3,
        LanguageExtensionDirectoryKind::Rust,
        extensions.rust,
        encode_rust
    );
    plane!(
        4,
        LanguageExtensionDirectoryKind::Python,
        extensions.python,
        encode_python
    );
    plane!(
        5,
        LanguageExtensionDirectoryKind::Java,
        extensions.java,
        encode_java
    );
    plane!(
        6,
        LanguageExtensionDirectoryKind::Clang,
        extensions.clang,
        encode_clang
    );
    debug_assert_eq!(payload, required);
    Ok(required)
}

/// Reopens a section only after its closed directories, compact IDs, canonical
/// dense pools, and shared-arena references agree with validated bounds.
pub fn reopen_language_extension_section(
    bytes: &[u8],
    authority: SemanticImageAuthority,
    bounds: ValidatedLanguageExtensionCommonBounds,
) -> Result<ReopenedLanguageExtensionSection<'_>, LanguageExtensionReopenError> {
    if bytes.len() < HEADER {
        return Err(LanguageExtensionReopenError::Truncated);
    }
    let observed = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if observed != MAGIC {
        return Err(LanguageExtensionReopenError::Magic { observed });
    }
    if get_u16(bytes, 4)? != SCHEMA {
        return Err(LanguageExtensionReopenError::Schema {
            observed: get_u16(bytes, 4)?,
        });
    }
    let claimed = get_u32(bytes, 12)?;
    if usize::try_from(claimed).ok() != Some(bytes.len()) {
        return Err(LanguageExtensionReopenError::Length {
            claimed,
            actual: bytes.len(),
        });
    }
    if bytes[6] != PLANES as u8 || read_authority(bytes, 7)? != authority {
        return Err(LanguageExtensionReopenError::Authority {
            expected: authority,
            observed: [bytes[7], bytes[8], bytes[9]],
        });
    }
    let empty = PlaneLayout {
        offset: 0,
        rows: 0,
        facts: 0,
        ordinal_bytes: 0,
    };
    let mut layouts = [empty; PLANES];
    let mut expected = u32::try_from(HEADER + DIRECTORY * PLANES)
        .map_err(|_| LanguageExtensionReopenError::Truncated)?;
    for (index, kind) in LanguageExtensionDirectoryKind::ALL
        .iter()
        .copied()
        .enumerate()
    {
        let directory = HEADER + DIRECTORY * index;
        if *bytes
            .get(directory)
            .ok_or(LanguageExtensionReopenError::Truncated)?
            != kind as u8
        {
            return Err(LanguageExtensionReopenError::DirectoryKind {
                expected: kind,
                observed: bytes[directory],
            });
        }
        let rows = get_u32(bytes, directory + 4)?;
        if rows != bounds.entities {
            return Err(LanguageExtensionReopenError::EntityRows {
                expected: bounds.entities,
                observed: rows,
            });
        }
        let facts = get_u32(bytes, directory + 8)?;
        if facts != 0 && !authority_admits(authority, kind) {
            return Err(LanguageExtensionReopenError::ProfilePlane {
                authority,
                kind,
                facts,
            });
        }
        let offset = get_u32(bytes, directory + 12)?;
        if offset != expected {
            return Err(LanguageExtensionReopenError::DirectoryOffset {
                kind,
                expected,
                observed: offset,
            });
        }
        let length = get_u32(bytes, directory + 16)?;
        let ordinal_bytes = if facts == 0 {
            0
        } else {
            rows.checked_mul(4)
                .ok_or(LanguageExtensionReopenError::Truncated)?
        };
        let exact = facts
            .checked_mul(kind.fact_bytes() as u32)
            .and_then(|f| ordinal_bytes.checked_add(f))
            .ok_or(LanguageExtensionReopenError::Truncated)?;
        if length != exact {
            return Err(LanguageExtensionReopenError::Truncated);
        }
        let end = offset
            .checked_add(length)
            .ok_or(LanguageExtensionReopenError::Truncated)?;
        if usize::try_from(end)
            .ok()
            .filter(|end| *end <= bytes.len())
            .is_none()
        {
            return Err(LanguageExtensionReopenError::Truncated);
        }
        for row in 0..rows {
            if facts != 0 {
                let raw = get_u32(
                    bytes,
                    usize::try_from(offset).unwrap_or(usize::MAX)
                        + usize::try_from(row).unwrap_or(usize::MAX) * 4,
                )?;
                if raw != NONE && raw >= facts {
                    return Err(LanguageExtensionReopenError::Ordinal {
                        kind,
                        row,
                        raw,
                        facts,
                    });
                }
            }
        }
        for fact in 0..facts {
            let mut referenced = false;
            for row in 0..rows {
                let ordinal = get_u32(
                    bytes,
                    usize::try_from(offset).unwrap_or(usize::MAX)
                        + usize::try_from(row).unwrap_or(usize::MAX) * 4,
                )?;
                referenced |= ordinal == fact;
            }
            if !referenced {
                return Err(LanguageExtensionReopenError::CanonicalFact { kind, fact });
            }
        }
        for fact in 0..facts {
            let base = usize::try_from(offset).unwrap_or(usize::MAX)
                + usize::try_from(ordinal_bytes).unwrap_or(usize::MAX)
                + usize::try_from(fact).unwrap_or(usize::MAX) * kind.fact_bytes();
            validate_fact(bytes, base, kind, fact, *bounds)?;
        }
        layouts[index] = PlaneLayout {
            offset: usize::try_from(offset).map_err(|_| LanguageExtensionReopenError::Truncated)?,
            rows,
            facts,
            ordinal_bytes: usize::try_from(ordinal_bytes)
                .map_err(|_| LanguageExtensionReopenError::Truncated)?,
        };
        expected = end;
    }
    if usize::try_from(expected).ok() != Some(bytes.len()) {
        return Err(LanguageExtensionReopenError::Truncated);
    }
    Ok(ReopenedLanguageExtensionSection {
        bytes,
        authority,
        entity_rows: bounds.entities,
        typescript: reopened_column(bytes, layouts[0]),
        csharp: reopened_column(bytes, layouts[1]),
        go: reopened_column(bytes, layouts[2]),
        rust: reopened_column(bytes, layouts[3]),
        python: reopened_column(bytes, layouts[4]),
        java: reopened_column(bytes, layouts[5]),
        clang: reopened_column(bytes, layouts[6]),
    })
}

fn reopened_column<Facts>(
    bytes: &[u8],
    layout: PlaneLayout,
) -> ReopenedLanguageExtensionColumn<'_, Facts> {
    ReopenedLanguageExtensionColumn {
        bytes,
        layout,
        marker: PhantomData,
    }
}

fn read_word(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)
        .and_then(|word| word.try_into().ok())
        .map(u32::from_le_bytes)
}

impl LanguageExtensionWireFact for crate::TypeScriptFacts {
    const WIDTH: usize = 12;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let type_parameters = crate::TypeParameterListId::new(read_word(bytes, offset)?);
        let declared = optional_type(read_word(bytes, offset + 4)?);
        let computed = optional_type(read_word(bytes, offset + 8)?)
            .map(crate::semantic::reopened_computed_type);
        Some(Self {
            type_parameters,
            declared,
            computed,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::TypeScriptFacts {}

impl LanguageExtensionWireFact for crate::CSharpFacts {
    const WIDTH: usize = 36;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let nullability = match read_word(bytes, offset)? {
            0 => crate::CSharpNullability::Oblivious,
            1 => crate::CSharpNullability::NonNullable,
            2 => crate::CSharpNullability::Nullable,
            _ => return None,
        };
        let reference_kind = match read_word(bytes, offset + 4)? {
            0 => crate::CSharpReferenceKind::Value,
            1 => crate::CSharpReferenceKind::In,
            2 => crate::CSharpReferenceKind::Ref,
            3 => crate::CSharpReferenceKind::Out,
            _ => return None,
        };
        let effects = read_word(bytes, offset + 8)?;
        if effects & !7 != 0 {
            return None;
        }
        let partial = match read_word(bytes, offset + 12)? {
            0 => crate::CSharpPartialRole::None,
            1 => crate::CSharpPartialRole::Definition,
            2 => crate::CSharpPartialRole::Implementation,
            _ => return None,
        };
        let file = read_word(bytes, offset + 24)?;
        let xml_provenance = if file == NONE {
            None
        } else {
            crate::SourceSpan::new(
                crate::AtomId::new(file),
                read_word(bytes, offset + 28)?,
                read_word(bytes, offset + 32)?,
            )
        };
        Some(Self {
            nullability,
            reference_kind,
            constraints: crate::TypeParameterListId::new(read_word(bytes, offset + 16)?),
            effects: crate::CSharpMemberEffects {
                is_async: effects & 1 != 0,
                is_iterator: effects & 2 != 0,
                is_extension: effects & 4 != 0,
            },
            attributes: crate::AtomListId::new(read_word(bytes, offset + 20)?),
            partial,
            xml_provenance,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::CSharpFacts {}

impl LanguageExtensionWireFact for crate::GoFacts {
    const WIDTH: usize = 28;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        Some(Self {
            signature: crate::GoSignature {
                parameters: crate::TypeListId::new(read_word(bytes, offset)?),
                results: crate::TypeListId::new(read_word(bytes, offset + 4)?),
                variadic: match read_word(bytes, offset + 8)? {
                    0 => false,
                    1 => true,
                    _ => return None,
                },
            },
            type_parameters: crate::TypeParameterListId::new(read_word(bytes, offset + 12)?),
            fields: crate::EntityListId::new(read_word(bytes, offset + 16)?),
            method_set: crate::EntityListId::new(read_word(bytes, offset + 20)?),
            build_constraints: crate::AtomListId::new(read_word(bytes, offset + 24)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::GoFacts {}
impl LanguageExtensionWireFact for crate::RustFacts {
    const WIDTH: usize = 16;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let ownership = match read_word(bytes, offset)? {
            0 => crate::RustOwnership::Value,
            1 => crate::RustOwnership::SharedBorrow,
            2 => crate::RustOwnership::MutableBorrow,
            3 => crate::RustOwnership::Moved,
            _ => return None,
        };
        Some(Self {
            ownership,
            lifetimes: crate::AtomListId::new(read_word(bytes, offset + 4)?),
            where_clauses: crate::TypeParameterListId::new(read_word(bytes, offset + 8)?),
            macros: crate::AtomListId::new(read_word(bytes, offset + 12)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::RustFacts {}
impl LanguageExtensionWireFact for crate::PythonFacts {
    const WIDTH: usize = 12;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let parameter_kind = match read_word(bytes, offset + 4)? {
            0 => crate::PythonParameterKind::PositionalOnly,
            1 => crate::PythonParameterKind::PositionalOrKeyword,
            2 => crate::PythonParameterKind::VariadicPositional,
            3 => crate::PythonParameterKind::KeywordOnly,
            4 => crate::PythonParameterKind::VariadicKeyword,
            _ => return None,
        };
        let dynamic_confidence = match read_word(bytes, offset + 8)? {
            0 => crate::Confidence::Syntactic,
            1 => crate::Confidence::Heuristic,
            2 => crate::Confidence::Indexed,
            3 => crate::Confidence::Imported,
            4 => crate::Confidence::Compiler,
            _ => return None,
        };
        Some(Self {
            decorators: crate::AtomListId::new(read_word(bytes, offset)?),
            parameter_kind,
            dynamic_confidence,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::PythonFacts {}
impl LanguageExtensionWireFact for crate::JavaFacts {
    const WIDTH: usize = 16;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        Some(Self {
            throws: crate::TypeListId::new(read_word(bytes, offset)?),
            annotations: crate::AtomListId::new(read_word(bytes, offset + 4)?),
            overloads: crate::EntityListId::new(read_word(bytes, offset + 8)?),
            record_components: crate::EntityListId::new(read_word(bytes, offset + 12)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::JavaFacts {}
impl LanguageExtensionWireFact for crate::ClangFacts {
    const WIDTH: usize = 24;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let qualifiers = read_word(bytes, offset)?;
        if qualifiers & !7 != 0 {
            return None;
        }
        let storage = match read_word(bytes, offset + 4)? {
            0 => crate::ClangStorageClass::None,
            1 => crate::ClangStorageClass::Auto,
            2 => crate::ClangStorageClass::Static,
            3 => crate::ClangStorageClass::Extern,
            4 => crate::ClangStorageClass::Register,
            5 => crate::ClangStorageClass::ThreadLocal,
            _ => return None,
        };
        let align = read_word(bytes, offset + 12)?;
        if align == 0 {
            return None;
        }
        Some(Self {
            qualifiers: crate::ClangQualifiers {
                is_const: qualifiers & 1 != 0,
                is_volatile: qualifiers & 2 != 0,
                is_restrict: qualifiers & 4 != 0,
            },
            storage,
            layout: crate::ClangLayout {
                size_bits: optional_u32(read_word(bytes, offset + 8)?),
                align_bits: optional_u32(align),
            },
            templates: crate::TypeParameterListId::new(read_word(bytes, offset + 16)?),
            includes: crate::AtomListId::new(read_word(bytes, offset + 20)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ClangFacts {}

fn optional_type(raw: u32) -> Option<crate::TypeId> {
    (raw != NONE).then(|| crate::TypeId::new(raw))
}
fn optional_u32(raw: u32) -> Option<u32> {
    (raw != NONE).then_some(raw)
}

fn put_u16(output: &mut [u8], at: usize, value: u16) {
    output[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put_u32(output: &mut [u8], at: usize, value: u32) {
    output[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn get_u16(input: &[u8], at: usize) -> Result<u16, LanguageExtensionReopenError> {
    Ok(u16::from_le_bytes(
        input
            .get(at..at + 2)
            .ok_or(LanguageExtensionReopenError::Truncated)?
            .try_into()
            .map_err(|_| LanguageExtensionReopenError::Truncated)?,
    ))
}
fn get_u32(input: &[u8], at: usize) -> Result<u32, LanguageExtensionReopenError> {
    Ok(u32::from_le_bytes(
        input
            .get(at..at + 4)
            .ok_or(LanguageExtensionReopenError::Truncated)?
            .try_into()
            .map_err(|_| LanguageExtensionReopenError::Truncated)?,
    ))
}

fn write_authority(output: &mut [u8], at: usize, authority: SemanticImageAuthority) {
    match authority {
        SemanticImageAuthority::Shared => output[at] = 0,
        SemanticImageAuthority::Language(profile) => {
            output[at] = 1;
            let code: [u8; 2] = profile.into();
            output[at + 1] = code[0];
            output[at + 2] = code[1];
        }
    }
}
fn read_authority(
    input: &[u8],
    at: usize,
) -> Result<SemanticImageAuthority, LanguageExtensionReopenError> {
    match *input
        .get(at)
        .ok_or(LanguageExtensionReopenError::Truncated)?
    {
        0 => Ok(SemanticImageAuthority::Shared),
        1 => compiler_vocabulary::LanguageProfile::try_from([
            *input
                .get(at + 1)
                .ok_or(LanguageExtensionReopenError::Truncated)?,
            *input
                .get(at + 2)
                .ok_or(LanguageExtensionReopenError::Truncated)?,
        ])
        .map(SemanticImageAuthority::Language)
        .map_err(|_| LanguageExtensionReopenError::Truncated),
        _ => Err(LanguageExtensionReopenError::Truncated),
    }
}
fn authority_admits(
    authority: SemanticImageAuthority,
    kind: LanguageExtensionDirectoryKind,
) -> bool {
    match authority {
        SemanticImageAuthority::Shared => false,
        SemanticImageAuthority::Language(profile) => matches!(
            (compiler_vocabulary::Language::from(profile), kind),
            (
                compiler_vocabulary::Language::TypeScript,
                LanguageExtensionDirectoryKind::TypeScript
            ) | (
                compiler_vocabulary::Language::CSharp,
                LanguageExtensionDirectoryKind::CSharp
            ) | (
                compiler_vocabulary::Language::Go,
                LanguageExtensionDirectoryKind::Go
            ) | (
                compiler_vocabulary::Language::Rust,
                LanguageExtensionDirectoryKind::Rust
            ) | (
                compiler_vocabulary::Language::Python,
                LanguageExtensionDirectoryKind::Python
            ) | (
                compiler_vocabulary::Language::Java,
                LanguageExtensionDirectoryKind::Java
            ) | (
                compiler_vocabulary::Language::Clang,
                LanguageExtensionDirectoryKind::Clang
            )
        ),
    }
}

fn word(output: &mut [u8], base: usize, index: usize, value: u32) {
    put_u32(output, base + index * 4, value);
}
fn option(id: Option<crate::TypeId>) -> u32 {
    id.map_or(NONE, |id| id.raw)
}
fn encode_typescript(o: &mut [u8], b: usize, f: crate::TypeScriptFacts) {
    word(o, b, 0, f.type_parameters.raw);
    word(o, b, 1, option(f.declared));
    word(o, b, 2, f.computed.map_or(NONE, |id| id.erase().raw));
}
fn encode_csharp(o: &mut [u8], b: usize, f: crate::CSharpFacts) {
    word(o, b, 0, f.nullability as u32);
    word(o, b, 1, f.reference_kind as u32);
    word(
        o,
        b,
        2,
        u32::from(f.effects.is_async)
            | u32::from(f.effects.is_iterator) << 1
            | u32::from(f.effects.is_extension) << 2,
    );
    word(o, b, 3, f.partial as u32);
    word(o, b, 4, f.constraints.raw);
    word(o, b, 5, f.attributes.raw);
    word(o, b, 6, f.xml_provenance.map_or(NONE, |s| s.file().raw));
    word(o, b, 7, f.xml_provenance.map_or(0, |s| s.start()));
    word(o, b, 8, f.xml_provenance.map_or(0, |s| s.end()));
}
fn encode_go(o: &mut [u8], b: usize, f: crate::GoFacts) {
    word(o, b, 0, f.signature.parameters.raw);
    word(o, b, 1, f.signature.results.raw);
    word(o, b, 2, u32::from(f.signature.variadic));
    word(o, b, 3, f.type_parameters.raw);
    word(o, b, 4, f.fields.raw);
    word(o, b, 5, f.method_set.raw);
    word(o, b, 6, f.build_constraints.raw);
}
fn encode_rust(o: &mut [u8], b: usize, f: crate::RustFacts) {
    word(o, b, 0, f.ownership as u32);
    word(o, b, 1, f.lifetimes.raw);
    word(o, b, 2, f.where_clauses.raw);
    word(o, b, 3, f.macros.raw);
}
fn encode_python(o: &mut [u8], b: usize, f: crate::PythonFacts) {
    word(o, b, 0, f.decorators.raw);
    word(o, b, 1, f.parameter_kind as u32);
    word(o, b, 2, f.dynamic_confidence as u32);
}
fn encode_java(o: &mut [u8], b: usize, f: crate::JavaFacts) {
    word(o, b, 0, f.throws.raw);
    word(o, b, 1, f.annotations.raw);
    word(o, b, 2, f.overloads.raw);
    word(o, b, 3, f.record_components.raw);
}
fn encode_clang(o: &mut [u8], b: usize, f: crate::ClangFacts) {
    word(
        o,
        b,
        0,
        u32::from(f.qualifiers.is_const)
            | u32::from(f.qualifiers.is_volatile) << 1
            | u32::from(f.qualifiers.is_restrict) << 2,
    );
    word(o, b, 1, f.storage as u32);
    word(o, b, 2, f.layout.size_bits.unwrap_or(NONE));
    word(o, b, 3, f.layout.align_bits.unwrap_or(NONE));
    word(o, b, 4, f.templates.raw);
    word(o, b, 5, f.includes.raw);
}

fn validate_fact(
    i: &[u8],
    b: usize,
    k: LanguageExtensionDirectoryKind,
    f: u32,
    n: LanguageExtensionCommonBounds,
) -> Result<(), LanguageExtensionReopenError> {
    let w = |x: usize| get_u32(i, b + x * 4);
    let check = |raw, limit| {
        if raw < limit {
            Ok(())
        } else {
            Err(LanguageExtensionReopenError::SharedReference {
                kind: k,
                fact: f,
                raw,
                limit,
            })
        }
    };
    match k {
        LanguageExtensionDirectoryKind::TypeScript => {
            check(w(0)?, n.type_parameters)?;
            for x in [w(1)?, w(2)?] {
                if x != NONE {
                    check(x, n.types)?;
                }
            }
        }
        LanguageExtensionDirectoryKind::CSharp => {
            check(w(4)?, n.type_parameters)?;
            check(w(5)?, n.atom_lists)?;
            if w(6)? != NONE {
                check(w(6)?, n.atoms)?;
            }
        }
        LanguageExtensionDirectoryKind::Go => {
            for x in [w(0)?, w(1)?] {
                check(x, n.type_lists)?;
            }
            check(w(3)?, n.type_parameters)?;
            for x in [w(4)?, w(5)?] {
                check(x, n.entity_lists)?;
            }
            check(w(6)?, n.atom_lists)?;
        }
        LanguageExtensionDirectoryKind::Rust => {
            check(w(1)?, n.atom_lists)?;
            check(w(2)?, n.type_parameters)?;
            check(w(3)?, n.atom_lists)?;
        }
        LanguageExtensionDirectoryKind::Python => check(w(0)?, n.atom_lists)?,
        LanguageExtensionDirectoryKind::Java => {
            check(w(0)?, n.type_lists)?;
            check(w(1)?, n.atom_lists)?;
            check(w(2)?, n.entity_lists)?;
            check(w(3)?, n.entity_lists)?;
        }
        LanguageExtensionDirectoryKind::Clang => {
            check(w(4)?, n.type_parameters)?;
            check(w(5)?, n.atom_lists)?;
        }
    }
    let decoded = match k {
        LanguageExtensionDirectoryKind::TypeScript => {
            crate::TypeScriptFacts::decode(i, b).is_some()
        }
        LanguageExtensionDirectoryKind::CSharp => crate::CSharpFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Go => crate::GoFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Rust => crate::RustFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Python => crate::PythonFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Java => crate::JavaFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Clang => crate::ClangFacts::decode(i, b).is_some(),
    };
    if !decoded {
        return Err(LanguageExtensionReopenError::FactEncoding {
            kind: k,
            fact: f,
            word: 0,
            observed: w(0)?,
        });
    }
    Ok(())
}
