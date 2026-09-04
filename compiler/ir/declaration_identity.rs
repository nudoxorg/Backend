//! Typed declaration stable-key framing.
//!
//! This module owns the profile and parentage preimage law.  A collision
//! skeleton is deliberately delegated to the vocabulary disambiguator only
//! after a caller has proven a same-scope sibling collision.

use compiler_vocabulary::LanguageProfile;
use heart_identity::{ContentId, SourceFactDomain};

use crate::semantic::StableEntityId;

/// Purpose tag for declaration keys whose language profile and closed
/// containment fact are part of stable identity.  These facts are not an
/// overload skeleton: unique declarations retain `Disambiguator::None`.
const SCOPED_DECLARATION_KEY_PURPOSE: &[u8] = b"compiler.scoped-declaration.v1";
const SCOPED_DECLARATION_KEY_PURPOSE_LEN: u32 = 31;
const _: () = assert!(
    SCOPED_DECLARATION_KEY_PURPOSE.len() == SCOPED_DECLARATION_KEY_PURPOSE_LEN as usize
);
const SCOPED_KEY_TAIL_BYTES: usize = 8;

/// Closed containment fact retained by one [`ScopedDeclarationKey`].
///
/// A bound parent uses its stable identity rather than any payload. Root and
/// unavailable use distinct tags; an unrepresented authority owner carries
/// the exact opaque identity instead of being collapsed into either state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationParentage {
    /// The authority proved that this declaration is a root.
    Root,
    /// The authority bound this declaration to one local parent's stable id.
    Bound(StableEntityId),
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

    const fn payload(self) -> Option<[u8; 16]> {
        match self {
            Self::Bound(parent) => Some(*parent.as_bytes()),
            Self::Unrepresented(identity) => Some(identity),
            Self::Root | Self::Unavailable => None,
        }
    }
}

/// Typed declaration stable-key input.  The profile remains a
/// `LanguageProfile`, so callers cannot mint a key from an arbitrary raw
/// profile code.  [`crate::Disambiguator::Skeleton`] remains exclusively for
/// a structural overload/implementation collision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedDeclarationKey<'bytes> {
    /// Validated package/path/kind/name fact.
    pub declaration: crate::DeclarationKey<'bytes>,
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
        declaration: crate::DeclarationKey<'bytes>,
        profile: LanguageProfile,
        parentage: DeclarationParentage,
    ) -> Self {
        Self {
            declaration,
            profile,
            parentage,
        }
    }

    /// Complete output width with saturating arithmetic.  An unrepresentable
    /// aggregate becomes `usize::MAX`, causing `write_preimage` to reject a
    /// normal caller buffer before the first byte is mutated.
    #[must_use]
    pub fn preimage_len(&self, disambiguator: crate::Disambiguator<'bytes>) -> usize {
        let nested = self
            .declaration
            .preimage_len(crate::Disambiguator::None);
        let mut length = 4_usize.saturating_add(SCOPED_DECLARATION_KEY_PURPOSE.len());
        length = length.saturating_add(4).saturating_add(nested);
        length = length.saturating_add(2);
        length = length.saturating_add(1);
        if self.parentage.payload().is_some() {
            length = length.saturating_add(16);
        }
        length = length.saturating_add(SCOPED_KEY_TAIL_BYTES);
        if let crate::Disambiguator::Skeleton(skeleton) = disambiguator {
            length = length.saturating_add(4).saturating_add(skeleton.len());
        }
        length
    }

    /// Writes the fully framed key after proving all capacity and fixed-width
    /// cells.  This owns profile/parentage framing centrally; callers only
    /// provide an overload skeleton when a collision has already been proven.
    pub fn write_preimage(
        &self,
        disambiguator: crate::Disambiguator<'bytes>,
        out: &mut [u8],
    ) -> Result<usize, crate::PreimageOverflow> {
        let needed = self.preimage_len(disambiguator);
        if out.len() < needed {
            return Err(crate::PreimageOverflow::OutputShort {
                needed,
                actual: out.len(),
            });
        }
        let nested_len = self
            .declaration
            .preimage_len(crate::Disambiguator::None);
        let nested_length = u32::try_from(nested_len).map_err(|_| {
            crate::PreimageOverflow::CellTooLong { actual: nested_len }
        })?;
        let skeleton_length = match disambiguator {
            crate::Disambiguator::None => 0,
            crate::Disambiguator::Skeleton(skeleton) => u32::try_from(skeleton.len()).map_err(
                |_| crate::PreimageOverflow::SkeletonTooLong {
                    actual: skeleton.len(),
                },
            )?,
        };
        let profile: [u8; 2] = self.profile.into();
        let mut cursor = 0;
        out[cursor..cursor + 4]
            .copy_from_slice(&SCOPED_DECLARATION_KEY_PURPOSE_LEN.to_le_bytes());
        cursor += 4;
        out[cursor..cursor + SCOPED_DECLARATION_KEY_PURPOSE.len()]
            .copy_from_slice(SCOPED_DECLARATION_KEY_PURPOSE);
        cursor += SCOPED_DECLARATION_KEY_PURPOSE.len();
        out[cursor..cursor + 4].copy_from_slice(&nested_length.to_le_bytes());
        cursor += 4;
        let written = self.declaration.write_preimage(
            crate::Disambiguator::None,
            &mut out[cursor..cursor + nested_len],
        )?;
        debug_assert_eq!(written, nested_len);
        cursor += nested_len;
        out[cursor..cursor + profile.len()].copy_from_slice(&profile);
        cursor += profile.len();
        out[cursor] = self.parentage.tag();
        cursor += 1;
        if let Some(parentage) = self.parentage.payload() {
            out[cursor..cursor + parentage.len()].copy_from_slice(&parentage);
            cursor += parentage.len();
        }
        let mut tail = [0_u8; SCOPED_KEY_TAIL_BYTES];
        tail[0] = match disambiguator {
            crate::Disambiguator::None => 0,
            crate::Disambiguator::Skeleton(_) => 1,
        };
        tail[1..5].copy_from_slice(&skeleton_length.to_le_bytes());
        out[cursor..cursor + SCOPED_KEY_TAIL_BYTES].copy_from_slice(&tail);
        cursor += SCOPED_KEY_TAIL_BYTES;
        if let crate::Disambiguator::Skeleton(skeleton) = disambiguator {
            let length = u32::try_from(skeleton.len()).map_err(|_| {
                crate::PreimageOverflow::SkeletonTooLong {
                    actual: skeleton.len(),
                }
            })?;
            out[cursor..cursor + 4].copy_from_slice(&length.to_le_bytes());
            cursor += 4;
            out[cursor..cursor + skeleton.len()].copy_from_slice(skeleton);
            cursor += skeleton.len();
        }
        Ok(cursor)
    }

    /// Mints this scoped source-fact identity through the central domain
    /// conventions, retaining the complete 32-byte hash until the owned IR
    /// narrows it to its compact stable-id lane.
    pub fn stable_id(
        &self,
        disambiguator: crate::Disambiguator<'bytes>,
        out: &mut [u8],
    ) -> Result<ContentId<SourceFactDomain>, crate::PreimageOverflow> {
        let written = self.write_preimage(disambiguator, out)?;
        Ok(ContentId::<SourceFactDomain>::from_canonical_bytes(
            &out[..written],
        ))
    }
}
