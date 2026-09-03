//! Canonical subordinate wire section for [`crate::LanguageExtensionsView`].
//!
//! This intentionally is not an `Ir` image format. A future full semantic-image
//! owner may place this validated, profile-bound section beside the common
//! columns it references.
#![deny(
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    unsafe_code
)]

use core::{fmt, marker::PhantomData, ops::Deref};

use crate::{LanguageExtensionsView, SemanticImageAuthority};

const MAGIC: [u8; 4] = *b"NXLE";
const SCHEMA_LEGACY: u16 = 1;
const SCHEMA: u16 = 2;
const PLANES: usize = 7;
const PLANES_WIRE: u8 = 7;
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

    const fn schema_fact_bytes(self, schema: u16) -> usize {
        if matches!(self, Self::Clang) && schema == SCHEMA {
            28
        } else {
            self.fact_bytes()
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::TypeScript => 1,
            Self::CSharp => 2,
            Self::Go => 3,
            Self::Rust => 4,
            Self::Python => 5,
            Self::Java => 6,
            Self::Clang => 7,
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
    OutputTooShort {
        required: usize,
        actual: usize,
    },
    OutputRange {
        offset: usize,
        width: usize,
        actual: usize,
    },
    RowCountMismatch {
        expected: usize,
        observed: usize,
    },
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
    AuthorityTag {
        observed: u8,
    },
    Profile {
        source: compiler_vocabulary::UnknownLanguageProfile,
    },
    PlaneCount {
        expected: u8,
        observed: u8,
    },
    Reserved {
        offset: usize,
        observed: u8,
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
    DirectoryLength {
        kind: LanguageExtensionDirectoryKind,
        expected: u32,
        observed: u32,
    },
    StructuralOverflow {
        offset: usize,
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
    SourceSpanEncoding {
        kind: LanguageExtensionDirectoryKind,
        fact: u32,
        file: u32,
        start: u32,
        end: u32,
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
    fact_bytes: usize,
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
        let row = entity.index();
        let ordinal_offset = row
            .checked_mul(4)
            .and_then(|width| self.layout.offset.checked_add(width))
            .ok_or(LanguageExtensionReopenError::StructuralOverflow {
                offset: self.layout.offset,
            })?;
        let ordinal =
            read_word(self.bytes, ordinal_offset).ok_or(LanguageExtensionReopenError::Truncated)?;
        if ordinal == NONE {
            return Ok(None);
        }
        let ordinal = usize::try_from(ordinal).map_err(|_| {
            LanguageExtensionReopenError::StructuralOverflow {
                offset: self.layout.offset,
            }
        })?;
        let offset = self
            .layout
            .offset
            .checked_add(self.layout.ordinal_bytes)
            .and_then(|offset| {
                ordinal
                    .checked_mul(self.layout.fact_bytes)
                    .and_then(|width| offset.checked_add(width))
            })
            .ok_or(LanguageExtensionReopenError::StructuralOverflow {
                offset: self.layout.offset,
            })?;
        Facts::decode(self.bytes, offset).map(Some).ok_or(
            LanguageExtensionReopenError::DecodedFact {
                row: entity.raw,
                fact: u32::try_from(ordinal)
                    .map_err(|_| LanguageExtensionReopenError::StructuralOverflow { offset })?,
            },
        )
    }
}

impl<'wire> ReopenedLanguageExtensionColumn<'wire, crate::ClangFacts> {
    /// Returns the schema-2 owner ordinal stored beside a clang fact.
    pub fn owner(
        self,
        entity: crate::EntityId,
    ) -> Result<Option<u32>, LanguageExtensionReopenError> {
        if entity.raw >= self.layout.rows || self.layout.facts == 0 {
            return Ok(None);
        }
        let row = entity.index();
        let ordinal = read_word(
            self.bytes,
            row.checked_mul(4)
                .and_then(|width| self.layout.offset.checked_add(width))
                .ok_or(LanguageExtensionReopenError::StructuralOverflow {
                    offset: self.layout.offset,
                })?,
        )
        .ok_or(LanguageExtensionReopenError::Truncated)?;
        if ordinal == NONE {
            return Ok(None);
        }
        let ordinal = usize::try_from(ordinal).map_err(|_| {
            LanguageExtensionReopenError::StructuralOverflow {
                offset: self.layout.offset,
            }
        })?;
        let offset = self
            .layout
            .offset
            .checked_add(self.layout.ordinal_bytes)
            .and_then(|offset| {
                ordinal
                    .checked_mul(self.layout.fact_bytes)
                    .and_then(|width| offset.checked_add(width))
            })
            .and_then(|offset| offset.checked_add(crate::ClangFacts::WIDTH))
            .ok_or(LanguageExtensionReopenError::StructuralOverflow {
                offset: self.layout.offset,
            })?;
        read_word(self.bytes, offset)
            .map(Some)
            .ok_or(LanguageExtensionReopenError::Truncated)
    }
}

/// Encode-side plane source: one language's row table and dense fact pool.
/// Implemented identically by the in-image column view and by the borrowed
/// [`ExtensionSectionPlane`], so section bytes never depend on the owner.
pub trait ExtensionSectionSource: Copy {
    /// The fixed-width wire fact this plane encodes.
    type Facts: Copy;

    /// Number of entity rows, including universally absent rows.
    fn row_count(&self) -> usize;
    /// The dense fact ordinal one row carries, or [`SECTION_NONE`] when the
    /// row carries no fact of this plane.
    fn fact_ordinal(&self, row: u32) -> u32;
    /// The dense fact pool in canonical order.
    fn fact_slice(&self) -> &[Self::Facts];
}

/// Sentinel marking a row without a fact in one plane's row table.
pub const SECTION_NONE: u32 = u32::MAX;

/// One borrowed sparse plane ready for section encoding.
#[derive(Clone, Copy, Debug)]
pub struct ExtensionSectionPlane<'a, Facts: Copy> {
    /// Dense fact pool addressed only by [`Self::row_ordinals`].
    pub facts: &'a [Facts],
    /// One ordinal per entity row; [`SECTION_NONE`] marks an absent row.
    pub row_ordinals: &'a [u32],
}

impl<'a, Facts: Copy> ExtensionSectionSource for ExtensionSectionPlane<'a, Facts> {
    type Facts = Facts;

    fn row_count(&self) -> usize {
        self.row_ordinals.len()
    }

    fn fact_ordinal(&self, row: u32) -> u32 {
        self.row_ordinals
            .get(usize::try_from(row).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(SECTION_NONE)
    }

    fn fact_slice(&self) -> &[Facts] {
        self.facts
    }
}

impl<Facts: Copy, Space> ExtensionSectionSource
    for crate::LanguageExtensionColumnView<'_, Facts, Space>
{
    type Facts = Facts;

    fn row_count(&self) -> usize {
        self.ids.row_count()
    }

    fn fact_ordinal(&self, row: u32) -> u32 {
        self.ids
            .get(crate::EntityId::new(row))
            .map_or(SECTION_NONE, |id| id.raw)
    }

    fn fact_slice(&self) -> &[Facts] {
        self.facts
    }
}

/// The seven borrowed planes plus the authority that selected them, ready
/// for fragment-side encoding.
#[derive(Clone, Copy, Debug)]
pub struct ExtensionSectionInput<'a> {
    /// The profile authority that selected every nonempty plane.
    pub authority: SemanticImageAuthority,
    /// TypeScript plane.
    pub typescript: ExtensionSectionPlane<'a, crate::TypeScriptFacts>,
    /// C# plane.
    pub csharp: ExtensionSectionPlane<'a, crate::CSharpFacts>,
    /// Go plane.
    pub go: ExtensionSectionPlane<'a, crate::GoFacts>,
    /// Rust plane.
    pub rust: ExtensionSectionPlane<'a, crate::RustFacts>,
    /// Python plane.
    pub python: ExtensionSectionPlane<'a, crate::PythonFacts>,
    /// Java plane.
    pub java: ExtensionSectionPlane<'a, crate::JavaFacts>,
    /// Clang plane.
    pub clang: ExtensionSectionPlane<'a, crate::ClangFacts>,
}

/// Exact byte count needed to encode the seven language planes of one
/// in-image extension view.
pub fn language_extension_section_len(
    extensions: LanguageExtensionsView<'_>,
) -> Result<usize, LanguageExtensionEncodeError> {
    section_len(
        extensions.typescript,
        extensions.csharp,
        extensions.go,
        extensions.rust,
        extensions.python,
        extensions.java,
        extensions.clang,
    )
}

/// Exact byte count needed to encode the seven borrowed planes.
pub fn fragment_extension_section_len(
    input: ExtensionSectionInput<'_>,
) -> Result<usize, LanguageExtensionEncodeError> {
    section_len(
        input.typescript,
        input.csharp,
        input.go,
        input.rust,
        input.python,
        input.java,
        input.clang,
    )
}

fn section_len<Ts, Cs, Go, Ru, Py, Ja, Cl>(
    typescript: Ts,
    csharp: Cs,
    go: Go,
    rust: Ru,
    python: Py,
    java: Ja,
    clang: Cl,
) -> Result<usize, LanguageExtensionEncodeError>
where
    Ts: ExtensionSectionSource,
    Cs: ExtensionSectionSource,
    Go: ExtensionSectionSource,
    Ru: ExtensionSectionSource,
    Py: ExtensionSectionSource,
    Ja: ExtensionSectionSource,
    Cl: ExtensionSectionSource,
{
    let rows = typescript.row_count();
    let views: [(LanguageExtensionDirectoryKind, usize, usize); PLANES] = [
        (
            LanguageExtensionDirectoryKind::TypeScript,
            typescript.row_count(),
            typescript.fact_slice().len(),
        ),
        (
            LanguageExtensionDirectoryKind::CSharp,
            csharp.row_count(),
            csharp.fact_slice().len(),
        ),
        (
            LanguageExtensionDirectoryKind::Go,
            go.row_count(),
            go.fact_slice().len(),
        ),
        (
            LanguageExtensionDirectoryKind::Rust,
            rust.row_count(),
            rust.fact_slice().len(),
        ),
        (
            LanguageExtensionDirectoryKind::Python,
            python.row_count(),
            python.fact_slice().len(),
        ),
        (
            LanguageExtensionDirectoryKind::Java,
            java.row_count(),
            java.fact_slice().len(),
        ),
        (
            LanguageExtensionDirectoryKind::Clang,
            clang.row_count(),
            clang.fact_slice().len(),
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
            .and_then(|value| value.checked_add(facts.checked_mul(kind.schema_fact_bytes(SCHEMA))?))
            .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
    }
    Ok(total)
}

/// Encodes every typed sparse plane in canonical directory order into caller
/// storage, from one in-image extension view.
pub fn encode_language_extension_section(
    extensions: LanguageExtensionsView<'_>,
    output: &mut [u8],
) -> Result<usize, LanguageExtensionEncodeError> {
    encode_section(
        extensions.authority,
        extensions.typescript,
        extensions.csharp,
        extensions.go,
        extensions.rust,
        extensions.python,
        extensions.java,
        extensions.clang,
        output,
    )
}

/// Encodes every typed sparse plane in canonical directory order into caller
/// storage, from seven borrowed planes.
pub fn encode_fragment_extension_section(
    input: ExtensionSectionInput<'_>,
    output: &mut [u8],
) -> Result<usize, LanguageExtensionEncodeError> {
    encode_section(
        input.authority,
        input.typescript,
        input.csharp,
        input.go,
        input.rust,
        input.python,
        input.java,
        input.clang,
        output,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one authority plus one borrowed source per closed plane"
)]
fn encode_section<Ts, Cs, Go, Ru, Py, Ja, Cl>(
    authority: SemanticImageAuthority,
    typescript: Ts,
    csharp: Cs,
    go: Go,
    rust: Ru,
    python: Py,
    java: Ja,
    clang: Cl,
    output: &mut [u8],
) -> Result<usize, LanguageExtensionEncodeError>
where
    Ts: ExtensionSectionSource<Facts = crate::TypeScriptFacts>,
    Cs: ExtensionSectionSource<Facts = crate::CSharpFacts>,
    Go: ExtensionSectionSource<Facts = crate::GoFacts>,
    Ru: ExtensionSectionSource<Facts = crate::RustFacts>,
    Py: ExtensionSectionSource<Facts = crate::PythonFacts>,
    Ja: ExtensionSectionSource<Facts = crate::JavaFacts>,
    Cl: ExtensionSectionSource<Facts = crate::ClangFacts>,
{
    let required = section_len(typescript, csharp, go, rust, python, java, clang)?;
    let required_wire =
        u32::try_from(required).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?;
    let rows = u32::try_from(typescript.row_count())
        .map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?;
    let rows_usize =
        usize::try_from(rows).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?;
    if output.len() < required {
        return Err(LanguageExtensionEncodeError::OutputTooShort {
            required,
            actual: output.len(),
        });
    }
    fill_bytes(output, 0, required, 0)?;
    write_bytes(output, 0, &MAGIC)?;
    put_u16(output, 4, SCHEMA)?;
    put_u8(output, 6, PLANES_WIRE)?;
    write_authority(output, 7, authority)?;
    put_u32(output, 12, required_wire)?;
    let mut payload = HEADER + DIRECTORY * PLANES;
    macro_rules! plane {
        ($index:expr, $kind:expr, $view:expr, $encode:ident) => {{
            let view = $view;
            let directory = DIRECTORY
                .checked_mul($index)
                .and_then(|offset| HEADER.checked_add(offset))
                .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
            put_u8(output, directory, $kind.code())?;
            put_u32(output, directory + 4, rows)?;
            put_u32(
                output,
                directory + 8,
                u32::try_from(view.fact_slice().len())
                    .map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
            )?;
            put_u32(
                output,
                directory + 12,
                u32::try_from(payload).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
            )?;
            let facts = view.fact_slice();
            let ordinal_bytes = if facts.is_empty() {
                0
            } else {
                rows_usize
                    .checked_mul(4)
                    .ok_or(LanguageExtensionEncodeError::LengthOverflow)?
            };
            let fact_bytes = facts
                .len()
                .checked_mul($kind.schema_fact_bytes(SCHEMA))
                .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
            let length = ordinal_bytes
                .checked_add(fact_bytes)
                .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
            put_u32(
                output,
                directory + 16,
                u32::try_from(length).map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
            )?;
            for raw in 0..rows {
                if !facts.is_empty() {
                    let id = view.fact_ordinal(raw);
                    let raw = usize::try_from(raw)
                        .map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?;
                    let offset = raw
                        .checked_mul(4)
                        .and_then(|offset| payload.checked_add(offset))
                        .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
                    put_u32(output, offset, id)?;
                }
            }
            let facts_start = payload
                .checked_add(ordinal_bytes)
                .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
            for (index, fact) in facts.iter().copied().enumerate() {
                let offset = index
                    .checked_mul($kind.schema_fact_bytes(SCHEMA))
                    .and_then(|offset| facts_start.checked_add(offset))
                    .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
                $encode(output, offset, fact)?;
                if matches!($kind, LanguageExtensionDirectoryKind::Clang) {
                    put_u32(
                        output,
                        offset + crate::ClangFacts::WIDTH,
                        u32::try_from(index)
                            .map_err(|_| LanguageExtensionEncodeError::LengthOverflow)?,
                    )?;
                }
            }
            payload = payload
                .checked_add(length)
                .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
        }};
    }
    plane!(
        0,
        LanguageExtensionDirectoryKind::TypeScript,
        typescript,
        encode_typescript
    );
    plane!(
        1,
        LanguageExtensionDirectoryKind::CSharp,
        csharp,
        encode_csharp
    );
    plane!(2, LanguageExtensionDirectoryKind::Go, go, encode_go);
    plane!(3, LanguageExtensionDirectoryKind::Rust, rust, encode_rust);
    plane!(
        4,
        LanguageExtensionDirectoryKind::Python,
        python,
        encode_python
    );
    plane!(5, LanguageExtensionDirectoryKind::Java, java, encode_java);
    plane!(
        6,
        LanguageExtensionDirectoryKind::Clang,
        clang,
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
    let observed = read_array::<4>(bytes, 0)?;
    if observed != MAGIC {
        return Err(LanguageExtensionReopenError::Magic { observed });
    }
    let observed_schema = get_u16(bytes, 4)?;
    if !matches!(observed_schema, SCHEMA_LEGACY | SCHEMA) {
        return Err(LanguageExtensionReopenError::Schema {
            observed: observed_schema,
        });
    }
    let claimed = get_u32(bytes, 12)?;
    if usize::try_from(claimed).ok() != Some(bytes.len()) {
        return Err(LanguageExtensionReopenError::Length {
            claimed,
            actual: bytes.len(),
        });
    }
    let observed_planes = get_u8(bytes, 6)?;
    if observed_planes != PLANES_WIRE {
        return Err(LanguageExtensionReopenError::PlaneCount {
            expected: PLANES_WIRE,
            observed: observed_planes,
        });
    }
    for offset in 10..12 {
        let observed = get_u8(bytes, offset)?;
        if observed != 0 {
            return Err(LanguageExtensionReopenError::Reserved { offset, observed });
        }
    }
    if read_authority(bytes, 7)? != authority {
        return Err(LanguageExtensionReopenError::Authority {
            expected: authority,
            observed: read_array::<3>(bytes, 7)?,
        });
    }
    let empty = PlaneLayout {
        offset: 0,
        rows: 0,
        facts: 0,
        ordinal_bytes: 0,
        fact_bytes: 0,
    };
    let mut layouts = [empty; PLANES];
    let directory_bytes = DIRECTORY
        .checked_mul(PLANES)
        .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: HEADER })?;
    let payload_start = HEADER
        .checked_add(directory_bytes)
        .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: HEADER })?;
    let mut expected = u32::try_from(payload_start).map_err(|_| {
        LanguageExtensionReopenError::StructuralOverflow {
            offset: payload_start,
        }
    })?;
    for (index, kind) in LanguageExtensionDirectoryKind::ALL
        .iter()
        .copied()
        .enumerate()
    {
        let directory = DIRECTORY
            .checked_mul(index)
            .and_then(|offset| HEADER.checked_add(offset))
            .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: index })?;
        for offset in directory + 1..directory + 4 {
            let observed = get_u8(bytes, offset)?;
            if observed != 0 {
                return Err(LanguageExtensionReopenError::Reserved { offset, observed });
            }
        }
        let observed_kind = get_u8(bytes, directory)?;
        if observed_kind != kind.code() {
            return Err(LanguageExtensionReopenError::DirectoryKind {
                expected: kind,
                observed: observed_kind,
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
                .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: directory })?
        };
        let exact = facts
            .checked_mul(
                u32::try_from(kind.schema_fact_bytes(observed_schema)).map_err(|_| {
                    LanguageExtensionReopenError::StructuralOverflow { offset: directory }
                })?,
            )
            .and_then(|f| ordinal_bytes.checked_add(f))
            .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: directory })?;
        if length != exact {
            return Err(LanguageExtensionReopenError::DirectoryLength {
                kind,
                expected: exact,
                observed: length,
            });
        }
        let end = offset
            .checked_add(length)
            .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: directory })?;
        if usize::try_from(end)
            .ok()
            .filter(|end| *end <= bytes.len())
            .is_none()
        {
            return Err(LanguageExtensionReopenError::Truncated);
        }
        let offset_usize = usize::try_from(offset)
            .map_err(|_| LanguageExtensionReopenError::StructuralOverflow { offset: directory })?;
        let ordinal_bytes_usize = usize::try_from(ordinal_bytes)
            .map_err(|_| LanguageExtensionReopenError::StructuralOverflow { offset: directory })?;
        let mut next_expected = 0;
        for row in 0..rows {
            if facts == 0 {
                break;
            }
            let ordinal = get_u32(bytes, row_offset(offset_usize, row, directory)?)?;
            if ordinal == NONE {
                continue;
            }
            if ordinal >= facts {
                return Err(LanguageExtensionReopenError::Ordinal {
                    kind,
                    row,
                    raw: ordinal,
                    facts,
                });
            }
            if ordinal > next_expected {
                return Err(LanguageExtensionReopenError::CanonicalFact {
                    kind,
                    fact: next_expected,
                });
            }
            if ordinal == next_expected {
                next_expected = next_expected.checked_add(1).ok_or(
                    LanguageExtensionReopenError::StructuralOverflow { offset: directory },
                )?;
            }
        }
        if next_expected != facts {
            return Err(LanguageExtensionReopenError::CanonicalFact {
                kind,
                fact: next_expected,
            });
        }
        for fact in 0..facts {
            let fact = usize::try_from(fact).map_err(|_| {
                LanguageExtensionReopenError::StructuralOverflow { offset: directory }
            })?;
            let base = fact
                .checked_mul(kind.schema_fact_bytes(observed_schema))
                .and_then(|fact_offset| ordinal_bytes_usize.checked_add(fact_offset))
                .and_then(|fact_offset| offset_usize.checked_add(fact_offset))
                .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: directory })?;
            validate_fact(
                bytes,
                base,
                kind,
                u32::try_from(fact).map_err(|_| {
                    LanguageExtensionReopenError::StructuralOverflow { offset: directory }
                })?,
                *bounds,
            )?;
        }
        let layout = PlaneLayout {
            offset: offset_usize,
            rows,
            facts,
            ordinal_bytes: ordinal_bytes_usize,
            fact_bytes: kind.schema_fact_bytes(observed_schema),
        };
        let slot = layouts
            .get_mut(index)
            .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: index })?;
        *slot = layout;
        expected = end;
    }
    if usize::try_from(expected).ok() != Some(bytes.len()) {
        return Err(LanguageExtensionReopenError::Truncated);
    }
    let [typescript, csharp, go, rust, python, java, clang] = layouts;
    Ok(ReopenedLanguageExtensionSection {
        bytes,
        authority,
        entity_rows: bounds.entities,
        typescript: reopened_column(bytes, typescript),
        csharp: reopened_column(bytes, csharp),
        go: reopened_column(bytes, go),
        rust: reopened_column(bytes, rust),
        python: reopened_column(bytes, python),
        java: reopened_column(bytes, java),
        clang: reopened_column(bytes, clang),
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

fn output_range(
    output: &mut [u8],
    at: usize,
    width: usize,
) -> Result<&mut [u8], LanguageExtensionEncodeError> {
    let actual = output.len();
    let end = at
        .checked_add(width)
        .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
    output
        .get_mut(at..end)
        .ok_or(LanguageExtensionEncodeError::OutputRange {
            offset: at,
            width,
            actual,
        })
}

fn write_bytes(
    output: &mut [u8],
    at: usize,
    value: &[u8],
) -> Result<(), LanguageExtensionEncodeError> {
    output_range(output, at, value.len())?.copy_from_slice(value);
    Ok(())
}

fn fill_bytes(
    output: &mut [u8],
    at: usize,
    width: usize,
    value: u8,
) -> Result<(), LanguageExtensionEncodeError> {
    output_range(output, at, width)?.fill(value);
    Ok(())
}

fn put_u8(output: &mut [u8], at: usize, value: u8) -> Result<(), LanguageExtensionEncodeError> {
    let actual = output.len();
    let byte = output_range(output, at, 1)?;
    let slot = byte
        .first_mut()
        .ok_or(LanguageExtensionEncodeError::OutputRange {
            offset: at,
            width: 1,
            actual,
        })?;
    *slot = value;
    Ok(())
}

fn put_u16(output: &mut [u8], at: usize, value: u16) -> Result<(), LanguageExtensionEncodeError> {
    write_bytes(output, at, &value.to_le_bytes())
}

fn put_u32(output: &mut [u8], at: usize, value: u32) -> Result<(), LanguageExtensionEncodeError> {
    write_bytes(output, at, &value.to_le_bytes())
}

fn read_range(
    input: &[u8],
    at: usize,
    width: usize,
) -> Result<&[u8], LanguageExtensionReopenError> {
    let end = at
        .checked_add(width)
        .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: at })?;
    input
        .get(at..end)
        .ok_or(LanguageExtensionReopenError::Truncated)
}

fn read_array<const WIDTH: usize>(
    input: &[u8],
    at: usize,
) -> Result<[u8; WIDTH], LanguageExtensionReopenError> {
    read_range(input, at, WIDTH)?
        .try_into()
        .map_err(|_| LanguageExtensionReopenError::Truncated)
}

fn get_u8(input: &[u8], at: usize) -> Result<u8, LanguageExtensionReopenError> {
    input
        .get(at)
        .copied()
        .ok_or(LanguageExtensionReopenError::Truncated)
}

fn get_u16(input: &[u8], at: usize) -> Result<u16, LanguageExtensionReopenError> {
    read_array(input, at).map(u16::from_le_bytes)
}
fn get_u32(input: &[u8], at: usize) -> Result<u32, LanguageExtensionReopenError> {
    read_array(input, at).map(u32::from_le_bytes)
}

fn row_offset(
    base: usize,
    row: u32,
    context: usize,
) -> Result<usize, LanguageExtensionReopenError> {
    let row = usize::try_from(row)
        .map_err(|_| LanguageExtensionReopenError::StructuralOverflow { offset: context })?;
    row.checked_mul(4)
        .and_then(|width| base.checked_add(width))
        .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: context })
}

fn write_authority(
    output: &mut [u8],
    at: usize,
    authority: SemanticImageAuthority,
) -> Result<(), LanguageExtensionEncodeError> {
    match authority {
        SemanticImageAuthority::Shared => fill_bytes(output, at, 3, 0),
        SemanticImageAuthority::Language(profile) => {
            put_u8(output, at, 1)?;
            let code: [u8; 2] = profile.into();
            write_bytes(
                output,
                at.checked_add(1)
                    .ok_or(LanguageExtensionEncodeError::LengthOverflow)?,
                &code,
            )
        }
    }
}
fn read_authority(
    input: &[u8],
    at: usize,
) -> Result<SemanticImageAuthority, LanguageExtensionReopenError> {
    let observed @ [tag, first, second] = read_array::<3>(input, at)?;
    match tag {
        0 if first == 0 && second == 0 => Ok(SemanticImageAuthority::Shared),
        0 => Err(LanguageExtensionReopenError::Authority {
            expected: SemanticImageAuthority::Shared,
            observed,
        }),
        1 => compiler_vocabulary::LanguageProfile::try_from([first, second])
            .map(SemanticImageAuthority::Language)
            .map_err(|source| LanguageExtensionReopenError::Profile { source }),
        observed => Err(LanguageExtensionReopenError::AuthorityTag { observed }),
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

fn word(
    output: &mut [u8],
    base: usize,
    index: usize,
    value: u32,
) -> Result<(), LanguageExtensionEncodeError> {
    let offset = index
        .checked_mul(4)
        .and_then(|width| base.checked_add(width))
        .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
    put_u32(output, offset, value)
}
fn option(id: Option<crate::TypeId>) -> u32 {
    id.map_or(NONE, |id| id.raw)
}
fn encode_typescript(
    output: &mut [u8],
    base: usize,
    facts: crate::TypeScriptFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.type_parameters.raw)?;
    word(output, base, 1, option(facts.declared))?;
    word(
        output,
        base,
        2,
        facts.computed.map_or(NONE, |id| id.erase().raw),
    )
}
fn encode_csharp(
    output: &mut [u8],
    base: usize,
    facts: crate::CSharpFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(
        output,
        base,
        0,
        match facts.nullability {
            crate::CSharpNullability::Oblivious => 0,
            crate::CSharpNullability::NonNullable => 1,
            crate::CSharpNullability::Nullable => 2,
        },
    )?;
    word(
        output,
        base,
        1,
        match facts.reference_kind {
            crate::CSharpReferenceKind::Value => 0,
            crate::CSharpReferenceKind::In => 1,
            crate::CSharpReferenceKind::Ref => 2,
            crate::CSharpReferenceKind::Out => 3,
        },
    )?;
    word(
        output,
        base,
        2,
        u32::from(facts.effects.is_async)
            | u32::from(facts.effects.is_iterator) << 1
            | u32::from(facts.effects.is_extension) << 2,
    )?;
    word(
        output,
        base,
        3,
        match facts.partial {
            crate::CSharpPartialRole::None => 0,
            crate::CSharpPartialRole::Definition => 1,
            crate::CSharpPartialRole::Implementation => 2,
        },
    )?;
    word(output, base, 4, facts.constraints.raw)?;
    word(output, base, 5, facts.attributes.raw)?;
    word(
        output,
        base,
        6,
        facts.xml_provenance.map_or(NONE, |span| span.file().raw),
    )?;
    word(
        output,
        base,
        7,
        facts.xml_provenance.map_or(0, crate::SourceSpan::start),
    )?;
    word(
        output,
        base,
        8,
        facts.xml_provenance.map_or(0, crate::SourceSpan::end),
    )
}
fn encode_go(
    output: &mut [u8],
    base: usize,
    facts: crate::GoFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.signature.parameters.raw)?;
    word(output, base, 1, facts.signature.results.raw)?;
    word(output, base, 2, u32::from(facts.signature.variadic))?;
    word(output, base, 3, facts.type_parameters.raw)?;
    word(output, base, 4, facts.fields.raw)?;
    word(output, base, 5, facts.method_set.raw)?;
    word(output, base, 6, facts.build_constraints.raw)
}
fn encode_rust(
    output: &mut [u8],
    base: usize,
    facts: crate::RustFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(
        output,
        base,
        0,
        match facts.ownership {
            crate::RustOwnership::Value => 0,
            crate::RustOwnership::SharedBorrow => 1,
            crate::RustOwnership::MutableBorrow => 2,
            crate::RustOwnership::Moved => 3,
        },
    )?;
    word(output, base, 1, facts.lifetimes.raw)?;
    word(output, base, 2, facts.where_clauses.raw)?;
    word(output, base, 3, facts.macros.raw)
}
fn encode_python(
    output: &mut [u8],
    base: usize,
    facts: crate::PythonFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.decorators.raw)?;
    word(
        output,
        base,
        1,
        match facts.parameter_kind {
            crate::PythonParameterKind::PositionalOnly => 0,
            crate::PythonParameterKind::PositionalOrKeyword => 1,
            crate::PythonParameterKind::VariadicPositional => 2,
            crate::PythonParameterKind::KeywordOnly => 3,
            crate::PythonParameterKind::VariadicKeyword => 4,
        },
    )?;
    word(
        output,
        base,
        2,
        match facts.dynamic_confidence {
            crate::Confidence::Syntactic => 0,
            crate::Confidence::Heuristic => 1,
            crate::Confidence::Indexed => 2,
            crate::Confidence::Imported => 3,
            crate::Confidence::Compiler => 4,
        },
    )
}
fn encode_java(
    output: &mut [u8],
    base: usize,
    facts: crate::JavaFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.throws.raw)?;
    word(output, base, 1, facts.annotations.raw)?;
    word(output, base, 2, facts.overloads.raw)?;
    word(output, base, 3, facts.record_components.raw)
}
fn encode_clang(
    output: &mut [u8],
    base: usize,
    facts: crate::ClangFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(
        output,
        base,
        0,
        u32::from(facts.qualifiers.is_const)
            | u32::from(facts.qualifiers.is_volatile) << 1
            | u32::from(facts.qualifiers.is_restrict) << 2,
    )?;
    word(
        output,
        base,
        1,
        match facts.storage {
            crate::ClangStorageClass::None => 0,
            crate::ClangStorageClass::Auto => 1,
            crate::ClangStorageClass::Static => 2,
            crate::ClangStorageClass::Extern => 3,
            crate::ClangStorageClass::Register => 4,
            crate::ClangStorageClass::ThreadLocal => 5,
        },
    )?;
    word(output, base, 2, facts.layout.size_bits.unwrap_or(NONE))?;
    word(output, base, 3, facts.layout.align_bits.unwrap_or(NONE))?;
    word(output, base, 4, facts.templates.raw)?;
    word(output, base, 5, facts.includes.raw)
}

fn validate_fact(
    i: &[u8],
    b: usize,
    k: LanguageExtensionDirectoryKind,
    f: u32,
    n: LanguageExtensionCommonBounds,
) -> Result<(), LanguageExtensionReopenError> {
    let w = |x: usize| {
        let offset = x
            .checked_mul(4)
            .and_then(|width| b.checked_add(width))
            .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: b })?;
        get_u32(i, offset)
    };
    validate_fact_encoding(k, f, &w)?;
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

fn validate_fact_encoding(
    kind: LanguageExtensionDirectoryKind,
    fact: u32,
    word: &impl Fn(usize) -> Result<u32, LanguageExtensionReopenError>,
) -> Result<(), LanguageExtensionReopenError> {
    let reject = |word_index: u8, observed: u32| {
        Err(LanguageExtensionReopenError::FactEncoding {
            kind,
            fact,
            word: word_index,
            observed,
        })
    };
    match kind {
        LanguageExtensionDirectoryKind::TypeScript | LanguageExtensionDirectoryKind::Java => {}
        LanguageExtensionDirectoryKind::CSharp => {
            let nullability = word(0)?;
            if nullability > 2 {
                return reject(0, nullability);
            }
            let reference_kind = word(1)?;
            if reference_kind > 3 {
                return reject(1, reference_kind);
            }
            let effects = word(2)?;
            if effects & !7 != 0 {
                return reject(2, effects);
            }
            let partial = word(3)?;
            if partial > 2 {
                return reject(3, partial);
            }
            let file = word(6)?;
            let start = word(7)?;
            let end = word(8)?;
            if (file == NONE && (start != 0 || end != 0)) || (file != NONE && start > end) {
                return Err(LanguageExtensionReopenError::SourceSpanEncoding {
                    kind,
                    fact,
                    file,
                    start,
                    end,
                });
            }
        }
        LanguageExtensionDirectoryKind::Go => {
            let variadic = word(2)?;
            if variadic > 1 {
                return reject(2, variadic);
            }
        }
        LanguageExtensionDirectoryKind::Rust => {
            let ownership = word(0)?;
            if ownership > 3 {
                return reject(0, ownership);
            }
        }
        LanguageExtensionDirectoryKind::Python => {
            let parameter_kind = word(1)?;
            if parameter_kind > 4 {
                return reject(1, parameter_kind);
            }
            let confidence = word(2)?;
            if confidence > 4 {
                return reject(2, confidence);
            }
        }
        LanguageExtensionDirectoryKind::Clang => {
            let qualifiers = word(0)?;
            if qualifiers & !7 != 0 {
                return reject(0, qualifiers);
            }
            let storage = word(1)?;
            if storage > 5 {
                return reject(1, storage);
            }
            let alignment = word(3)?;
            if alignment == 0 {
                return reject(3, alignment);
            }
        }
    }
    Ok(())
}
