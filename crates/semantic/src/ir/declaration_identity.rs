//! Typed declaration stable-key framing.
//!
//! This module owns only the profile and closed-parentage family preimage.
//! Structural variants are framed outside this key and never enter it.

use crate::vocabulary::LanguageProfile;
use backend_version::{ContentId, DeclarationFamilyDomain};

use crate::ir::DeclarationIdentity;

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
    pub fn family_preimage_len(&self) -> Result<usize, crate::ir::PreimageOverflow> {
        let nested = self.declaration.preimage_len()?;
        let _ = u32::try_from(nested)
            .map_err(|_| crate::ir::PreimageOverflow::CellTooLong { actual: nested })?;
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
    pub fn write_family_preimage(&self, out: &mut [u8]) -> Result<usize, crate::ir::PreimageOverflow> {
        let needed = self.family_preimage_len()?;
        if out.len() < needed {
            return Err(crate::ir::PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let nested_len = self.declaration.preimage_len()?;
        let nested_length = u32::try_from(nested_len)
            .map_err(|_| crate::ir::PreimageOverflow::CellTooLong { actual: nested_len })?;
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
    ) -> Result<ContentId<DeclarationFamilyDomain>, crate::ir::PreimageOverflow> {
        let written = self.write_family_preimage(out)?;
        Ok(ContentId::<DeclarationFamilyDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}

fn checked_length(accumulated: usize, additional: usize) -> Result<usize, crate::ir::PreimageOverflow> {
    accumulated
        .checked_add(additional)
        .ok_or(crate::ir::PreimageOverflow::AggregateTooLong {
            accumulated,
            additional,
        })
}
