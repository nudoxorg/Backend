//! Bounded native generation discovery and owner-bound continuations.
use super::{SemanticSourceHistory, SemanticSourceMetadata, SemanticSourceReference};
use crate::{IndexSearchCursor, PackageReference, ProductAdmissionError};
use serde::{Deserialize, Serialize};

/// Requested initial discovery credit. Adapters measure the complete response
/// and may reduce the first page; a returned cursor fixes its effective credit.
pub const DEFAULT_SEMANTIC_VERSIONS_LIMIT: u16 = 8;

/// Opaque stateless owner cursor for one exact generation-discovery snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SemanticVersionsCursor(String);
/// Parsed cursor operands carry no owner authority. Native lookup requires
/// the separate private-constructor verified continuation capability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionsCursorClaims {
    /// Exact queried product package.
    pub package: PackageReference,
    /// Original immutable source relation root.
    pub source_relation_root: [u8; 32],
    /// Original immutable compiler relation root.
    pub semantic_relation_root: [u8; 32],
    /// Last fully returned native generation coordinate.
    pub coordinate: crate::PackageCoordinate,
    /// Last fully returned native generation profile.
    pub profile: crate::SemanticLanguageProfile,
    /// Last fully returned native generation identity.
    pub generation: crate::SemanticGenerationId,
    /// Admitted continuation's unchanged native page bound.
    pub limit: u16,
    /// Exact native generation count captured once on the first page.
    pub total: u64,
    /// Exact number of descriptors preceding the next page.
    pub offset: u64,
}
impl crate::NativeOwnerStatement for SemanticVersionsCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.semantic.versions-cursor.v1\0";
}
impl SemanticVersionsCursorClaims {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.source_relation_root == [0; 32]
            || self.semantic_relation_root == [0; 32]
            || crate::QueryLimit::new(self.limit).is_none()
            || self.offset == 0
            || self.offset >= self.total
            || self.coordinate.package_type().language() != self.profile.profile()?.language()
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorEnvelope {
    binding: SemanticVersionsCursorClaims,
    seal: [u8; 32],
}
/// Owner-authenticated stateless continuation, never constructed from parsed
/// cursor roots alone. The native caller still checks the current root pair.
#[derive(Debug)]
pub struct VerifiedSemanticVersionsCursor {
    claims: SemanticVersionsCursorClaims,
}
impl VerifiedSemanticVersionsCursor {
    /// Borrows authenticated operands for native root/keyset admission.
    pub const fn claims(&self) -> &SemanticVersionsCursorClaims {
        &self.claims
    }
}
fn encode_cursor<T: Serialize>(value: &T) -> Result<Vec<u8>, ProductAdmissionError> {
    let mut output = super::SourceBuffer {
        bytes: Vec::new(),
        limit: crate::MAX_INDEX_SEARCH_CURSOR_BYTES,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
    Ok(output.bytes)
}
impl SemanticVersionsCursor {
    /// Admits the existing bounded opaque cursor grammar.
    pub fn new(value: String) -> Result<Self, ProductAdmissionError> {
        IndexSearchCursor::new(value.clone())?;
        Ok(Self(value))
    }
    /// Borrows the owner token without replacing its snapshot binding.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    fn envelope(&self) -> Result<CursorEnvelope, ProductAdmissionError> {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.0)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        let envelope: CursorEnvelope = serde_json::from_slice(&bytes)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        envelope.binding.admit()?;
        if encode_cursor(&envelope)? != bytes {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(envelope)
    }
    /// Parses canonical request-correlation operands without granting native
    /// lookup authority; the owner must independently verify the seal.
    pub fn claims(&self) -> Result<SemanticVersionsCursorClaims, ProductAdmissionError> {
        Ok(self.envelope()?.binding)
    }
    /// Issues a cursor only from admitted exact page coverage and native roots.
    pub fn issue(
        binding: SemanticVersionsCursorClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        binding.admit()?;
        let seal = crate::seal_native_owner_statement(secret, &binding)?;
        Self::new(URL_SAFE_NO_PAD.encode(encode_cursor(&CursorEnvelope { binding, seal })?))
    }
    /// Produces the capability required before native continuation root/key use.
    pub fn verify_owner(
        &self,
        secret: &[u8; 32],
    ) -> Result<VerifiedSemanticVersionsCursor, ProductAdmissionError> {
        let envelope = self.envelope()?;
        crate::verify_native_owner_statement(secret, &envelope.binding, &envelope.seal)?;
        Ok(VerifiedSemanticVersionsCursor {
            claims: envelope.binding,
        })
    }
}
impl TryFrom<String> for SemanticVersionsCursor {
    type Error = ProductAdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<SemanticVersionsCursor> for String {
    fn from(value: SemanticVersionsCursor) -> Self {
        value.0
    }
}

/// A version remains discoverable when native source retention refuses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticVersionDescriptor {
    /// The owner has retained the exact source's existing native history roots.
    Available {
        /// Portable exact source guard; use semantic-source to retrieve bytes.
        reference: SemanticSourceReference,
    },
    /// Exact version metadata remains useful; no unreachable source guard is issued.
    Unavailable {
        /// Frozen compiler identity and source frontier.
        metadata: SemanticSourceMetadata,
        /// Exact captured native history availability.
        history: SemanticSourceHistory,
        /// Native retention refusal, bounded by existing product text policy.
        reason: String,
    },
}
impl SemanticVersionDescriptor {
    /// Borrows the exact frozen metadata independent of source availability.
    pub fn metadata(&self) -> &SemanticSourceMetadata {
        match self {
            Self::Available { reference } => &reference.metadata,
            Self::Unavailable { metadata, .. } => metadata,
        }
    }
    /// Borrows the frozen history observation; image-count claims are complete.
    pub fn history(&self) -> &SemanticSourceHistory {
        match self {
            Self::Available { reference } => &reference.history,
            Self::Unavailable { history, .. } => history,
        }
    }
}

/// One native descriptor page with exact immutable generation membership,
/// count, root pair, and continuation. Freshness and history availability are
/// observed per descriptor, not a frozen mutable sidecar snapshot across pages.
/// Each returned source guard seals its own complete observation and bytes;
/// a later observation never rebinds an earlier guard's identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionsPage {
    /// Exact requested product package.
    pub package: PackageReference,
    /// Captured source relation root shared by the whole page chain.
    pub source_relation_root: [u8; 32],
    /// Captured compiler relation root shared by the whole page chain.
    pub semantic_relation_root: [u8; 32],
    /// Count of all immutable versions in this exact native snapshot.
    pub total: u64,
    /// Count of descriptors preceding this page, never a partial total.
    pub offset: u64,
    /// Exact version identities and portable native source availability.
    pub entries: Box<[SemanticVersionDescriptor]>,
    /// Stateless native continuation, present exactly when more rows exist.
    pub next_cursor: Option<SemanticVersionsCursor>,
    /// More native descriptors exist in this same snapshot.
    pub more: bool,
    /// This page ends the exact discovery extent.
    pub complete: bool,
}
impl SemanticVersionsPage {
    /// Correlates exact page coverage with the actual caller operands. Parsed
    /// cursor claims constrain egress only and never authorize native lookup.
    pub fn admit_request(
        &self,
        package: &PackageReference,
        limit: u16,
        cursor: Option<&SemanticVersionsCursor>,
    ) -> Result<(), ProductAdmissionError> {
        self.admit()?;
        if crate::QueryLimit::new(limit).is_none()
            || self.package != *package
            || self.entries.len() > usize::from(limit)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let Some(cursor) = cursor {
            let claims = cursor.claims()?;
            if claims.package != *package
                || claims.limit != limit
                || claims.offset != self.offset
                || claims.total != self.total
                || claims.source_relation_root != self.source_relation_root
                || claims.semantic_relation_root != self.semantic_relation_root
                || self.entries.iter().any(|entry| {
                    let metadata = entry.metadata();
                    // Match ProductSemanticPublicationKey's native ordering,
                    // including the parsed profile (not display spelling).
                    (
                        &metadata.coordinate,
                        metadata.profile.profile().ok(),
                        metadata.generation,
                    ) <= (
                        &claims.coordinate,
                        claims.profile.profile().ok(),
                        claims.generation,
                    )
                })
            {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
        } else if self.offset != 0 {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let Some(cursor) = &self.next_cursor {
            let claims = cursor.claims()?;
            let last = self
                .entries
                .last()
                .ok_or(ProductAdmissionError::SemanticVersionShape)?
                .metadata();
            if claims.package != *package
                || claims.limit != limit
                || claims.offset != self.offset + self.entries.len() as u64
                || claims.total != self.total
                || claims.source_relation_root != self.source_relation_root
                || claims.semantic_relation_root != self.semantic_relation_root
                || claims.coordinate != last.coordinate
                || claims.profile != last.profile
                || claims.generation != last.generation
            {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
        }
        Ok(())
    }
    /// Admits page identity, exact extent, terminal state and source bindings.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        let end = self
            .offset
            .checked_add(self.entries.len() as u64)
            .ok_or(ProductAdmissionError::SemanticVersionShape)?;
        if self.source_relation_root == [0; 32]
            || self.semantic_relation_root == [0; 32]
            || end > self.total
            || (self.entries.is_empty() && self.total != 0)
            || self.more != (end < self.total)
            || self.complete == self.more
            || self.next_cursor.is_some() != self.more
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let mut previous = None;
        for descriptor in &self.entries {
            let metadata = descriptor.metadata();
            let key = (
                &metadata.coordinate,
                metadata.profile.profile()?,
                metadata.generation,
            );
            if metadata.package != self.package
                || metadata.artifacts == 0
                || metadata.coordinate.package_type().language()
                    != metadata
                        .profile
                        .profile()
                        .map_err(|_| ProductAdmissionError::SemanticVersionShape)?
                        .language()
                || previous.is_some_and(|previous| previous >= key)
            {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
            previous = Some(key);
            match descriptor {
                SemanticVersionDescriptor::Available { reference } => {
                    if reference.source_relation_root != self.source_relation_root
                        || reference.semantic_relation_root != self.semantic_relation_root
                        || reference.source.schema != 1
                        || reference.source.total_bytes == 0
                        || reference.source.total_bytes > crate::MAX_SEMANTIC_SHAPE_IMAGE_BYTES
                    {
                        return Err(ProductAdmissionError::SemanticVersionShape);
                    }
                }
                SemanticVersionDescriptor::Unavailable { reason, .. } => {
                    if reason.is_empty() || reason.len() > crate::MAX_PRODUCT_TEXT_BYTES {
                        return Err(ProductAdmissionError::SemanticVersionShape);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    fn page() -> SemanticVersionsPage {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../present/fixtures/semantic-versions-semver-public.json"
        ))
        .expect("real 42-image capture");
        let source: crate::SemanticVersionRecord =
            serde_json::from_value(packet["surface"]["data"][0].clone()).expect("exact source");
        let root = source
            .selected_source_frontier
            .as_ref()
            .expect("captured frontier")
            .source_relation_root;
        let reference = SemanticSourceReference::capture(&source, root, [34; 32], &[7; 32])
            .expect("compact frozen header");
        SemanticVersionsPage {
            package: source.package,
            source_relation_root: root,
            semantic_relation_root: [34; 32],
            total: 1,
            offset: 0,
            entries: Box::new([SemanticVersionDescriptor::Available { reference }]),
            next_cursor: None,
            more: false,
            complete: true,
        }
    }
    #[test]
    fn request_admission_refuses_self_consistent_foreign_and_oversized_pages() {
        let page = page();
        let package = page.package.clone();
        page.admit_request(&package, 1, None)
            .expect("exact first-page caller binding");
        let mut foreign = page.clone();
        let other =
            PackageReference::parse("/abs/another-native-package").expect("foreign exact package");
        foreign.package = other.clone();
        let SemanticVersionDescriptor::Available { reference } = &mut foreign.entries[0] else {
            panic!("available selector")
        };
        reference.metadata.package = other.clone();
        reference
            .metadata
            .selected_source_frontier
            .as_mut()
            .expect("frontier")
            .package = other;
        foreign
            .admit()
            .expect("self-consistency alone does not authenticate caller package");
        assert!(foreign.admit_request(&package, 1, None).is_err());
        let command = crate::SurfaceCommand::SemanticVersions {
            package: package.clone(),
            limit: 1,
            cursor: None,
        };
        assert!(
            crate::admit_surface_reply(
                &command,
                &crate::SurfaceReply::SemanticVersionsPage(foreign)
            )
            .is_err()
        );
        let mut oversized = page.clone();
        let mut second = oversized.entries[0].clone();
        let SemanticVersionDescriptor::Available { reference } = &mut second else {
            panic!("available selector")
        };
        let mut generation = reference.metadata.generation.to_bytes();
        generation[0] ^= 1;
        reference.metadata.generation = crate::SemanticGenerationId::new(generation);
        let mut entries = vec![oversized.entries[0].clone(), second];
        entries.sort_by_key(|entry| entry.metadata().generation);
        oversized.entries = entries.into_boxed_slice();
        oversized.total = 2;
        oversized
            .admit()
            .expect("distinct rows form a self-consistent complete page");
        assert!(oversized.admit_request(&package, 1, None).is_err());
        assert!(
            crate::admit_surface_reply(
                &command,
                &crate::SurfaceReply::SemanticVersionsPage(oversized)
            )
            .is_err()
        );
    }

    #[test]
    fn paged_typed_boundary_refuses_genuine_legacy_records_even_when_self_consistent() {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../present/fixtures/semantic-versions-semver-public.json"
        ))
        .expect("unchanged complete native capture");
        let original: crate::SemanticVersionRecord =
            serde_json::from_value(packet["surface"]["data"][0].clone())
                .expect("genuine legacy raw record");
        let legacy = crate::SurfaceReply::SemanticVersions(Box::new([original.clone()]));
        legacy
            .admit(crate::CommandId::SemanticVersions)
            .expect("archive/self-consistency still readable");
        for package in [
            original.package.clone(),
            PackageReference::parse("/abs/foreign-current-request").expect("foreign caller"),
        ] {
            let command = crate::SurfaceCommand::SemanticVersions {
                package,
                limit: 1,
                cursor: None,
            };
            assert!(
                crate::admit_surface_reply(&command, &legacy).is_err(),
                "legacy arrays cannot acknowledge the current paged command, including a different package"
            );
        }
    }

    #[test]
    fn cursor_owner_admission_and_page_correlation_bind_the_exact_native_frontier() {
        let mut first = page();
        first.total = 3;
        first.more = true;
        first.complete = false;
        let last = first.entries[0].metadata();
        let claims = SemanticVersionsCursorClaims {
            package: first.package.clone(),
            source_relation_root: first.source_relation_root,
            semantic_relation_root: first.semantic_relation_root,
            coordinate: last.coordinate.clone(),
            profile: last.profile,
            generation: last.generation,
            limit: 1,
            total: 3,
            offset: 1,
        };
        let cursor = SemanticVersionsCursor::issue(claims.clone(), &[7; 32])
            .expect("owner-issued exact cursor");
        cursor
            .verify_owner(&[7; 32])
            .expect("same configured authority after restart");
        assert!(cursor.verify_owner(&[8; 32]).is_err());
        first.next_cursor = Some(cursor.clone());
        first
            .admit_request(&first.package, 1, None)
            .expect("native exact first extent");
        let mut skip = cursor
            .envelope()
            .expect("parsed operands have no authority");
        skip.binding.offset = 2;
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let changed = SemanticVersionsCursor::new(
            URL_SAFE_NO_PAD.encode(encode_cursor(&skip).expect("mutated DTO")),
        )
        .expect("bounded token");
        assert!(changed.verify_owner(&[7; 32]).is_err());
        let mut wrong_limit = first.clone();
        let mut wrong_claims = claims.clone();
        wrong_claims.limit = 2;
        wrong_limit.next_cursor = Some(
            SemanticVersionsCursor::issue(wrong_claims, &[7; 32])
                .expect("self-consistent different bound"),
        );
        assert!(wrong_limit.admit_request(&first.package, 1, None).is_err());
        let mut wrong_root = first.clone();
        wrong_root.semantic_relation_root[0] ^= 1;
        assert!(wrong_root.admit_request(&first.package, 1, None).is_err());
        let mut repeated = page();
        repeated.total = 3;
        repeated.offset = 1;
        repeated.more = true;
        repeated.complete = false;
        let mut repeated_claims = claims;
        repeated_claims.offset = 2;
        repeated.next_cursor = Some(
            SemanticVersionsCursor::issue(repeated_claims, &[7; 32])
                .expect("exact second extent claims"),
        );
        repeated
            .admit()
            .expect("self-consistent page still cannot repeat the caller's last key");
        assert!(
            repeated
                .admit_request(&first.package, 1, Some(&cursor))
                .is_err(),
            "last native generation cannot repeat across the boundary"
        );
    }

    #[test]
    fn real_42_image_descriptor_keeps_complete_capture_identity_and_refuses_false_page_extents() {
        let page = page();
        page.admit().expect("complete bounded descriptor");
        let SemanticSourceHistory::Published { proof, .. } = page.entries[0].history() else {
            panic!("real published capture")
        };
        assert_eq!(proof.image_count, 42);
        let encoded = serde_json::to_vec(&page).expect("descriptor wire");
        assert!(
            encoded.len() < 12_000,
            "whole exact descriptor is compact without omitted counts"
        );
        let reopened: SemanticVersionsPage =
            serde_json::from_slice(&encoded).expect("portable cold DTO");
        assert_eq!(reopened, page);
        for mutation in 0..5 {
            let mut wrong = page.clone();
            match mutation {
                0 => wrong.total = 2,
                1 => wrong.offset = 1,
                2 => wrong.more = true,
                3 => wrong.complete = false,
                _ => wrong.semantic_relation_root[0] ^= 1,
            }
            assert!(wrong.admit().is_err());
        }
    }

    #[test]
    fn native_order_refuses_reordered_duplicates_and_older_keys_across_a_cursor_boundary() {
        let original = page();
        let member = |generation| {
            let mut descriptor = original.entries[0].clone();
            let SemanticVersionDescriptor::Available { reference } = &mut descriptor else {
                unreachable!()
            };
            reference.metadata.generation = crate::SemanticGenerationId::new([generation; 32]);
            descriptor
        };
        let mut complete = original.clone();
        complete.total = 3;
        complete.entries = vec![member(10), member(20), member(30)].into_boxed_slice();
        complete
            .admit_request(&complete.package, 3, None)
            .expect("strict native key order");
        let mut reordered = complete.clone();
        reordered.entries.swap(0, 1);
        assert!(reordered.admit().is_err());
        let mut duplicate = complete.clone();
        duplicate.entries[1] = duplicate.entries[0].clone();
        assert!(duplicate.admit().is_err());
        let claims = SemanticVersionsCursorClaims {
            package: original.package.clone(),
            source_relation_root: original.source_relation_root,
            semantic_relation_root: original.semantic_relation_root,
            coordinate: original.entries[0].metadata().coordinate.clone(),
            profile: original.entries[0].metadata().profile,
            generation: crate::SemanticGenerationId::new([20; 32]),
            limit: 1,
            total: 2,
            offset: 1,
        };
        let cursor =
            SemanticVersionsCursor::issue(claims, &[7; 32]).expect("native owner boundary");
        let mut resumed = original.clone();
        resumed.total = 2;
        resumed.offset = 1;
        for generation in [10, 20, 30] {
            resumed.entries = vec![member(generation)].into_boxed_slice();
            resumed
                .admit()
                .expect("one complete page is internally consistent");
            assert_eq!(
                resumed
                    .admit_request(&resumed.package, 1, Some(&cursor))
                    .is_ok(),
                generation > 20,
                "strict progress includes every earlier key, not only equality"
            );
        }
    }
}
