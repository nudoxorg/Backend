//! Language-extension row planning and production.
#![deny(
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    unsafe_code
)]

use alloc::{boxed::Box, vec::Vec};

use super::super::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, CanonicalSemanticPlaneSegmentPayload,
    SemanticPlaneRecordError, TypedRecordPlan, encode_canonical_plane_family,
};
use super::wire::{
    encode_clang, encode_csharp, encode_go, encode_java, encode_python, encode_rust,
    encode_typescript, extension_row_key, extension_tag,
};
use crate::ir::{
    DeclarationIdentity, EntityAuthorityFacts, EntityId, FactAvailability, Language,
    LanguageProfile, SemanticImageAuthority, SemanticInputWitness, SemanticIrPlane,
    SemanticPlaneKind, SemanticReader,
};

/// Stable-key encoder for exactly one source-language profile.
///
/// A family row is emitted for every owner in the profile's sparse extension
/// column. The row handle is only a declaration identity; no IR coordinate is
/// written to the payload.
#[derive(Clone, Copy, Debug)]
pub struct LanguageExtensionRows {
    profile: LanguageProfile,
}

impl LanguageExtensionRows {
    /// Selects the closed language profile named by the resulting SPIR plane.
    #[must_use]
    pub const fn new(profile: LanguageProfile) -> Self {
        Self { profile }
    }

    /// Exact profile whose sparse extension rows are encoded.
    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }
}

impl CanonicalPlaneRowEncoder for LanguageExtensionRows {
    type Handle = DeclarationIdentity;
    type Plan = TypedRecordPlan;

    fn kind(&self) -> SemanticPlaneKind {
        SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(self.profile))
    }

    fn build_plan<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
    ) -> Result<Self::Plan, SemanticPlaneRecordError> {
        require_profile(reader, self.profile)?;
        TypedRecordPlan::build(reader)
    }

    fn collect_keys<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        _plan: &Self::Plan,
        sink: &mut CanonicalSemanticPlaneKeySink<Self::Handle>,
    ) -> Result<(), SemanticPlaneRecordError> {
        require_profile(reader, self.profile)?;
        let tag = extension_tag(self.profile.language());
        let observed = match self.profile.language() {
            Language::TypeScript => collect_rows(
                reader,
                self.profile,
                tag,
                reader.typescript_extensions(),
                sink,
            )?,
            Language::CSharp => {
                collect_rows(reader, self.profile, tag, reader.csharp_extensions(), sink)?
            }
            Language::Go => collect_rows(reader, self.profile, tag, reader.go_extensions(), sink)?,
            Language::Rust => {
                collect_rows(reader, self.profile, tag, reader.rust_extensions(), sink)?
            }
            Language::Python => {
                collect_rows(reader, self.profile, tag, reader.python_extensions(), sink)?
            }
            Language::Java => {
                collect_rows(reader, self.profile, tag, reader.java_extensions(), sink)?
            }
            Language::Clang => {
                collect_rows(reader, self.profile, tag, reader.clang_extensions(), sink)?
            }
        };
        require_other_columns_empty(reader, self.profile.language())?;
        let expected = captured_extension_owners(reader)?;
        if observed != expected {
            return Err(SemanticPlaneRecordError::RowGrammar);
        }
        Ok(())
    }

    fn encode_row<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        plan: &Self::Plan,
        identity: Self::Handle,
        out: &mut Vec<u8>,
    ) -> Result<u8, SemanticPlaneRecordError> {
        require_profile(reader, self.profile)?;
        let owner = reader
            .entity_by_identity(identity)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        let tag = extension_tag(self.profile.language());
        encode_identity(identity, out);
        out.push(1);
        match self.profile.language() {
            Language::TypeScript => encode_typescript(
                plan,
                reader
                    .typescript_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
            Language::CSharp => encode_csharp(
                reader,
                plan,
                reader
                    .csharp_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
            Language::Go => encode_go(
                reader,
                plan,
                reader
                    .go_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
            Language::Rust => encode_rust(
                plan,
                reader
                    .rust_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
            Language::Python => encode_python(
                plan,
                reader
                    .python_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
            Language::Java => encode_java(
                reader,
                plan,
                reader
                    .java_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
            Language::Clang => encode_clang(
                plan,
                reader
                    .clang_extension(owner.id)
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
                out,
            )?,
        }
        Ok(tag)
    }
}

/// Encodes one complete profile-specific extension family.
pub fn encode_language_extension_plane<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    profile: LanguageProfile,
    input: SemanticInputWitness,
    maximum_bytes: usize,
) -> Result<Box<[CanonicalSemanticPlaneSegmentPayload]>, SemanticPlaneRecordError> {
    encode_canonical_plane_family(
        reader,
        &LanguageExtensionRows::new(profile),
        input,
        maximum_bytes,
    )
}

/// Re-encodes the complete extension census from a reader and compares the
/// exact segment inventory and bytes supplied by the caller.
pub fn verify_language_extension_plane_against_reader<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    profile: LanguageProfile,
    input: SemanticInputWitness,
    descriptors: &[crate::ir::SemanticPlaneSegment],
    payloads: &[&[u8]],
    maximum_bytes: usize,
) -> Result<(), SemanticPlaneRecordError> {
    super::verify_semantic_plane_family_against_reader(
        reader,
        &LanguageExtensionRows::new(profile),
        input,
        descriptors,
        payloads,
        maximum_bytes,
    )
}

fn require_profile<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    profile: LanguageProfile,
) -> Result<(), SemanticPlaneRecordError> {
    match reader.image_facts().authority {
        SemanticImageAuthority::Language(observed) if observed == profile => Ok(()),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn collect_rows<Reader, Facts, Rows>(
    reader: &Reader,
    profile: LanguageProfile,
    role: u8,
    rows: Rows,
    sink: &mut CanonicalSemanticPlaneKeySink<DeclarationIdentity>,
) -> Result<usize, SemanticPlaneRecordError>
where
    Reader: SemanticReader + ?Sized,
    Rows: IntoIterator<Item = (EntityId, Facts)>,
{
    let mut count = 0_usize;
    for (owner, _) in rows {
        let entity = reader
            .entity(owner)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        require_extension_availability(entity.authority)?;
        let identity = entity.version.identity();
        sink.push(extension_row_key(profile, identity, role), identity)?;
        count = count
            .checked_add(1)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    }
    Ok(count)
}

fn require_extension_availability(
    authority: EntityAuthorityFacts,
) -> Result<(), SemanticPlaneRecordError> {
    if authority.language_extension == FactAvailability::Captured {
        Ok(())
    } else {
        Err(SemanticPlaneRecordError::RowGrammar)
    }
}

fn require_other_columns_empty<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    selected: Language,
) -> Result<(), SemanticPlaneRecordError> {
    if selected != Language::TypeScript && reader.typescript_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if selected != Language::CSharp && reader.csharp_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if selected != Language::Go && reader.go_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if selected != Language::Rust && reader.rust_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if selected != Language::Python && reader.python_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if selected != Language::Java && reader.java_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    if selected != Language::Clang && reader.clang_extensions().next().is_some() {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    Ok(())
}

fn captured_extension_owners<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
) -> Result<usize, SemanticPlaneRecordError> {
    let mut count = 0_usize;
    for entity in reader.canonical_entities() {
        if entity.authority.language_extension == FactAvailability::Captured {
            count = count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        }
    }
    Ok(count)
}
