//! Cross-fragment reference and declaration-identity vocabulary.
//!
//! A declaration's stable identity is minted from producer-visible facts
//! alone — `(package lineage, path, kind, name, disambiguator)` — hashed
//! through the central `heart/identity` domain conventions
//! (`ContentId<SourceFactDomain>`). There is deliberately **no ordinal
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

use heart_identity::{ContentId, SourceFactDomain};

use crate::entity::EntityKind;

/// Purpose tag naming the declaration-key preimage inside the shared source
/// fact domain. Source identities hash raw source bytes in the same domain;
/// this prefix keeps the two preimage families non-colliding by construction.
const DECLARATION_KEY_PURPOSE: &[u8] = b"compiler.declaration.v1";
/// Purpose tag naming the foreign-key digest preimage inside the shared
/// source fact domain.
const FOREIGN_KEY_PURPOSE: &[u8] = b"compiler.foreign-key.v1";

/// A package lineage: ecosystem plus package name, stable across all
/// generations (for example `cargo:serde`). This — not any content digest —
/// keys declaration identities, so identities never depend on the fragment
/// bytes they are later embedded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLineage<'bytes> {
    /// Ecosystem registry name (`cargo`, `npm`, `pypi`, `go`, `nuget`,
    /// `maven`, ...).
    pub ecosystem: &'bytes str,
    /// Package name inside that ecosystem.
    pub name: &'bytes str,
}

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
        Ok(Self { ecosystem, name })
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

/// Collision-scoped disambiguator for sibling declarations that share the
/// complete `(lineage, path, kind, name)` key.
///
/// `None` is the common unique case, so a unique declaration's identity is
/// signature-stable. `Skeleton` carries producer-authored structural bytes
/// (generics, wheres, overload signature shape) that keep real overloads and
/// impls distinct.
///
/// There is deliberately **no ordinal variant** (identities must survive
/// declaration reordering — the measured 32,339-group defect) and **no span
/// variant** (spans are presentation, and degenerate `0..0` spans once
/// collapsed ~24,000 declaration groups).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disambiguator<'bytes> {
    /// The unique, signature-stable case.
    None,
    /// Producer-authored structural skeleton bytes (for example an overload
    /// or impl skeleton) that distinguish same-key siblings by content.
    Skeleton(&'bytes [u8]),
}

impl<'bytes> Disambiguator<'bytes> {
    const NONE_TAG: u8 = 0;
    const SKELETON_TAG: u8 = 1;

    /// The stable preimage tag of this disambiguator.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::None => Self::NONE_TAG,
            Self::Skeleton(_) => Self::SKELETON_TAG,
        }
    }
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
    /// The skeleton exceeds the fixed `u32` length cell.
    SkeletonTooLong {
        /// Observed skeleton byte length.
        actual: usize,
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

    /// Complete canonical preimage length for `disambiguator`.
    #[must_use]
    pub const fn preimage_len(&self, disambiguator: Disambiguator<'bytes>) -> usize {
        let mut length = str_cell_len(DECLARATION_KEY_PURPOSE);
        length += str_cell_len(self.lineage.ecosystem.as_bytes());
        length += str_cell_len(self.lineage.name.as_bytes());
        length += str_cell_len(self.path.as_bytes());
        length += str_cell_len(self.name);
        length += KEY_TAIL_BYTES;
        if let Disambiguator::Skeleton(skeleton) = disambiguator {
            length += str_cell_len(skeleton);
        }
        length
    }

    /// Writes the complete canonical preimage into `out` and returns the
    /// exact byte count written. Every variable-length field is
    /// length-prefixed, so distinct keys never share a preimage by
    /// concatenation. Output is written only after the total-length
    /// preflight; a short output leaves every byte untouched.
    pub fn write_preimage(
        &self,
        disambiguator: Disambiguator<'bytes>,
        out: &mut [u8],
    ) -> Result<usize, PreimageOverflow> {
        let needed = self.preimage_len(disambiguator);
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
        // Fixed tail: kind discriminant, disambiguator tag, one reserved
        // zero byte, skeleton length cell. Reserved zero keeps the tail
        // width-stable if a second skeleton-class disambiguator ever lands.
        let (tag, skeleton_len): (u8, u32) = match disambiguator {
            Disambiguator::None => (Disambiguator::NONE_TAG, 0),
            Disambiguator::Skeleton(skeleton) => {
                let Ok(len) = u32::try_from(skeleton.len()) else {
                    return Err(PreimageOverflow::SkeletonTooLong {
                        actual: skeleton.len(),
                    });
                };
                (Disambiguator::SKELETON_TAG, len)
            }
        };
        let mut tail = [0_u8; KEY_TAIL_BYTES];
        tail[..2].copy_from_slice(&u16::from(self.kind).to_le_bytes());
        tail[2] = tag;
        tail[4..].copy_from_slice(&skeleton_len.to_le_bytes());
        out[cursor..cursor + KEY_TAIL_BYTES].copy_from_slice(&tail);
        cursor += KEY_TAIL_BYTES;
        if let Disambiguator::Skeleton(skeleton) = disambiguator {
            cursor = write_str_cell(out, cursor, skeleton)?;
        }
        Ok(cursor)
    }

    /// Mints the declaration's stable identity through the central
    /// `heart/identity` domain conventions. `out` is caller-owned scratch
    /// holding the canonical preimage; see [`DeclarationKey::write_preimage`]
    /// for the exact width requirement.
    pub fn stable_id(
        &self,
        disambiguator: Disambiguator<'bytes>,
        out: &mut [u8],
    ) -> Result<ContentId<SourceFactDomain>, PreimageOverflow> {
        let written = self.write_preimage(disambiguator, out)?;
        Ok(ContentId::<SourceFactDomain>::from_canonical_bytes(
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

/// Wire-stable cross-fragment declaration reference: the owning fragment's
/// typed artifact identity plus the target declaration's stable identity.
///
/// This is the **only** cross-fragment reference form. Both cells are
/// independently valid typed identities; containment (the entity id naming a
/// declaration inside that fragment) is proven at resolution time by the
/// owning fragment, not at construction — references are honest data that a
/// resolver validates.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableRef {
    /// Typed identity of the fragment that declares the target.
    pub fragment: crate::ExternalFragmentId,
    /// Stable declaration identity inside that fragment.
    pub entity: ContentId<SourceFactDomain>,
}

impl StableRef {
    /// Complete canonical byte width: two 32-byte identity cells.
    pub const CANONICAL_BYTES: usize = 64;
}

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

impl<'bytes> ForeignOrigin<'bytes> {
    const PACKAGE_TAG: u8 = 0;
    const NAMESPACE_TAG: u8 = 1;
    const UNIVERSE_TAG: u8 = 2;

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
    #[must_use]
    pub fn key_preimage_len(&self) -> usize {
        let mut length = str_cell_len(FOREIGN_KEY_PURPOSE);
        length += ORIGIN_CELL;
        match self.origin {
            ForeignOrigin::Package(lineage) => {
                length += str_cell_len(lineage.ecosystem.as_bytes());
                length += str_cell_len(lineage.name.as_bytes());
            }
            ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } => {
                length += str_cell_len(ecosystem.as_bytes());
                length += str_cell_len(namespace.as_bytes());
            }
            ForeignOrigin::Universe { ecosystem } => {
                length += str_cell_len(ecosystem.as_bytes());
            }
        }
        length += str_cell_len(self.path.as_bytes());
        length
    }

    /// Writes the canonical key-digest preimage, excluding `display`.
    /// Output is written only after the total-length preflight.
    pub fn write_key_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.key_preimage_len();
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
    /// through the central identity domain, so sealing with and without
    /// dependencies loaded is byte-identical.
    pub fn key_id(&self, out: &mut [u8]) -> Result<ContentId<SourceFactDomain>, PreimageOverflow> {
        let written = self.write_key_preimage(out)?;
        Ok(ContentId::<SourceFactDomain>::from_canonical_bytes(
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
    pub kind: crate::occurrence::ReferenceKind,
    /// The fidelity of the resolution.
    pub confidence: crate::occurrence::Confidence,
    /// The byte range of the reference site, relative to the owning
    /// declaration's span start.
    pub span: crate::occurrence::RelSpan,
}

/// Byte width of one length-prefixed string cell: u32 length + bytes.
const fn str_cell_len(bytes: &[u8]) -> usize {
    4 + bytes.len()
}
/// Byte width of the fixed key tail: kind u16 + tag u8 + reserved u8 +
/// skeleton length u32.
const KEY_TAIL_BYTES: usize = 8;
/// Byte width of the fixed origin cell: origin tag u8 + kind cell u16 +
/// reserved u8.
const ORIGIN_CELL: usize = 4;

/// Writes one length-prefixed byte cell at `cursor`; returns the next
/// cursor. The caller preflighted the total length, so the bounds checks
/// below are proven in bounds; a failed check names the exact shortfall.
fn write_str_cell(out: &mut [u8], cursor: usize, bytes: &[u8]) -> Result<usize, PreimageOverflow> {
    let Ok(len) = u32::try_from(bytes.len()) else {
        return Err(PreimageOverflow::SkeletonTooLong {
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
