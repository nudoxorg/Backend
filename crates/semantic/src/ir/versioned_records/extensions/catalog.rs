//! Checked language-extension family catalog.
#![deny(
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    unsafe_code
)]

use alloc::{boxed::Box, vec::Vec};

use super::super::{
    CanonicalSemanticPlaneSegmentView, CheckedTypesFamilyV2, SemanticIrPlane, SemanticPlaneKind,
    SemanticPlaneRecordError, TypesReferenceV2,
};
use super::wire::{identity_bytes, parse_record_with_declarations};
use crate::ir::LanguageProfile;

const EXTENSION_FAMILY_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.language-extension-family.v1\0";

/// Explicit resource ceiling for standalone language-extension validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LanguageExtensionVerificationLimitsV2 {
    max_payload_bytes: u64,
    max_rows: u64,
    max_references: u64,
}

impl LanguageExtensionVerificationLimitsV2 {
    /// Standard bounded local validation policy.
    pub const fn standard() -> Self {
        Self {
            max_payload_bytes: 64 * 1024 * 1024,
            max_rows: 500_000,
            max_references: 1_000_000,
        }
    }

    /// Higher bounded tier for larger semantic packages.
    pub const fn large_package() -> Self {
        Self {
            max_payload_bytes: 512 * 1024 * 1024,
            max_rows: 2_000_000,
            max_references: 8_000_000,
        }
    }

    pub(crate) const fn bounded(
        max_payload_bytes: u64,
        max_rows: u64,
        max_references: u64,
    ) -> Self {
        Self {
            max_payload_bytes,
            max_rows,
            max_references,
        }
    }
}

/// A locally decoded language-extension family with every Types reference
/// resolved against the checked Types family.
pub struct CheckedLanguageExtensionFamilyV2 {
    profile: LanguageProfile,
    row_keys: Box<[[u8; 32]]>,
    owner_identities: Box<[[u8; 32]]>,
    declaration_references: Box<[[u8; 32]]>,
    types_references: Box<[TypesReferenceV2]>,
    reference_count: u64,
    local_root: [u8; 32],
    row_count: u64,
}

impl CheckedLanguageExtensionFamilyV2 {
    /// Exact source profile bound to this checked family.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Sorted stable extension row keys.
    #[must_use]
    pub fn row_keys(&self) -> &[[u8; 32]] {
        &self.row_keys
    }

    /// Sorted declaration identity bytes owned by this family.
    #[must_use]
    pub fn owner_identities(&self) -> &[[u8; 32]] {
        &self.owner_identities
    }

    /// Local declaration identities named by extension entity lists.
    #[must_use]
    pub fn declaration_references(&self) -> &[[u8; 32]] {
        &self.declaration_references
    }

    /// Every typed/list/atom root directly named by extension rows, in
    /// payload order, for normalized Types-closure reachability.
    #[must_use]
    pub fn types_references(&self) -> &[TypesReferenceV2] {
        &self.types_references
    }

    /// Number of exact Types and declaration references parsed from these
    /// extension rows, before any cross-family set deduplication.
    #[must_use]
    pub const fn reference_count(&self) -> u64 {
        self.reference_count
    }

    /// Canonical local commitment over rows in stable-key order.
    #[must_use]
    pub const fn local_root(&self) -> &[u8; 32] {
        &self.local_root
    }

    /// Number of validated extension rows.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }
}

/// Validates a complete sequence of already reopened extension segments,
/// matches its owners against the captured-extension inventory from checked
/// Core rows, and resolves every typed, list, and atom reference through the
/// Types catalog.
///
/// `expected_captured_owners` must come from the checked Core family and be in
/// strict identity-byte order. Go and Java declaration-list references are
/// returned for the aggregate verifier to resolve against that same Core
/// catalog.
pub fn validate_language_extension_family_v2<'bytes>(
    profile: LanguageProfile,
    segments: impl IntoIterator<Item = CanonicalSemanticPlaneSegmentView<'bytes>>,
    types: &CheckedTypesFamilyV2,
    expected_captured_owners: &[[u8; 32]],
) -> Result<CheckedLanguageExtensionFamilyV2, SemanticPlaneRecordError> {
    validate_language_extension_family_v2_with_limits(
        profile,
        segments,
        types,
        expected_captured_owners,
        LanguageExtensionVerificationLimitsV2::standard(),
    )
}

/// Validates a complete extension family under one explicit resource tier.
pub fn validate_language_extension_family_v2_with_limits<'bytes>(
    profile: LanguageProfile,
    segments: impl IntoIterator<Item = CanonicalSemanticPlaneSegmentView<'bytes>>,
    types: &CheckedTypesFamilyV2,
    expected_captured_owners: &[[u8; 32]],
    limits: LanguageExtensionVerificationLimitsV2,
) -> Result<CheckedLanguageExtensionFamilyV2, SemanticPlaneRecordError> {
    let expected_kind = SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile));
    if u64::try_from(expected_captured_owners.len())
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
        > limits.max_rows
        || expected_captured_owners.windows(2).any(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_none_or(|(left, right)| left >= right)
        })
    {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut previous = None;
    let mut row_keys = Vec::new();
    let mut owners = Vec::new();
    let mut declaration_references = Vec::new();
    let mut types_references = Vec::new();
    let mut root = blake3::Hasher::new();
    root.update(EXTENSION_FAMILY_ROOT_DOMAIN);
    root.update(&<[u8; 2]>::from(profile));
    let mut row_count = 0_u64;
    let mut payload_bytes = 0_u64;
    let mut reference_count = 0_u64;
    for segment in segments {
        if segment.kind() != expected_kind {
            return Err(SemanticPlaneRecordError::PlaneKind);
        }
        for record in segment.records() {
            payload_bytes = payload_bytes
                .checked_add(
                    u64::try_from(record.payload().len())
                        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                )
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            row_count = row_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if payload_bytes > limits.max_payload_bytes || row_count > limits.max_rows {
                return Err(SemanticPlaneRecordError::RowTooLarge);
            }
            row_keys
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            owners
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            let key = record.key();
            if previous.is_some_and(|prior| prior >= key) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            let declaration_reference_count = declaration_references.len();
            let parsed = parse_record_with_declarations(
                expected_kind,
                key,
                record.tag(),
                record.payload(),
                &mut declaration_references,
            )?;
            let added_declaration_references = declaration_references
                .len()
                .checked_sub(declaration_reference_count)
                .ok_or(SemanticPlaneRecordError::RowGrammar)?;
            let row_reference_count = parsed
                .references
                .iter()
                .flatten()
                .count()
                .checked_add(added_declaration_references)
                .and_then(|count| count.checked_add(1)) // Owner identity resolves against Core.
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            types_references
                .try_reserve(parsed.references.len())
                .map_err(SemanticPlaneRecordError::Allocation)?;
            types_references.extend(parsed.references.iter().flatten().copied());
            reference_count = reference_count
                .checked_add(
                    u64::try_from(row_reference_count)
                        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                )
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if reference_count > limits.max_references {
                return Err(SemanticPlaneRecordError::RowTooLarge);
            }
            for reference in parsed.references.iter().flatten() {
                types.require_reference(*reference)?;
            }
            let identity_bytes = identity_bytes(parsed.identity);
            owners.push(identity_bytes);
            row_keys.push(key);
            root.update(&key);
            root.update(&[record.tag()]);
            let payload_len = u64::try_from(record.payload().len())
                .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            root.update(&payload_len.to_be_bytes());
            root.update(record.payload());
            previous = Some(key);
        }
    }
    owners.sort_unstable();
    if owners.windows(2).any(|pair| {
        pair.first()
            .zip(pair.get(1))
            .is_none_or(|(left, right)| left >= right)
    }) {
        return Err(SemanticPlaneRecordError::StableKeyCollision);
    }
    if owners.as_slice() != expected_captured_owners {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    root.update(&row_count.to_be_bytes());
    Ok(CheckedLanguageExtensionFamilyV2 {
        profile,
        row_keys: row_keys.into_boxed_slice(),
        owner_identities: owners.into_boxed_slice(),
        declaration_references: declaration_references.into_boxed_slice(),
        types_references: types_references.into_boxed_slice(),
        reference_count,
        local_root: *root.finalize().as_bytes(),
        row_count,
    })
}
