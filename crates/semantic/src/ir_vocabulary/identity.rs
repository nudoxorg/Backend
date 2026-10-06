//! Cross-fragment reference and declaration-identity vocabulary.
//!
//! A declaration's stable identity is minted from producer-visible facts
//! alone — `(package lineage, path, kind, name)` — hashed
//! through dedicated declaration identity domains. There is deliberately **no ordinal
//! disambiguator and no span disambiguator**: the old system's measured
//! 32,339 ordinal-keyed identity groups (446,947 declarations, 79 package
//! versions) and its degenerate `0..0` span era are the defects this module
//! exists to never reproduce.
//!
//! [`StableRef`] is the only cross-fragment reference form; [`ForeignKey`]
//! is the self-describing placeholder a producer records when the target is
//! not loaded. Foreign keys hash their **key cells, never a resolved
//! target**: sealing with and without dependencies loaded is byte-identical,
//! so `display` is excluded from the key digest.

use core::{mem::size_of, ops::Deref};

use backend_version::{
    ContentId, DeclarationFamilyDomain, DeclarationKeyDomain, DeclarationVariantDomain,
    ForeignDeclarationDomain,
};

use crate::ir_vocabulary::coordinates::EntityId;
use crate::ir_vocabulary::entity::EntityKind;

/// Purpose tag naming the declaration-key preimage inside its dedicated
/// declaration-key identity domain.
const DECLARATION_KEY_PURPOSE: &[u8] = b"compiler.declaration.v2";
/// Purpose tag naming the foreign-key digest preimage inside the shared
/// foreign-declaration identity domain.
const FOREIGN_KEY_PURPOSE: &[u8] = b"compiler.foreign-key.v1";

/// A package lineage: ecosystem plus package name, stable across all
/// generations (for example `cargo:serde`). This — not any content digest —
/// keys declaration identities, so identities never depend on the fragment
/// bytes they are later embedded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLineageView<'bytes> {
    /// Ecosystem registry name (`cargo`, `npm`, `pypi`, `go`, `nuget`,
    /// `maven`, ...).
    pub ecosystem: &'bytes str,
    /// Package name inside that ecosystem.
    pub name: &'bytes str,
}

/// Validated package lineage used by every identity-bearing key.  The raw
/// fields live in [`PackageLineageView`], which can only enter this owner
/// through [`PackageLineage::new`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLineage<'bytes>(PackageLineageView<'bytes>);

/// Exact lineage rejection retaining the offending part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageLineageFault {
    /// The ecosystem segment is empty.
    EmptyEcosystem,
    /// The package name is empty.
    EmptyName,
    /// The ecosystem segment contains the lineage render separator `:`.
    SeparatorInEcosystem,
    /// The package name contains the lineage render separator `:`.
    SeparatorInName,
    /// A segment contains a path separator, which would make keyed paths
    /// ambiguous across ecosystems.
    Backslash {
        /// Offending segment (`0` = ecosystem, `1` = package name).
        segment: u8,
    },
}

impl<'bytes> PackageLineage<'bytes> {
    /// Validates one lineage: both segments non-empty, neither containing
    /// the `ecosystem:name` render separator or a path separator.
    pub const fn new(
        ecosystem: &'bytes str,
        name: &'bytes str,
    ) -> Result<Self, PackageLineageFault> {
        if ecosystem.is_empty() {
            return Err(PackageLineageFault::EmptyEcosystem);
        }
        if name.is_empty() {
            return Err(PackageLineageFault::EmptyName);
        }
        if contains_colon(ecosystem) {
            return Err(PackageLineageFault::SeparatorInEcosystem);
        }
        if contains_colon(name) {
            return Err(PackageLineageFault::SeparatorInName);
        }
        if contains_backslash(ecosystem.as_bytes()) {
            return Err(PackageLineageFault::Backslash { segment: 0 });
        }
        if contains_backslash(name.as_bytes()) {
            return Err(PackageLineageFault::Backslash { segment: 1 });
        }
        Ok(Self(PackageLineageView { ecosystem, name }))
    }
}

impl<'bytes> Deref for PackageLineage<'bytes> {
    type Target = PackageLineageView<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

const fn contains_colon(text: &str) -> bool {
    contains_byte(text.as_bytes(), b':')
}

const fn contains_backslash(bytes: &[u8]) -> bool {
    contains_byte(bytes, b'\\')
}

const fn contains_byte(bytes: &[u8], needle: u8) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == needle {
            return true;
        }
        index += 1;
    }
    false
}

/// Exact declaration-path rejection retaining the observed operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationPathFault {
    /// The package-relative path is empty.
    Empty,
    /// The path contains a backslash; callers normalize to `/` first.
    Backslash,
}

/// Every producer-visible fact that mints one declaration's stable identity.
///
/// The identity is `(lineage, path, kind, name)`-keyed — never ordinal-keyed
/// and never content-of-fragment-keyed — so two compilations that declare
/// the same fact in a different order, or with different neighbors, mint the
/// identical id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationKey<'bytes> {
    /// Owning package lineage.
    pub lineage: PackageLineage<'bytes>,
    /// Package-relative, `/`-separated source path of the declaring file.
    pub path: &'bytes str,
    /// Declaration kind discriminant.
    pub kind: EntityKind,
    /// Exact declaration name bytes.
    pub name: &'bytes [u8],
}

/// Exact preimage-write rejection retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreimageOverflow {
    /// The caller output cannot hold the complete preimage; `needed` is the
    /// complete canonical length and `actual` the supplied width.
    OutputShort {
        /// Complete canonical preimage length the caller must provide room for.
        needed: usize,
        /// Output width the caller actually supplied.
        actual: usize,
    },
    /// A nested canonical byte cell exceeds its fixed `u32` length field.
    CellTooLong {
        /// Observed nested-cell byte length.
        actual: usize,
    },
    /// Adding one validated cell would overflow the platform's addressable
    /// preimage width.  The exact partial total and requested cell width are
    /// retained before any caller allocates scratch.
    AggregateTooLong {
        /// Canonical bytes already accounted for.
        accumulated: usize,
        /// Width of the next complete cell or fixed segment.
        additional: usize,
    },
}

impl<'bytes> DeclarationKey<'bytes> {
    /// Validates one declaration key, proving the path is a non-empty,
    /// `/`-separated package-relative path and the name is non-empty.
    pub const fn new(
        lineage: PackageLineage<'bytes>,
        path: &'bytes str,
        kind: EntityKind,
        name: &'bytes [u8],
    ) -> Result<Self, DeclarationKeyFault> {
        // `PackageLineage` is an opaque validated owner.  Keep this explicit
        // constructor boundary so every declaration key remains the one
        // place that proves its package fact before it can mint identity.
        if path.is_empty() {
            return Err(DeclarationKeyFault::Path(DeclarationPathFault::Empty));
        }
        if contains_backslash(path.as_bytes()) {
            return Err(DeclarationKeyFault::Path(DeclarationPathFault::Backslash));
        }
        if name.is_empty() {
            return Err(DeclarationKeyFault::EmptyName);
        }
        Ok(Self {
            lineage,
            path,
            kind,
            name,
        })
    }

    /// Complete family-only canonical preimage length.
    pub fn preimage_len(&self) -> Result<usize, PreimageOverflow> {
        let mut length = cell_len(DECLARATION_KEY_PURPOSE)?;
        length = add_preimage_len(length, cell_len(self.lineage.ecosystem.as_bytes())?)?;
        length = add_preimage_len(length, cell_len(self.lineage.name.as_bytes())?)?;
        length = add_preimage_len(length, cell_len(self.path.as_bytes())?)?;
        length = add_preimage_len(length, cell_len(self.name)?)?;
        add_preimage_len(length, KEY_TAIL_BYTES)
    }

    /// Writes the complete canonical preimage into `out` and returns the
    /// exact byte count written. Every variable-length field is
    /// length-prefixed, so distinct keys never share a preimage by
    /// concatenation. Output is written only after the total-length
    /// preflight; a short output leaves every byte untouched.
    pub fn write_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let mut cursor = 0;
        cursor = write_str_cell(out, cursor, DECLARATION_KEY_PURPOSE)?;
        cursor = write_str_cell(out, cursor, self.lineage.ecosystem.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.lineage.name.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.path.as_bytes())?;
        cursor = write_str_cell(out, cursor, self.name)?;
        out[cursor..cursor + KEY_TAIL_BYTES].copy_from_slice(&u16::from(self.kind).to_le_bytes());
        cursor += KEY_TAIL_BYTES;
        Ok(cursor)
    }

    /// Mints the declaration's stable identity through the central
    /// `heart/identity` domain conventions. `out` is caller-owned scratch
    /// holding the canonical preimage; see [`DeclarationKey::write_preimage`]
    /// for the exact width requirement.
    pub fn stable_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<DeclarationKeyDomain>, PreimageOverflow> {
        let written = self.write_preimage(out)?;
        Ok(ContentId::<DeclarationKeyDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

/// Exact declaration-key rejection retaining the offending part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKeyFault {
    /// The package-relative path is malformed.
    Path(DeclarationPathFault),
    /// The declaration name is empty.
    EmptyName,
}

/// Coordinate-free declaration family identity. It owns only stable scope,
/// profile, parentage, kind, and name; overload instances may share it.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeclarationFamilyId([u8; 16]);

impl DeclarationFamilyId {
    /// Reconstitutes the compact family identity from its raw 16-byte payload.
    /// This does not hash or validate the supplied bytes.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Hashes the canonical family preimage in `DeclarationFamilyDomain` and
    /// retains its compact 16-byte identity payload.
    /// Narrows one dedicated declaration-family digest for the compact live
    /// IR lane. The domain remains present in the full content identity at
    /// the mint boundary; this compact value is never a source digest.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let digest = ContentId::<DeclarationFamilyDomain>::from_canonical_bytes(bytes);
        Self::from_content_id(digest)
    }

    /// Narrows an already domain-validated declaration-family content id
    /// without accidentally retaining its wire-domain byte as entropy.
    #[must_use]
    pub fn from_content_id(value: ContentId<DeclarationFamilyDomain>) -> Self {
        Self(compact_identity_payload(value.as_ref()))
    }

    /// Returns the compact 16-byte family payload, without a content-domain tag.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Coordinate-free structural fingerprint for every declaration instance.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VariantFingerprint([u8; 16]);

impl VariantFingerprint {
    /// Reconstitutes the structural fingerprint from its raw 16-byte payload.
    /// This does not hash or validate the supplied bytes.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Hashes the canonical declaration-variant preimage in
    /// `DeclarationVariantDomain` and retains its compact 16-byte payload.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let digest = ContentId::<DeclarationVariantDomain>::from_canonical_bytes(bytes);
        Self(compact_identity_payload(digest.as_ref()))
    }

    /// Returns the compact 16-byte structural fingerprint payload.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Compact unresolved foreign declaration key. It is deliberately not a
/// [`DeclarationFamilyId`]: a foreign authority key cannot be substituted
/// for a locally minted lexical declaration family.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ForeignDeclarationId([u8; 16]);

impl ForeignDeclarationId {
    /// Reconstitutes the unresolved foreign-key fingerprint from its raw
    /// 16-byte payload, without deriving or validating it.
    #[must_use]
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Hashes the canonical foreign-key preimage in `ForeignDeclarationDomain`
    /// and retains its compact 16-byte fingerprint.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self::from_content_id(ContentId::<ForeignDeclarationDomain>::from_canonical_bytes(
            bytes,
        ))
    }

    /// Narrows a foreign-key content id without mixing its wire domain byte
    /// into the compact fingerprint.
    #[must_use]
    pub fn from_content_id(value: ContentId<ForeignDeclarationDomain>) -> Self {
        Self(compact_identity_payload(value.as_ref()))
    }

    /// Returns the compact 16-byte foreign-key fingerprint payload.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact current-generation local declaration endpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeclarationIdentity {
    /// Stable declaration family shared by structurally distinct instances such as overloads.
    pub family: DeclarationFamilyId,
    /// Structural fingerprint distinguishing this exact declaration instance within its family.
    pub variant: VariantFingerprint,
}

/// Variant knowledge retained for an unresolved foreign declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VariantAvailability {
    /// The foreign target's exact structural declaration fingerprint is known.
    Known(VariantFingerprint),
    /// The target is unresolved and no variant fingerprint was available to record.
    Unavailable,
}

/// Cross-package declaration identity without fabricating a local variant.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExternalDeclarationIdentity {
    /// Exact unresolved foreign-key fingerprint, never a local family.
    pub foreign: ForeignDeclarationId,
    /// Known target variant, or an explicit marker that its variant is unavailable.
    pub variant: VariantAvailability,
}

/// Wire-stable cross-fragment declaration reference: the owning fragment's
/// typed artifact identity plus the target's exact composite declaration identity.
///
/// This is the **only** cross-fragment reference form. Both cells are
/// independently valid typed identities; containment (the entity id naming a
/// declaration inside that fragment) is proven at resolution time by the
/// owning fragment, not at construction — references are honest data that a
/// resolver validates.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableRef {
    /// Typed identity of the fragment that declares the target.
    pub fragment: crate::ir_vocabulary::ExternalFragmentId,
    /// Exact declaration endpoint inside that fragment.
    pub declaration: DeclarationIdentity,
}

impl StableRef {
    /// Complete canonical byte width: one 32-byte fragment identity plus
    /// two compact 16-byte declaration identity cells.
    pub const CANONICAL_BYTES: usize = 64;
}

const _: () = assert!(size_of::<StableRef>() == StableRef::CANONICAL_BYTES);

/// Where an unresolved foreign target lives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForeignOrigin<'bytes> {
    /// Another loaded package lineage.
    Package(PackageLineage<'bytes>),
    /// A namespace outside any package lineage (`java.util`,
    /// assembly-qualified .NET namespaces).
    Namespace {
        /// Ecosystem registry name.
        ecosystem: &'bytes str,
        /// Namespace spelling.
        namespace: &'bytes str,
    },
    /// A universe scope outside every package (`error`, `comparable` in Go;
    /// C builtins).
    Universe {
        /// Ecosystem registry name.
        ecosystem: &'bytes str,
    },
}

/// Ecosystem discriminator for a TypeScript reference whose target is known
/// by a source coordinate in the same admitted TSZ program. The external
/// target remains a [`ForeignKey`] in the canonical occurrence lane, while
/// project query joins can distinguish this typed coordinate from a package
/// or a name-only universe reference.
pub const TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM: &str = "typescript-tsz-source-v1";

/// One exact TypeScript declaration-name coordinate in a compiled source
/// file. The coordinate is a byte offset in the admitted UTF-8 source, not a
/// line number, node ordinal, symbol name, or guessed module target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeScriptSourceCoordinate<'source> {
    /// Deterministic identity of the complete admitted project source set.
    pub program: [u8; 32],
    /// Content identity of the target source file in that project.
    pub source: [u8; 32],
    /// Stable project-relative source path supplied to the TSZ project.
    pub path: &'source str,
    /// Start of the exact target declaration span in UTF-8 bytes.
    pub declaration_start: u32,
    /// End of the exact target declaration span in UTF-8 bytes.
    pub declaration_end: u32,
    /// Byte offset of the declaration name token in that exact file.
    pub name_start: u32,
}

impl<'source> TypeScriptSourceCoordinate<'source> {
    const PREFIX: &'static str = "tsz-source-coordinate-v2:";

    /// Encodes a coordinate into the existing validated foreign-key path
    /// cell. A byte-length prefix keeps paths containing colons or Unicode
    /// unambiguous without introducing a second persisted operand.
    #[must_use]
    pub fn encode(self) -> Option<String> {
        if self.path.is_empty()
            || self.path.contains('\\')
            || self.path.contains('\0')
            || self.declaration_start >= self.declaration_end
            || self.name_start < self.declaration_start
            || self.name_start >= self.declaration_end
        {
            return None;
        }
        Some(format!(
            "{}{}:{}:{}:{}:{}:{}:{}",
            Self::PREFIX,
            hex_digest(&self.program),
            hex_digest(&self.source),
            self.path.len(),
            self.path,
            self.declaration_start,
            self.declaration_end,
            self.name_start
        ))
    }

    /// Decodes only this closed versioned source-coordinate grammar.
    #[must_use]
    pub fn decode(encoded: &'source str) -> Option<Self> {
        let rest = encoded.strip_prefix(Self::PREFIX)?;
        let (program, rest) = rest.split_once(':')?;
        let program = parse_hex_digest(program)?;
        let (source, rest) = rest.split_once(':')?;
        let source = parse_hex_digest(source)?;
        let (path_len, rest) = rest.split_once(':')?;
        let path_len = path_len.parse::<usize>().ok()?;
        let path = rest.get(..path_len)?;
        let (declaration_start, rest) = rest.get(path_len..)?.strip_prefix(':')?.split_once(':')?;
        let (declaration_end, name_start) = rest.split_once(':')?;
        let declaration_start = declaration_start.parse().ok()?;
        let declaration_end = declaration_end.parse().ok()?;
        let name_start = name_start.parse().ok()?;
        if path.is_empty()
            || path.contains('\\')
            || path.contains('\0')
            || declaration_start >= declaration_end
            || name_start < declaration_start
            || name_start >= declaration_end
        {
            return None;
        }
        Some(Self {
            program,
            source,
            path,
            declaration_start,
            declaration_end,
            name_start,
        })
    }
}

/// Hashes one exact project source manifest. Duplicate paths are rejected,
/// rather than allowing source order or accidental last-writer wins to
/// decide which program a cross-file reference names.
#[must_use]
pub fn typescript_program_identity(sources: &[(String, [u8; 32])]) -> Option<[u8; 32]> {
    if sources.is_empty() {
        return None;
    }
    let mut ordered = sources.to_vec();
    ordered.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"typescript-tsz-program-source-manifest-v1\0");
    for (path, source) in ordered {
        let length = u32::try_from(path.len()).ok()?;
        hasher.update(&length.to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&source);
    }
    Some(*hasher.finalize().as_bytes())
}

fn hex_digest(digest: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest {
        use core::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn parse_hex_digest(digest: &str) -> Option<[u8; 32]> {
    if digest.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = u8::from_str_radix(digest.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    if hex_digest(&decoded) != digest {
        return None;
    }
    Some(decoded)
}

impl<'bytes> ForeignOrigin<'bytes> {
    /// Stable wire tag of the [`ForeignOrigin::Package`] family.
    pub const PACKAGE_TAG: u8 = 0;
    /// Stable wire tag of the [`ForeignOrigin::Namespace`] family.
    pub const NAMESPACE_TAG: u8 = 1;
    /// Stable wire tag of the [`ForeignOrigin::Universe`] family.
    pub const UNIVERSE_TAG: u8 = 2;

    /// The stable preimage tag of this origin.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Package(_) => Self::PACKAGE_TAG,
            Self::Namespace { .. } => Self::NAMESPACE_TAG,
            Self::Universe { .. } => Self::UNIVERSE_TAG,
        }
    }
}

/// Everything a producer knows about a foreign target at the reference site.
///
/// `display` is exactly what renders for an unlinked reference; it is
/// deliberately **excluded from the key digest** so sealing with and without
/// dependencies loaded stays byte-identical.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForeignKey<'bytes> {
    /// Where the target lives.
    pub origin: ForeignOrigin<'bytes>,
    /// Canonical cross-package path of the target.
    pub path: &'bytes str,
    /// Human display spelling at the reference site.
    pub display: &'bytes str,
    /// Expected declaration kind, when the producer knows one.
    pub kind: Option<EntityKind>,
}

/// Exact foreign-key rejection retaining the offending operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForeignKeyFault {
    /// The canonical path is empty.
    EmptyPath,
    /// The canonical path contains a backslash; callers normalize to `/`.
    BackslashInPath,
}

impl<'bytes> ForeignKey<'bytes> {
    /// Validates one foreign key: the canonical path is non-empty and
    /// `/`-separated. `display` may be empty (the producer may honestly know
    /// nothing worth displaying beyond the path).
    pub const fn new(
        origin: ForeignOrigin<'bytes>,
        path: &'bytes str,
        display: &'bytes str,
        kind: Option<EntityKind>,
    ) -> Result<Self, ForeignKeyFault> {
        if path.is_empty() {
            return Err(ForeignKeyFault::EmptyPath);
        }
        if contains_backslash(path.as_bytes()) {
            return Err(ForeignKeyFault::BackslashInPath);
        }
        Ok(Self {
            origin,
            path,
            display,
            kind,
        })
    }

    /// Complete canonical key-digest preimage length.
    pub fn key_preimage_len(&self) -> Result<usize, PreimageOverflow> {
        let mut length = cell_len(FOREIGN_KEY_PURPOSE)?;
        length = add_preimage_len(length, ORIGIN_CELL)?;
        match self.origin {
            ForeignOrigin::Package(lineage) => {
                length = add_preimage_len(length, cell_len(lineage.ecosystem.as_bytes())?)?;
                length = add_preimage_len(length, cell_len(lineage.name.as_bytes())?)?;
            }
            ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                length = add_preimage_len(length, cell_len(ecosystem.as_bytes())?)?;
                length = add_preimage_len(length, cell_len(namespace.as_bytes())?)?;
            }
            ForeignOrigin::Universe { ecosystem } => {
                length = add_preimage_len(length, cell_len(ecosystem.as_bytes())?)?;
            }
        }
        add_preimage_len(length, cell_len(self.path.as_bytes())?)
    }

    /// Writes the canonical key-digest preimage, excluding `display`.
    /// Output is written only after the total-length preflight.
    pub fn write_key_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.key_preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let mut cursor = 0;
        cursor = write_str_cell(out, cursor, FOREIGN_KEY_PURPOSE)?;
        // Fixed origin cell: origin tag, kind cell biased by one so an
        // unknown kind (`None`) and kind `Function` never share a cell.
        let kind_cell = match self.kind {
            None => 0_u16,
            Some(kind) => u16::from(kind) + 1,
        };
        let mut origin = [0_u8; ORIGIN_CELL];
        origin[0] = self.origin.tag();
        origin[1..3].copy_from_slice(&kind_cell.to_le_bytes());
        out[cursor..cursor + ORIGIN_CELL].copy_from_slice(&origin);
        cursor += ORIGIN_CELL;
        cursor = match self.origin {
            ForeignOrigin::Package(lineage) => {
                let after_ecosystem = write_str_cell(out, cursor, lineage.ecosystem.as_bytes())?;
                write_str_cell(out, after_ecosystem, lineage.name.as_bytes())?
            }
            ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                let after_ecosystem = write_str_cell(out, cursor, ecosystem.as_bytes())?;
                write_str_cell(out, after_ecosystem, namespace.as_bytes())?
            }
            ForeignOrigin::Universe { ecosystem } => {
                write_str_cell(out, cursor, ecosystem.as_bytes())?
            }
        };
        cursor = write_str_cell(out, cursor, self.path.as_bytes())?;
        Ok(cursor)
    }

    /// Digests the key cells (never a resolved target, never `display`)
    /// through the foreign-declaration identity domain, so sealing with and without
    /// dependencies loaded is byte-identical.
    pub fn key_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<ForeignDeclarationDomain>, PreimageOverflow> {
        let written = self.write_key_preimage(out)?;
        Ok(ContentId::<ForeignDeclarationDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

/// The outcome of resolving one [`ForeignKey`] against loaded dependencies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution {
    /// The target resolved to a stable declaration reference.
    Resolved(StableRef),
    /// The owning package is not loaded; the key remains self-describing.
    PackageNotLoaded,
    /// The package is loaded but names no such path.
    PathNotFound,
}

/// One reference fact's target: resolved, or a self-describing foreign key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OccurrenceTarget<'bytes> {
    /// A resolved cross-fragment declaration reference.
    Stable(StableRef),
    /// The producer could not resolve the target; the key travels instead.
    Foreign(ForeignKey<'bytes>),
    /// A declaration inside the same fragment as the reference.
    ///
    /// Same-fragment references never embed their own artifact identity: a
    /// fragment's content identity is derived from its written bytes, so a
    /// [`StableRef`](StableRef) naming the carrying fragment cannot appear
    /// inside those bytes. The local ordinal is proven against the entity
    /// lane by whichever lane admits the occurrence.
    Local(EntityId),
}

/// One reference fact: the owning declaration (implied by the containing
/// lane) references `target` via `kind` at `span`, resolved at
/// `confidence`.
///
/// The owner is not a field; it is implied by whichever lane holds this
/// value, keeping the record compact.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occurrence<'bytes> {
    /// The referenced declaration.
    pub target: OccurrenceTarget<'bytes>,
    /// The category of this reference.
    pub kind: crate::ir_vocabulary::occurrence::ReferenceKind,
    /// The fidelity of the resolution.
    pub confidence: crate::ir_vocabulary::occurrence::Confidence,
    /// The byte range of the reference site, relative to the owning
    /// declaration's span start.
    pub span: crate::ir_vocabulary::occurrence::RelSpan,
}

/// Checked byte width of one length-prefixed string cell.
fn cell_len(bytes: &[u8]) -> Result<usize, PreimageOverflow> {
    if u32::try_from(bytes.len()).is_err() {
        return Err(PreimageOverflow::CellTooLong {
            actual: bytes.len(),
        });
    }
    add_preimage_len(4, bytes.len())
}

/// Adds one already-validated canonical segment without allowing platform
/// width wraparound to become an undersized caller allocation.
fn add_preimage_len(accumulated: usize, additional: usize) -> Result<usize, PreimageOverflow> {
    accumulated
        .checked_add(additional)
        .ok_or(PreimageOverflow::AggregateTooLong {
            accumulated,
            additional,
        })
}
/// Byte width of the family-only fixed key tail: kind u16.
const KEY_TAIL_BYTES: usize = 2;
/// Byte width of the fixed origin cell: origin tag u8 + kind cell u16 +
/// reserved u8.
const ORIGIN_CELL: usize = 4;

/// Narrows sixteen payload bytes from a typed 32-byte content identity.
/// Byte zero is the domain authority, not digest entropy, so compact
/// identities retain bytes `1..=16` rather than spending one of their fixed
/// sixteen bytes on a constant tag.
fn compact_identity_payload(bytes: &[u8; backend_version::HASH_BYTES]) -> [u8; 16] {
    [
        bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16],
    ]
}

/// Writes one length-prefixed byte cell at `cursor`; returns the next
/// cursor. The caller preflighted the total length, so the bounds checks
/// below are proven in bounds; a failed check names the exact shortfall.
fn write_str_cell(out: &mut [u8], cursor: usize, bytes: &[u8]) -> Result<usize, PreimageOverflow> {
    let Ok(len) = u32::try_from(bytes.len()) else {
        return Err(PreimageOverflow::CellTooLong {
            actual: bytes.len(),
        });
    };
    let after = cursor
        .checked_add(4)
        .and_then(|cursor| cursor.checked_add(bytes.len()))
        .ok_or(PreimageOverflow::OutputShort {
            needed: usize::MAX,
            actual: out.len(),
        })?;
    let out_len = out.len();
    let cell = out
        .get_mut(cursor..after)
        .ok_or(PreimageOverflow::OutputShort {
            needed: after,
            actual: out_len,
        })?;
    let (length_cell, payload) = cell.split_at_mut(4);
    length_cell.copy_from_slice(&len.to_le_bytes());
    payload.copy_from_slice(bytes);
    Ok(after)
}

#[cfg(test)]
mod typescript_source_coordinate_tests {
    use super::TypeScriptSourceCoordinate;

    #[test]
    fn source_coordinate_round_trips_colons_and_utf8_by_byte_length() {
        let path = "src/über:service.ts";
        let encoded = TypeScriptSourceCoordinate {
            program: [4; 32],
            source: [5; 32],
            path,
            declaration_start: 30,
            declaration_end: 41,
            name_start: 37,
        }
        .encode()
        .expect("valid source coordinate");
        assert_eq!(
            TypeScriptSourceCoordinate::decode(&encoded),
            Some(TypeScriptSourceCoordinate {
                program: [4; 32],
                source: [5; 32],
                path,
                declaration_start: 30,
                declaration_end: 41,
                name_start: 37,
            })
        );
    }

    #[test]
    fn source_coordinate_rejects_noncanonical_paths_and_malformed_lengths() {
        assert!(
            TypeScriptSourceCoordinate {
                program: [4; 32],
                source: [5; 32],
                path: "src\\service.ts",
                declaration_start: 0,
                declaration_end: 1,
                name_start: 1,
            }
            .encode()
            .is_none()
        );
        assert!(TypeScriptSourceCoordinate::decode("tsz-source-coordinate-v2:999:a:1").is_none());
        assert!(TypeScriptSourceCoordinate::decode("tsz-source-coordinate-v2:3:a:b:1").is_none());
    }

    #[test]
    fn project_identity_commits_sorted_complete_sources_and_rejects_duplicate_paths() {
        let forward = typescript_program_identity(&[
            ("src/b.ts".to_owned(), [2; 32]),
            ("src/a.ts".to_owned(), [1; 32]),
        ]);
        let reverse = typescript_program_identity(&[
            ("src/a.ts".to_owned(), [1; 32]),
            ("src/b.ts".to_owned(), [2; 32]),
        ]);
        assert_eq!(forward, reverse);
        assert_ne!(
            forward,
            typescript_program_identity(&[("src/a.ts".to_owned(), [1; 32])])
        );
        assert!(
            typescript_program_identity(&[
                ("src/a.ts".to_owned(), [1; 32]),
                ("src/a.ts".to_owned(), [1; 32])
            ])
            .is_none()
        );
    }
}
