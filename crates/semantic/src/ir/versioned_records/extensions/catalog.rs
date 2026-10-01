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

/// Typed failure used by aggregate verification for shared reference budgeting.
#[derive(Debug)]
pub(in crate::ir::versioned_records) enum LanguageExtensionFamilyValidationError {
    Record(SemanticPlaneRecordError),
    ReferenceLimitExceeded,
}

impl From<SemanticPlaneRecordError> for LanguageExtensionFamilyValidationError {
    fn from(error: SemanticPlaneRecordError) -> Self {
        Self::Record(error)
    }
}

impl From<LanguageExtensionFamilyValidationError> for SemanticPlaneRecordError {
    fn from(error: LanguageExtensionFamilyValidationError) -> Self {
        match error {
            LanguageExtensionFamilyValidationError::Record(error) => error,
            LanguageExtensionFamilyValidationError::ReferenceLimitExceeded => Self::RowTooLarge,
        }
    }
}

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

/// Incremental owner for one language-extension family. It parses one row
/// while the segment containing that row is lent, retaining only the checked
/// cross-family catalog.
pub(crate) struct CheckedLanguageExtensionFamilyV2Builder {
    profile: LanguageProfile,
    limits: LanguageExtensionVerificationLimitsV2,
    previous: Option<[u8; 32]>,
    row_keys: Vec<[u8; 32]>,
    owners: Vec<[u8; 32]>,
    declaration_references: Vec<[u8; 32]>,
    types_references: Vec<TypesReferenceV2>,
    root: blake3::Hasher,
    row_count: u64,
    payload_bytes: u64,
    reference_count: u64,
}

impl CheckedLanguageExtensionFamilyV2Builder {
    pub(crate) fn new(
        profile: LanguageProfile,
        limits: LanguageExtensionVerificationLimitsV2,
    ) -> Self {
        let mut root = blake3::Hasher::new();
        root.update(EXTENSION_FAMILY_ROOT_DOMAIN);
        root.update(&<[u8; 2]>::from(profile));
        Self {
            profile,
            limits,
            previous: None,
            row_keys: Vec::new(),
            owners: Vec::new(),
            declaration_references: Vec::new(),
            types_references: Vec::new(),
            root,
            row_count: 0,
            payload_bytes: 0,
            reference_count: 0,
        }
    }

    pub(crate) fn push(
        &mut self,
        key: [u8; 32],
        tag: u8,
        payload: &[u8],
        types: &CheckedTypesFamilyV2,
    ) -> Result<(), LanguageExtensionFamilyValidationError> {
        self.payload_bytes = self
            .payload_bytes
            .checked_add(
                u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
            )
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        self.row_count = self
            .row_count
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        if self.payload_bytes > self.limits.max_payload_bytes
            || self.row_count > self.limits.max_rows
        {
            return Err(SemanticPlaneRecordError::RowTooLarge.into());
        }
        self.row_keys
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        if self.previous.is_some_and(|prior| prior >= key) {
            return Err(SemanticPlaneRecordError::RecordOrder.into());
        }
        let declaration_reference_count = self.declaration_references.len();
        let remaining_reference_budget = self
            .limits
            .max_references
            .saturating_sub(self.reference_count)
            .saturating_sub(1);
        let aggregate_declaration_reference_limit = usize::try_from(remaining_reference_budget)
            .ok()
            .and_then(|remaining| declaration_reference_count.checked_add(remaining))
            .unwrap_or(usize::MAX);
        let parsed = match parse_record_with_declarations(
            SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(self.profile)),
            key,
            tag,
            payload,
            &mut self.declaration_references,
            remaining_reference_budget,
        ) {
            Ok(parsed) => parsed,
            Err(SemanticPlaneRecordError::RowTooLarge) => {
                super::wire::validate_record(
                    SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(self.profile)),
                    key,
                    tag,
                    payload,
                )?;
                if aggregate_declaration_reference_limit
                    <= super::wire::MAX_EXTENSION_DECLARATION_REFERENCES
                {
                    return Err(LanguageExtensionFamilyValidationError::ReferenceLimitExceeded);
                }
                return Err(SemanticPlaneRecordError::RowTooLarge.into());
            }
            Err(error) => return Err(error.into()),
        };
        let added_declaration_references = self
            .declaration_references
            .len()
            .checked_sub(declaration_reference_count)
            .ok_or(SemanticPlaneRecordError::RowGrammar)?;
        let row_reference_count = parsed
            .references
            .iter()
            .flatten()
            .count()
            .checked_add(added_declaration_references)
            .and_then(|count| count.checked_add(1))
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        self.reference_count = self
            .reference_count
            .checked_add(
                u64::try_from(row_reference_count)
                    .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
            )
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        if self.reference_count > self.limits.max_references {
            return Err(LanguageExtensionFamilyValidationError::ReferenceLimitExceeded);
        }
        self.owners
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.types_references
            .try_reserve(parsed.references.iter().flatten().count())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        for reference in parsed.references.iter().flatten() {
            types.require_reference(*reference)?;
            self.types_references.push(*reference);
        }
        self.owners.push(identity_bytes(parsed.identity));
        self.row_keys.push(key);
        self.root.update(&key);
        self.root.update(&[tag]);
        let payload_len =
            u64::try_from(payload.len()).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        self.root.update(&payload_len.to_be_bytes());
        self.root.update(payload);
        self.previous = Some(key);
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        expected_captured_owners: &[[u8; 32]],
    ) -> Result<CheckedLanguageExtensionFamilyV2, SemanticPlaneRecordError> {
        if u64::try_from(expected_captured_owners.len())
            .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?
            > self.limits.max_rows
            || expected_captured_owners.windows(2).any(|pair| {
                pair.first()
                    .zip(pair.get(1))
                    .is_none_or(|(left, right)| left >= right)
            })
        {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        self.owners.sort_unstable();
        if self.owners.windows(2).any(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_none_or(|(left, right)| left >= right)
        }) || self.owners.as_slice() != expected_captured_owners
        {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        self.root.update(&self.row_count.to_be_bytes());
        Ok(CheckedLanguageExtensionFamilyV2 {
            profile: self.profile,
            row_keys: self.row_keys.into_boxed_slice(),
            owner_identities: self.owners.into_boxed_slice(),
            declaration_references: self.declaration_references.into_boxed_slice(),
            types_references: self.types_references.into_boxed_slice(),
            reference_count: self.reference_count,
            local_root: *self.root.finalize().as_bytes(),
            row_count: self.row_count,
        })
    }
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
    validate_language_extension_family_v2_with_limits_detailed(
        profile,
        segments,
        types,
        expected_captured_owners,
        limits,
    )
    .map_err(Into::into)
}

/// Validates a family while preserving the typed reference-limit failure.
pub(in crate::ir::versioned_records) fn validate_language_extension_family_v2_with_limits_detailed<
    'bytes,
>(
    profile: LanguageProfile,
    segments: impl IntoIterator<Item = CanonicalSemanticPlaneSegmentView<'bytes>>,
    types: &CheckedTypesFamilyV2,
    expected_captured_owners: &[[u8; 32]],
    limits: LanguageExtensionVerificationLimitsV2,
) -> Result<CheckedLanguageExtensionFamilyV2, LanguageExtensionFamilyValidationError> {
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
        return Err(SemanticPlaneRecordError::RowGrammar.into());
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
            return Err(SemanticPlaneRecordError::PlaneKind.into());
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
                return Err(SemanticPlaneRecordError::RowTooLarge.into());
            }
            row_keys
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            let key = record.key();
            if previous.is_some_and(|prior| prior >= key) {
                return Err(SemanticPlaneRecordError::RecordOrder.into());
            }
            let declaration_reference_count = declaration_references.len();
            let remaining_reference_budget = limits
                .max_references
                .saturating_sub(reference_count)
                .saturating_sub(1); // Every extension row resolves its owner against Core.
            let aggregate_declaration_reference_limit = usize::try_from(remaining_reference_budget)
                .ok()
                .and_then(|remaining| declaration_reference_count.checked_add(remaining))
                .unwrap_or(usize::MAX);
            let parsed = match parse_record_with_declarations(
                expected_kind,
                key,
                record.tag(),
                record.payload(),
                &mut declaration_references,
                remaining_reference_budget,
            ) {
                Ok(parsed) => parsed,
                Err(SemanticPlaneRecordError::RowTooLarge) => {
                    match super::wire::validate_record(
                        expected_kind,
                        key,
                        record.tag(),
                        record.payload(),
                    ) {
                        Ok(())
                            if aggregate_declaration_reference_limit
                                <= super::wire::MAX_EXTENSION_DECLARATION_REFERENCES =>
                        {
                            return Err(
                                LanguageExtensionFamilyValidationError::ReferenceLimitExceeded,
                            );
                        }
                        Ok(()) => {
                            return Err(SemanticPlaneRecordError::RowTooLarge.into());
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            };
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
            reference_count = reference_count
                .checked_add(
                    u64::try_from(row_reference_count)
                        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?,
                )
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            if reference_count > limits.max_references {
                return Err(LanguageExtensionFamilyValidationError::ReferenceLimitExceeded);
            }
            owners
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            types_references
                .try_reserve(parsed.references.len())
                .map_err(SemanticPlaneRecordError::Allocation)?;
            types_references.extend(parsed.references.iter().flatten().copied());
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
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    if owners.as_slice() != expected_captured_owners {
        return Err(SemanticPlaneRecordError::RowGrammar.into());
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
