//! Typed declaration stable-key framing.
//!
//! This module owns only the profile and closed-parentage family preimage.
//! Structural variants are framed outside this key and never enter it.

use crate::vocabulary::LanguageProfile;
use backend_version::{ContentId, DeclarationFamilyDomain};

use crate::ir::{
    DeclarationIdentity, DeclarationName, PreimageOverflow, TypeScriptCallableSourceCoordinate,
    TypedDeclarationKey, VariantFingerprint,
};
use crate::ir_vocabulary::write_str_cell;

/// Purpose tag for declaration keys whose language profile and closed
/// containment fact are part of the declaration family. Variants are not
/// accepted by this writer.
const SCOPED_DECLARATION_KEY_PURPOSE: &[u8] = b"compiler.declaration-family.v2";
const SCOPED_DECLARATION_KEY_PURPOSE_LEN: u32 = 30;
const _: () =
    assert!(SCOPED_DECLARATION_KEY_PURPOSE.len() == SCOPED_DECLARATION_KEY_PURPOSE_LEN as usize);

/// Closed containment fact retained by one [`ScopedDeclarationKey`].
///
/// A bound parent uses its exact composite declaration identity rather than
/// any payload. Root and
/// unavailable use distinct tags; an unrepresented authority owner carries
/// the exact opaque identity instead of being collapsed into either state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationParentage {
    /// The authority proved that this declaration is a root.
    Root,
    /// The authority bound this declaration to one exact local parent instance.
    Bound(DeclarationIdentity),
    /// The authority supplied a non-local owner identity.
    Unrepresented([u8; 16]),
    /// The authority did not supply parentage.
    Unavailable,
}

impl DeclarationParentage {
    const fn tag(self) -> u8 {
        match self {
            Self::Root => 0,
            Self::Bound(_) => 1,
            Self::Unrepresented(_) => 2,
            Self::Unavailable => 3,
        }
    }

    const fn payload_len(self) -> usize {
        match self {
            Self::Bound(_) => 32,
            Self::Unrepresented(_) => 16,
            Self::Root | Self::Unavailable => 0,
        }
    }
}

/// Typed declaration-family key input. The profile remains a
/// `LanguageProfile`, so callers cannot mint a family from an arbitrary raw
/// profile code. Variant framing is deliberately separate from this scope key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedDeclarationKey<'bytes> {
    /// Validated package/path/kind/name fact.
    pub declaration: crate::ir::DeclarationKey<'bytes>,
    /// Semantic-authority language profile.
    pub profile: LanguageProfile,
    /// Explicit closed parentage fact.
    pub parentage: DeclarationParentage,
}

/// Scoped key for the typed declaration-name sum. Named declarations delegate
/// to [`ScopedDeclarationKey`] byte-for-byte. An anonymous family includes a
/// bound parent's family but keeps the exact parent variant for its current
/// instance key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedTypedDeclarationKey<'bytes> {
    declaration: TypedDeclarationKey<'bytes>,
    profile: LanguageProfile,
    parentage: DeclarationParentage,
}

/// Exact constructor failure for a current anonymous callable instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnonymousCallableInstanceFault {
    /// The stable family key does not name an anonymous callable.
    NotAnonymousCallable,
    /// The exact site belongs to a different path than the stable family.
    PathMismatch,
}

/// Current exact callable endpoint: a durable structural family plus its
/// checked source-site witness. Site coordinates distinguish duplicate
/// callbacks within the current source; they never enter the family key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnonymousCallableInstanceKey<'bytes> {
    family: ScopedTypedDeclarationKey<'bytes>,
    site: TypeScriptCallableSourceCoordinate<'bytes>,
}

const SCOPED_ANONYMOUS_CALLABLE_PURPOSE: &[u8] =
    b"compiler.declaration-family.anonymous-callable.v1";
const ANONYMOUS_CALLABLE_INSTANCE_PURPOSE: &[u8] =
    b"compiler.declaration-instance.anonymous-callable-site.v1";

impl<'bytes> ScopedDeclarationKey<'bytes> {
    /// Binds one validated declaration fact to a typed authority profile and
    /// closed parentage state.
    #[must_use]
    pub const fn new(
        declaration: crate::ir::DeclarationKey<'bytes>,
        profile: LanguageProfile,
        parentage: DeclarationParentage,
    ) -> Self {
        Self {
            declaration,
            profile,
            parentage,
        }
    }

    /// Complete checked family-preimage width. Every nested fixed-width cell and the
    /// aggregate are proved before a caller allocates or this writer mutates
    /// a byte.
    #[must_use]
    pub fn family_preimage_len(&self) -> Result<usize, PreimageOverflow> {
        let nested = self.declaration.preimage_len()?;
        let _ =
            u32::try_from(nested).map_err(|_| PreimageOverflow::CellTooLong { actual: nested })?;
        let mut length = checked_length(4, SCOPED_DECLARATION_KEY_PURPOSE.len())?;
        length = checked_length(length, 4)?;
        length = checked_length(length, nested)?;
        length = checked_length(length, 2)?;
        length = checked_length(length, 1)?;
        length = checked_length(length, self.parentage.payload_len())?;
        Ok(length)
    }

    /// Writes the fully framed declaration family after proving all capacity
    /// and fixed-width cells. Structural variants are never accepted here.
    pub fn write_family_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.family_preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let nested_len = self.declaration.preimage_len()?;
        let nested_length = u32::try_from(nested_len)
            .map_err(|_| PreimageOverflow::CellTooLong { actual: nested_len })?;
        let profile: [u8; 2] = self.profile.into();
        let mut cursor = 0;
        out[cursor..cursor + 4].copy_from_slice(&SCOPED_DECLARATION_KEY_PURPOSE_LEN.to_le_bytes());
        cursor += 4;
        out[cursor..cursor + SCOPED_DECLARATION_KEY_PURPOSE.len()]
            .copy_from_slice(SCOPED_DECLARATION_KEY_PURPOSE);
        cursor += SCOPED_DECLARATION_KEY_PURPOSE.len();
        out[cursor..cursor + 4].copy_from_slice(&nested_length.to_le_bytes());
        cursor += 4;
        let written = self
            .declaration
            .write_preimage(&mut out[cursor..cursor + nested_len])?;
        debug_assert_eq!(written, nested_len);
        cursor += nested_len;
        out[cursor..cursor + profile.len()].copy_from_slice(&profile);
        cursor += profile.len();
        out[cursor] = self.parentage.tag();
        cursor += 1;
        match self.parentage {
            DeclarationParentage::Bound(parent) => {
                out[cursor..cursor + 16].copy_from_slice(parent.family.as_bytes());
                cursor += 16;
                out[cursor..cursor + 16].copy_from_slice(parent.variant.as_bytes());
                cursor += 16;
            }
            DeclarationParentage::Unrepresented(identity) => {
                out[cursor..cursor + 16].copy_from_slice(&identity);
                cursor += 16;
            }
            DeclarationParentage::Root | DeclarationParentage::Unavailable => {}
        }
        Ok(cursor)
    }

    /// Mints this scoped declaration family through the central domain
    /// conventions, retaining the full 32-byte digest until the owned IR
    /// narrows it to its compact family lane.
    pub fn family_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<DeclarationFamilyDomain>, PreimageOverflow> {
        let written = self.write_family_preimage(out)?;
        Ok(ContentId::<DeclarationFamilyDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

impl<'bytes> ScopedTypedDeclarationKey<'bytes> {
    /// Binds one validated typed declaration key to a profile and exact
    /// parentage. The anchor itself contains no declaration ordinal or span.
    #[must_use]
    pub const fn new(
        declaration: TypedDeclarationKey<'bytes>,
        profile: LanguageProfile,
        parentage: DeclarationParentage,
    ) -> Self {
        Self {
            declaration,
            profile,
            parentage,
        }
    }

    /// Returns the typed source declaration input.
    #[must_use]
    pub const fn declaration(self) -> TypedDeclarationKey<'bytes> {
        self.declaration
    }

    /// Returns the closed language profile.
    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }

    /// Returns the exact parentage fact.
    #[must_use]
    pub const fn parentage(self) -> DeclarationParentage {
        self.parentage
    }

    /// Complete checked family-preimage width. Named names retain the exact
    /// existing `ScopedDeclarationKey` writer. Anonymous families use a
    /// distinct purpose and include only their parent's stable family.
    pub fn family_preimage_len(&self) -> Result<usize, PreimageOverflow> {
        if let Some(declaration) = self.declaration.as_named_key() {
            return ScopedDeclarationKey::new(declaration, self.profile, self.parentage)
                .family_preimage_len();
        }

        let nested = self.declaration.preimage_len()?;
        let nested_length =
            u32::try_from(nested).map_err(|_| PreimageOverflow::CellTooLong { actual: nested })?;
        let mut length = checked_length(4, SCOPED_ANONYMOUS_CALLABLE_PURPOSE.len())?;
        length = checked_length(length, 4)?;
        length = checked_length(length, nested_length as usize)?;
        length = checked_length(length, 2)?;
        length = checked_length(length, 1)?;
        checked_length(length, anonymous_parentage_payload_len(self.parentage))
    }

    /// Writes the family key. Named declaration bytes are unchanged; the
    /// anonymous branch frames its typed anchor and parent identity explicitly.
    pub fn write_family_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.family_preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        if let Some(declaration) = self.declaration.as_named_key() {
            return ScopedDeclarationKey::new(declaration, self.profile, self.parentage)
                .write_family_preimage(out);
        }

        let nested_len = self.declaration.preimage_len()?;
        let nested_len_u32 = u32::try_from(nested_len)
            .map_err(|_| PreimageOverflow::CellTooLong { actual: nested_len })?;
        let mut cursor = write_str_cell(out, 0, SCOPED_ANONYMOUS_CALLABLE_PURPOSE)?;
        out[cursor..cursor + 4].copy_from_slice(&nested_len_u32.to_le_bytes());
        cursor += 4;
        let nested_end =
            cursor
                .checked_add(nested_len)
                .ok_or(PreimageOverflow::AggregateTooLong {
                    accumulated: cursor,
                    additional: nested_len,
                })?;
        self.declaration
            .write_preimage(&mut out[cursor..nested_end])?;
        cursor = nested_end;
        let profile: [u8; 2] = self.profile.into();
        out[cursor..cursor + profile.len()].copy_from_slice(&profile);
        cursor += profile.len();
        out[cursor] = self.parentage.tag();
        cursor += 1;
        match self.parentage {
            DeclarationParentage::Bound(parent) => {
                out[cursor..cursor + 16].copy_from_slice(parent.family.as_bytes());
                cursor += 16;
            }
            DeclarationParentage::Unrepresented(identity) => {
                out[cursor..cursor + 16].copy_from_slice(&identity);
                cursor += 16;
            }
            DeclarationParentage::Root | DeclarationParentage::Unavailable => {}
        }
        Ok(cursor)
    }

    /// Mints the scoped family through the central declaration-family domain.
    pub fn family_id(
        &self,
        out: &mut [u8],
    ) -> Result<ContentId<DeclarationFamilyDomain>, PreimageOverflow> {
        let written = self.write_family_preimage(out)?;
        Ok(ContentId::<DeclarationFamilyDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

impl<'bytes> AnonymousCallableInstanceKey<'bytes> {
    /// Binds one stable anonymous family to its exact validated TSZ source
    /// site. The project and source digests remain admission witnesses; the
    /// site-derived variant uses the family and byte range, preserving
    /// identity through same-length body-only edits.
    pub fn new(
        family: ScopedTypedDeclarationKey<'bytes>,
        site: TypeScriptCallableSourceCoordinate<'bytes>,
    ) -> Result<Self, AnonymousCallableInstanceFault> {
        if !matches!(
            family.declaration().name(),
            DeclarationName::AnonymousCallable(_)
        ) {
            return Err(AnonymousCallableInstanceFault::NotAnonymousCallable);
        }
        if family.declaration().path() != site.path() {
            return Err(AnonymousCallableInstanceFault::PathMismatch);
        }
        Ok(Self { family, site })
    }

    /// Returns the stable family key, which contains no source-site field.
    #[must_use]
    pub const fn family(self) -> ScopedTypedDeclarationKey<'bytes> {
        self.family
    }

    /// Returns the exact source-site witness admitted by the TSZ project.
    #[must_use]
    pub const fn site(self) -> TypeScriptCallableSourceCoordinate<'bytes> {
        self.site
    }

    /// Complete current-instance variant preimage width.
    pub fn variant_preimage_len(&self) -> Result<usize, PreimageOverflow> {
        let nested = self.family.family_preimage_len()?;
        let nested_length =
            u32::try_from(nested).map_err(|_| PreimageOverflow::CellTooLong { actual: nested })?;
        let mut length = checked_length(4, ANONYMOUS_CALLABLE_INSTANCE_PURPOSE.len())?;
        length = checked_length(length, 4)?;
        length = checked_length(length, nested_length as usize)?;
        let exact_parent_variant = match self.family.parentage {
            DeclarationParentage::Bound(_) => 16,
            DeclarationParentage::Root
            | DeclarationParentage::Unrepresented(_)
            | DeclarationParentage::Unavailable => 0,
        };
        length = checked_length(length, exact_parent_variant)?;
        checked_length(length, 8)
    }

    /// Writes the exact instance variant preimage. The full program/source
    /// digests authenticate admission but do not destabilize this key when
    /// same-length body bytes change at the same source site.
    pub fn write_variant_preimage(&self, out: &mut [u8]) -> Result<usize, PreimageOverflow> {
        let needed = self.variant_preimage_len()?;
        if out.len() < needed {
            return Err(PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let nested_len = self.family.family_preimage_len()?;
        let nested_length = u32::try_from(nested_len)
            .map_err(|_| PreimageOverflow::CellTooLong { actual: nested_len })?;
        let mut cursor = write_str_cell(out, 0, ANONYMOUS_CALLABLE_INSTANCE_PURPOSE)?;
        out[cursor..cursor + 4].copy_from_slice(&nested_length.to_le_bytes());
        cursor += 4;
        let nested_end =
            cursor
                .checked_add(nested_len)
                .ok_or(PreimageOverflow::AggregateTooLong {
                    accumulated: cursor,
                    additional: nested_len,
                })?;
        self.family
            .write_family_preimage(&mut out[cursor..nested_end])?;
        cursor = nested_end;
        if let DeclarationParentage::Bound(parent) = self.family.parentage {
            out[cursor..cursor + 16].copy_from_slice(parent.variant.as_bytes());
            cursor += 16;
        }
        out[cursor..cursor + 4].copy_from_slice(&self.site.callable_start().to_le_bytes());
        cursor += 4;
        out[cursor..cursor + 4].copy_from_slice(&self.site.callable_end().to_le_bytes());
        cursor += 4;
        Ok(cursor)
    }

    /// Mints the stable family and exact current variant as one local entity
    /// identity. The caller supplies scratch large enough for the larger of
    /// the two preimages.
    pub fn declaration_identity(
        &self,
        scratch: &mut [u8],
    ) -> Result<DeclarationIdentity, PreimageOverflow> {
        let family = self.family.family_id(scratch)?;
        let family = crate::ir::DeclarationFamilyId::from_content_id(family);
        let written = self.write_variant_preimage(scratch)?;
        let variant = VariantFingerprint::from_canonical_bytes(&scratch[..written]);
        Ok(DeclarationIdentity { family, variant })
    }
}

fn checked_length(accumulated: usize, additional: usize) -> Result<usize, PreimageOverflow> {
    accumulated
        .checked_add(additional)
        .ok_or(PreimageOverflow::AggregateTooLong {
            accumulated,
            additional,
        })
}

const fn anonymous_parentage_payload_len(parentage: DeclarationParentage) -> usize {
    match parentage {
        DeclarationParentage::Bound(_) => 16,
        DeclarationParentage::Unrepresented(_) => 16,
        DeclarationParentage::Root | DeclarationParentage::Unavailable => 0,
    }
}
