//! Canonical profile-specific language-extension rows for SPIR.
#![deny(
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    unsafe_code
)]

use alloc::{boxed::Box, collections::BTreeSet, vec::Vec};

use super::wire::{Cursor, encode_identity, put_bytes, put_u32, read_identity};
use super::{
    CanonicalPlaneRowEncoder, CanonicalSemanticPlaneKeySink, CanonicalSemanticPlaneSegmentPayload,
    CanonicalSemanticPlaneSegmentView, SemanticPlaneRecordError, TypedRecordPlan, TypesReferenceV2,
    TypesRowDomainV2, encode_canonical_plane_family,
};
use crate::ir::{
    CSharpFacts, ClangFacts, Confidence, DeclarationIdentity, EntityAuthorityFacts, EntityId,
    EntityListId, FactAvailability, GoFacts, JavaFacts, Language, LanguageProfile, PythonFacts,
    RustFacts, SemanticImageAuthority, SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind,
    SemanticReader, TypeScriptFacts,
};

const TYPESCRIPT_TAG: u8 = 1;
const CSHARP_TAG: u8 = 2;
const GO_TAG: u8 = 3;
const RUST_TAG: u8 = 4;
const PYTHON_TAG: u8 = 5;
const JAVA_TAG: u8 = 6;
const CLANG_TAG: u8 = 7;
const EXTENSION_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.language-extension-row.v1\0";
const EXTENSION_FAMILY_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.language-extension-family.v1\0";

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

/// A locally decoded language-extension family with every Types reference
/// resolved against the checked Types family.
pub struct CheckedLanguageExtensionFamilyV2 {
    profile: LanguageProfile,
    row_keys: Box<[[u8; 32]]>,
    owner_identities: Box<[[u8; 32]]>,
    declaration_references: Box<[[u8; 32]]>,
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

/// Validates a complete sequence of already reopened extension segments and
/// resolves every typed, list, and atom reference through the Types catalog.
pub fn validate_language_extension_family_v2<'bytes>(
    profile: LanguageProfile,
    segments: impl IntoIterator<Item = CanonicalSemanticPlaneSegmentView<'bytes>>,
    types: &super::CheckedTypesFamilyV2,
) -> Result<CheckedLanguageExtensionFamilyV2, SemanticPlaneRecordError> {
    let expected_kind = SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile));
    let mut previous = None;
    let mut row_keys = Vec::new();
    let mut owners = BTreeSet::new();
    let mut declaration_references = Vec::new();
    let mut root = blake3::Hasher::new();
    root.update(EXTENSION_FAMILY_ROOT_DOMAIN);
    root.update(&<[u8; 2]>::from(profile));
    let mut row_count = 0_u64;
    for segment in segments {
        if segment.kind() != expected_kind {
            return Err(SemanticPlaneRecordError::PlaneKind);
        }
        for record in segment.records() {
            let key = record.key();
            if previous.is_some_and(|prior| prior >= key) {
                return Err(SemanticPlaneRecordError::RecordOrder);
            }
            let parsed = parse_record_with_declarations(
                expected_kind,
                key,
                record.tag(),
                record.payload(),
                &mut declaration_references,
            )?;
            for reference in parsed.references.iter().flatten() {
                types.require_reference(*reference)?;
            }
            let identity_bytes = identity_bytes(parsed.identity);
            if !owners.insert(identity_bytes) {
                return Err(SemanticPlaneRecordError::StableKeyCollision);
            }
            row_keys
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            row_keys.push(key);
            root.update(&key);
            root.update(&[record.tag()]);
            let payload_len = u64::try_from(record.payload().len())
                .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
            root.update(&payload_len.to_be_bytes());
            root.update(record.payload());
            row_count = row_count
                .checked_add(1)
                .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
            previous = Some(key);
        }
    }
    root.update(&row_count.to_be_bytes());
    Ok(CheckedLanguageExtensionFamilyV2 {
        profile,
        row_keys: row_keys.into_boxed_slice(),
        owner_identities: owners.into_iter().collect::<Vec<_>>().into_boxed_slice(),
        declaration_references: declaration_references.into_boxed_slice(),
        local_root: *root.finalize().as_bytes(),
        row_count,
    })
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

fn extension_row_key(
    profile: LanguageProfile,
    identity: DeclarationIdentity,
    role: u8,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EXTENSION_KEY_DOMAIN);
    hasher.update(&<[u8; 2]>::from(profile));
    hasher.update(identity.family.as_bytes());
    hasher.update(identity.variant.as_bytes());
    hasher.update(&[role]);
    *hasher.finalize().as_bytes()
}

fn identity_bytes(identity: DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (target, source) in bytes.iter_mut().take(16).zip(identity.family.as_bytes()) {
        *target = *source;
    }
    for (target, source) in bytes.iter_mut().skip(16).zip(identity.variant.as_bytes()) {
        *target = *source;
    }
    bytes
}

fn extension_tag(language: Language) -> u8 {
    match language {
        Language::TypeScript => TYPESCRIPT_TAG,
        Language::CSharp => CSHARP_TAG,
        Language::Go => GO_TAG,
        Language::Rust => RUST_TAG,
        Language::Python => PYTHON_TAG,
        Language::Java => JAVA_TAG,
        Language::Clang => CLANG_TAG,
    }
}

fn put_reference(out: &mut Vec<u8>, domain: TypesRowDomainV2, key: [u8; 32]) {
    out.push(domain_code(domain));
    out.extend_from_slice(&key);
}

fn put_optional_type_reference(
    out: &mut Vec<u8>,
    plan: &TypedRecordPlan,
    value: Option<crate::ir::TypeId>,
) -> Result<(), SemanticPlaneRecordError> {
    match value {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            put_reference(out, TypesRowDomainV2::Type, plan.type_key(id)?);
        }
    }
    Ok(())
}

fn encode_typescript(
    plan: &TypedRecordPlan,
    facts: TypeScriptFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.type_parameters)?,
    );
    put_optional_type_reference(out, plan, facts.declared)?;
    put_optional_type_reference(out, plan, facts.observed)?;
    Ok(())
}

fn encode_csharp<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    facts: CSharpFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    out.push(csharp_nullability_code(facts.nullability));
    out.push(csharp_reference_code(facts.reference_kind));
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.constraints)?,
    );
    out.push(bool_code(facts.effects.is_async));
    out.push(bool_code(facts.effects.is_iterator));
    out.push(bool_code(facts.effects.is_extension));
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.attributes)?,
    );
    out.push(csharp_partial_code(facts.partial));
    match facts.xml_provenance {
        None => out.push(0),
        Some(span) => {
            out.push(1);
            put_bytes(
                out,
                reader
                    .atom(span.file())
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
            )?;
            out.extend_from_slice(&span.start().to_be_bytes());
            out.extend_from_slice(&span.end().to_be_bytes());
        }
    }
    Ok(())
}

fn encode_go<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    facts: GoFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::TypeList,
        plan.type_list_key(facts.signature.parameters)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::TypeList,
        plan.type_list_key(facts.signature.results)?,
    );
    out.push(bool_code(facts.signature.variadic));
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.type_parameters)?,
    );
    encode_entity_list(reader, facts.fields, out)?;
    encode_entity_list(reader, facts.method_set, out)?;
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.build_constraints)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.constant_value)?,
    );
    out.extend_from_slice(&facts.constant_group.to_be_bytes());
    out.extend_from_slice(&facts.constant_flags.to_be_bytes());
    Ok(())
}

fn encode_rust(
    plan: &TypedRecordPlan,
    facts: RustFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    out.push(rust_ownership_code(facts.ownership));
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.lifetimes)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.where_clauses)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.macros)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.const_defaults)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::FreePredicates,
        plan.free_predicates_key(facts.free_predicates)?,
    );
    Ok(())
}

fn encode_python(
    plan: &TypedRecordPlan,
    facts: PythonFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.decorators)?,
    );
    out.push(python_parameter_code(facts.parameter_kind));
    out.push(confidence_code(facts.dynamic_confidence));
    Ok(())
}

fn encode_java<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    facts: JavaFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::TypeList,
        plan.type_list_key(facts.throws)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.annotations)?,
    );
    encode_entity_list(reader, facts.overloads, out)?;
    encode_entity_list(reader, facts.record_components, out)?;
    Ok(())
}

fn encode_clang(
    plan: &TypedRecordPlan,
    facts: ClangFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    out.push(bool_code(facts.qualifiers.is_const));
    out.push(bool_code(facts.qualifiers.is_volatile));
    out.push(bool_code(facts.qualifiers.is_restrict));
    out.push(clang_storage_code(facts.storage));
    put_optional_u32(out, facts.layout.size_bits);
    put_optional_u32(out, facts.layout.align_bits);
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.templates)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.includes)?,
    );
    Ok(())
}

fn encode_entity_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    list: EntityListId,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let members = reader
        .entity_list(list)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    put_u32(out, members.len())?;
    for member in members {
        let entity = reader
            .entity(member)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        encode_identity(entity.version.identity(), out);
    }
    Ok(())
}

fn put_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    match value {
        None => out.push(0),
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn bool_code(value: bool) -> u8 {
    if value { 1 } else { 0 }
}

fn csharp_nullability_code(value: crate::ir::CSharpNullability) -> u8 {
    match value {
        crate::ir::CSharpNullability::Oblivious => 0,
        crate::ir::CSharpNullability::NonNullable => 1,
        crate::ir::CSharpNullability::Nullable => 2,
    }
}

fn csharp_reference_code(value: crate::ir::CSharpReferenceKind) -> u8 {
    match value {
        crate::ir::CSharpReferenceKind::Value => 0,
        crate::ir::CSharpReferenceKind::In => 1,
        crate::ir::CSharpReferenceKind::Ref => 2,
        crate::ir::CSharpReferenceKind::Out => 3,
    }
}

fn csharp_partial_code(value: crate::ir::CSharpPartialRole) -> u8 {
    match value {
        crate::ir::CSharpPartialRole::None => 0,
        crate::ir::CSharpPartialRole::Definition => 1,
        crate::ir::CSharpPartialRole::Implementation => 2,
    }
}

fn rust_ownership_code(value: crate::ir::RustOwnership) -> u8 {
    match value {
        crate::ir::RustOwnership::Value => 0,
        crate::ir::RustOwnership::SharedBorrow => 1,
        crate::ir::RustOwnership::MutableBorrow => 2,
        crate::ir::RustOwnership::Moved => 3,
    }
}

fn python_parameter_code(value: crate::ir::PythonParameterKind) -> u8 {
    match value {
        crate::ir::PythonParameterKind::PositionalOnly => 0,
        crate::ir::PythonParameterKind::PositionalOrKeyword => 1,
        crate::ir::PythonParameterKind::VariadicPositional => 2,
        crate::ir::PythonParameterKind::KeywordOnly => 3,
        crate::ir::PythonParameterKind::VariadicKeyword => 4,
    }
}

fn confidence_code(value: Confidence) -> u8 {
    match value {
        Confidence::Syntactic => 0,
        Confidence::Heuristic => 1,
        Confidence::Indexed => 2,
        Confidence::Imported => 3,
        Confidence::Compiler => 4,
    }
}

fn clang_storage_code(value: crate::ir::ClangStorageClass) -> u8 {
    match value {
        crate::ir::ClangStorageClass::None => 0,
        crate::ir::ClangStorageClass::Auto => 1,
        crate::ir::ClangStorageClass::Static => 2,
        crate::ir::ClangStorageClass::Extern => 3,
        crate::ir::ClangStorageClass::Register => 4,
        crate::ir::ClangStorageClass::ThreadLocal => 5,
    }
}

fn domain_code(domain: TypesRowDomainV2) -> u8 {
    match domain {
        TypesRowDomainV2::EntityRoot => 0,
        TypesRowDomainV2::TypedNode => 1,
        TypesRowDomainV2::Type => 2,
        TypesRowDomainV2::TypeList => 3,
        TypesRowDomainV2::TupleElements => 4,
        TypesRowDomainV2::ObjectMembers => 5,
        TypesRowDomainV2::TemplateParts => 6,
        TypesRowDomainV2::AtomList => 7,
        TypesRowDomainV2::TypeParameters => 8,
        TypesRowDomainV2::TypeParameterBounds => 9,
        TypesRowDomainV2::FreePredicates => 10,
        TypesRowDomainV2::Atom => 11,
        TypesRowDomainV2::ExternalTarget => 12,
    }
}

fn decode_domain(raw: u8) -> Result<TypesRowDomainV2, SemanticPlaneRecordError> {
    Ok(match raw {
        0 => TypesRowDomainV2::EntityRoot,
        1 => TypesRowDomainV2::TypedNode,
        2 => TypesRowDomainV2::Type,
        3 => TypesRowDomainV2::TypeList,
        4 => TypesRowDomainV2::TupleElements,
        5 => TypesRowDomainV2::ObjectMembers,
        6 => TypesRowDomainV2::TemplateParts,
        7 => TypesRowDomainV2::AtomList,
        8 => TypesRowDomainV2::TypeParameters,
        9 => TypesRowDomainV2::TypeParameterBounds,
        10 => TypesRowDomainV2::FreePredicates,
        11 => TypesRowDomainV2::Atom,
        12 => TypesRowDomainV2::ExternalTarget,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}

#[derive(Clone, Copy)]
struct ParsedExtensionRecord {
    identity: DeclarationIdentity,
    references: [Option<TypesReferenceV2>; 5],
}

fn parse_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<ParsedExtensionRecord, SemanticPlaneRecordError> {
    parse_record_inner(kind, key, tag, payload, &mut None)
}

fn parse_record_with_declarations(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
    declaration_references: &mut Vec<[u8; 32]>,
) -> Result<ParsedExtensionRecord, SemanticPlaneRecordError> {
    parse_record_inner(kind, key, tag, payload, &mut Some(declaration_references))
}

fn parse_record_inner(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
    declaration_references: &mut Option<&mut Vec<[u8; 32]>>,
) -> Result<ParsedExtensionRecord, SemanticPlaneRecordError> {
    let SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile)) = kind else {
        return Err(SemanticPlaneRecordError::RowGrammar);
    };
    let language = match tag {
        TYPESCRIPT_TAG => Language::TypeScript,
        CSHARP_TAG => Language::CSharp,
        GO_TAG => Language::Go,
        RUST_TAG => Language::Rust,
        PYTHON_TAG => Language::Python,
        JAVA_TAG => Language::Java,
        CLANG_TAG => Language::Clang,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    if profile.language() != language {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut cursor = Cursor::new(payload);
    let identity = read_identity(&mut cursor)?;
    if cursor.u8()? != 1 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut references = [None; 5];
    let mut next_reference = 0_usize;
    match tag {
        TYPESCRIPT_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_optional_reference(
                &mut cursor,
                TypesRowDomainV2::Type,
                &mut references,
                &mut next_reference,
            )?;
            read_optional_reference(
                &mut cursor,
                TypesRowDomainV2::Type,
                &mut references,
                &mut next_reference,
            )?;
        }
        CSHARP_TAG => {
            if cursor.u8()? > 2 || cursor.u8()? > 3 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            for _ in 0..3 {
                read_bool(&mut cursor)?;
            }
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            if cursor.u8()? > 2 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            match cursor.u8()? {
                0 => {}
                1 => {
                    let _raw_path = cursor.bytes32()?;
                    let start = cursor.u32()?;
                    let end = cursor.u32()?;
                    if start > end {
                        return Err(SemanticPlaneRecordError::RowGrammar);
                    }
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        GO_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeList,
                &mut references,
                &mut next_reference,
            )?;
            read_bool(&mut cursor)?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_entity_list(&mut cursor, declaration_references)?;
            read_entity_list(&mut cursor, declaration_references)?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            let _group = cursor.take(8)?;
            let _flags = cursor.u32()?;
        }
        RUST_TAG => {
            if cursor.u8()? > 3 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::FreePredicates,
                &mut references,
                &mut next_reference,
            )?;
        }
        PYTHON_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            if cursor.u8()? > 4 || cursor.u8()? > 4 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
        }
        JAVA_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_entity_list(&mut cursor, declaration_references)?;
            read_entity_list(&mut cursor, declaration_references)?;
        }
        CLANG_TAG => {
            for _ in 0..3 {
                read_bool(&mut cursor)?;
            }
            if cursor.u8()? > 5 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            read_optional_u32(&mut cursor)?;
            read_optional_u32(&mut cursor)?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    if extension_row_key(profile, identity, tag) != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(ParsedExtensionRecord {
        identity,
        references,
    })
}

fn read_reference(
    cursor: &mut Cursor<'_>,
    expected: TypesRowDomainV2,
    references: &mut [Option<TypesReferenceV2>; 5],
    next: &mut usize,
) -> Result<(), SemanticPlaneRecordError> {
    let domain = decode_domain(cursor.u8()?)?;
    if domain != expected {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let key = cursor
        .take(32)?
        .try_into()
        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
    let slot = references
        .get_mut(*next)
        .ok_or(SemanticPlaneRecordError::RowGrammar)?;
    *slot = Some(TypesReferenceV2 { domain, key });
    *next = next
        .checked_add(1)
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    Ok(())
}

fn read_optional_reference(
    cursor: &mut Cursor<'_>,
    expected: TypesRowDomainV2,
    references: &mut [Option<TypesReferenceV2>; 5],
    next: &mut usize,
) -> Result<(), SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 => Ok(()),
        1 => read_reference(cursor, expected, references, next),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn read_bool(cursor: &mut Cursor<'_>) -> Result<(), SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 | 1 => Ok(()),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn read_optional_u32(cursor: &mut Cursor<'_>) -> Result<(), SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 => Ok(()),
        1 => {
            let _ = cursor.u32()?;
            Ok(())
        }
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn read_entity_list(
    cursor: &mut Cursor<'_>,
    declaration_references: &mut Option<&mut Vec<[u8; 32]>>,
) -> Result<(), SemanticPlaneRecordError> {
    let count = cursor.u32()?;
    for _ in 0..count {
        let identity = read_identity(cursor)?;
        if let Some(references) = declaration_references.as_deref_mut() {
            references
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            references.push(identity_bytes(identity));
        }
    }
    Ok(())
}

pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    parse_record(kind, key, tag, payload).map(|_| ())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use alloc::vec::Vec;

    use backend_version::ScopeRoot;

    use super::*;
    use crate::ir::versioned_records::{
        CanonicalSemanticPlaneSegmentPayload, SemanticPlaneRecordError, TypesRows,
        decode_semantic_plane_segment, encode_canonical_plane_family, validate_types_family_v2,
    };
    use crate::ir::{
        BuiltinType, CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
        CSharpReferenceKind, CSharpVersion, CStandard, ClangFacts, ClangLayout, ClangQualifiers,
        ClangStorageClass, Confidence, CorePayloadHash, CxxStandard, DeclarationFamilyId,
        EntityAuthorityFacts, EntityVersion, FactAvailability, GoFacts, GoSignature, GoVersion, Ir,
        IrBuilder, Item, ItemKind, JavaFacts, JavaRelease, LanguageExtensionInput,
        ParentageAuthority, PythonFacts, PythonParameterKind, PythonVersion, RustEdition,
        RustFacts, RustOwnership, SemanticInputWitness, SourceSpan, TypeExpr, TypeScriptFacts,
        TypeScriptSource, VariantFingerprint, Visibility,
    };

    const MAXIMUM_BYTES: usize = crate::ir::MAX_SEMANTIC_SEGMENT_BYTES;

    fn version(seed: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([seed; 16]),
            variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
            core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
        }
    }

    fn add_extension(builder: &mut IrBuilder, seed: u8, extension: LanguageExtensionInput<'_>) {
        let item = Item {
            name: builder.intern_atom(b"extension-owner").expect("owner atom"),
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            parent: None,
            semantic_type: None,
            members: builder.intern_members(&[]).expect("empty members"),
            docs: builder.intern_docs(&[]).expect("empty docs"),
            attributes: builder.intern_attributes(&[]).expect("empty attributes"),
            source: None,
        };
        builder
            .add_item(
                version(seed),
                item,
                Some(extension),
                EntityAuthorityFacts {
                    parentage: ParentageAuthority::Root,
                    visibility: FactAvailability::Captured,
                    language_extension: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
            )
            .expect("valid extension owner");
    }

    fn fixture(profile: LanguageProfile) -> Ir {
        let mut builder = IrBuilder::new();
        builder.set_language_profile(profile).expect("profile");
        match profile {
            LanguageProfile::TypeScript(_) => {
                let ty = builder
                    .intern_type(TypeExpr::Concrete(crate::ir::ConcreteType::Builtin(
                        BuiltinType::I32,
                    )))
                    .expect("declared type");
                let facts = TypeScriptFacts {
                    type_parameters: builder
                        .intern_type_parameters(&[])
                        .expect("type parameters"),
                    declared: Some(ty),
                    observed: None,
                };
                add_extension(&mut builder, 1, LanguageExtensionInput::TypeScript(&facts));
            }
            LanguageProfile::CSharp(_) => {
                let attributes = builder.intern_attributes(&[]).expect("attributes");
                let path = builder
                    .intern_atom(&[0xff, b'/', b's', b'r', b'c'])
                    .expect("path");
                let facts = CSharpFacts {
                    nullability: CSharpNullability::Nullable,
                    reference_kind: CSharpReferenceKind::Ref,
                    constraints: builder.intern_type_parameters(&[]).expect("constraints"),
                    effects: CSharpMemberEffects {
                        is_async: true,
                        is_iterator: false,
                        is_extension: true,
                    },
                    attributes,
                    partial: CSharpPartialRole::Implementation,
                    xml_provenance: Some(SourceSpan::new(path, 2, 7).expect("span")),
                };
                add_extension(&mut builder, 2, LanguageExtensionInput::CSharp(&facts));
            }
            LanguageProfile::Go(_) => {
                let types = builder.intern_types(&[]).expect("types");
                let entities = builder.intern_members(&[]).expect("entities");
                let atoms = builder.intern_attributes(&[]).expect("atoms");
                let facts = GoFacts {
                    signature: GoSignature {
                        parameters: types,
                        results: types,
                        variadic: true,
                    },
                    type_parameters: builder
                        .intern_type_parameters(&[])
                        .expect("type parameters"),
                    fields: entities,
                    method_set: entities,
                    build_constraints: atoms,
                    constant_value: atoms,
                    constant_group: -9,
                    constant_flags: 0x81,
                };
                add_extension(&mut builder, 3, LanguageExtensionInput::Go(&facts));
            }
            LanguageProfile::Rust(_) => {
                let atoms = builder.intern_attributes(&[]).expect("atoms");
                let facts = RustFacts {
                    ownership: RustOwnership::MutableBorrow,
                    lifetimes: atoms,
                    where_clauses: builder.intern_type_parameters(&[]).expect("where clauses"),
                    macros: atoms,
                    const_defaults: atoms,
                    free_predicates: builder
                        .intern_free_predicates(&[])
                        .expect("free predicates"),
                };
                add_extension(&mut builder, 4, LanguageExtensionInput::Rust(&facts));
            }
            LanguageProfile::Python(_) => {
                let facts = PythonFacts {
                    decorators: builder.intern_attributes(&[]).expect("decorators"),
                    parameter_kind: PythonParameterKind::VariadicKeyword,
                    dynamic_confidence: Confidence::Compiler,
                };
                add_extension(&mut builder, 5, LanguageExtensionInput::Python(&facts));
            }
            LanguageProfile::Java(_) => {
                let entities = builder.intern_members(&[]).expect("entities");
                let facts = JavaFacts {
                    throws: builder.intern_types(&[]).expect("throws"),
                    annotations: builder.intern_attributes(&[]).expect("annotations"),
                    overloads: entities,
                    record_components: entities,
                };
                add_extension(&mut builder, 6, LanguageExtensionInput::Java(&facts));
            }
            LanguageProfile::C(_) | LanguageProfile::Cxx(_) => {
                let facts = ClangFacts {
                    qualifiers: ClangQualifiers {
                        is_const: true,
                        is_volatile: false,
                        is_restrict: true,
                    },
                    storage: ClangStorageClass::ThreadLocal,
                    layout: ClangLayout {
                        size_bits: Some(128),
                        align_bits: None,
                    },
                    templates: builder.intern_type_parameters(&[]).expect("templates"),
                    includes: builder.intern_attributes(&[]).expect("includes"),
                };
                add_extension(&mut builder, 7, LanguageExtensionInput::Clang(&facts));
            }
        }
        builder.finish().expect("IR")
    }

    fn witness() -> SemanticInputWitness {
        SemanticInputWitness::claimed([0xa1; 32], ScopeRoot::from_bytes([0xb2; 32]))
    }

    fn checked_types(ir: &Ir) -> crate::ir::versioned_records::CheckedTypesFamilyV2 {
        let payloads = encode_canonical_plane_family(ir, &TypesRows, witness(), MAXIMUM_BYTES)
            .expect("Types rows");
        let views = payloads
            .iter()
            .map(|segment| {
                decode_semantic_plane_segment(
                    segment.kind(),
                    &segment.metadata().expect("Types descriptor"),
                    segment.bytes(),
                )
                .expect("reopened Types segment")
            })
            .collect::<Vec<_>>();
        validate_types_family_v2(views).expect("checked Types family")
    }

    fn encode_extensions(
        ir: &Ir,
        profile: LanguageProfile,
    ) -> Box<[CanonicalSemanticPlaneSegmentPayload]> {
        encode_language_extension_plane(ir, profile, witness(), MAXIMUM_BYTES)
            .expect("extension rows")
    }

    fn views(
        payloads: &[CanonicalSemanticPlaneSegmentPayload],
    ) -> Vec<CanonicalSemanticPlaneSegmentView<'_>> {
        payloads
            .iter()
            .map(|segment| {
                decode_semantic_plane_segment(
                    segment.kind(),
                    &segment.metadata().expect("extension descriptor"),
                    segment.bytes(),
                )
                .expect("reopened extension segment")
            })
            .collect()
    }

    #[test]
    fn all_supported_extension_profiles_reopen_and_resolve_types_references() {
        let profiles = [
            LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            LanguageProfile::CSharp(CSharpVersion::CSharp14),
            LanguageProfile::Go(GoVersion::Go125),
            LanguageProfile::Rust(RustEdition::Rust2024),
            LanguageProfile::Python(PythonVersion::Python314),
            LanguageProfile::Java(JavaRelease::Java25),
            LanguageProfile::C(CStandard::C23),
            LanguageProfile::Cxx(CxxStandard::Cxx26),
        ];
        for profile in profiles {
            let ir = fixture(profile);
            let payloads = encode_extensions(&ir, profile);
            let checked = checked_types(&ir);
            let checked_extensions =
                validate_language_extension_family_v2(profile, views(&payloads), &checked)
                    .expect("all exact Types references resolve");
            assert_eq!(checked_extensions.row_count(), 1);
            assert_eq!(checked_extensions.owner_identities().len(), 1);
            let descriptors = payloads
                .iter()
                .map(|segment| segment.metadata().expect("descriptor"))
                .collect::<Vec<_>>();
            let bytes = payloads
                .iter()
                .map(|segment| segment.bytes())
                .collect::<Vec<_>>();
            verify_language_extension_plane_against_reader(
                &ir,
                profile,
                witness(),
                &descriptors,
                &bytes,
                MAXIMUM_BYTES,
            )
            .expect("independent reader census agrees");
        }
    }

    fn typescript_pair(reverse: bool) -> Ir {
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let mut builder = IrBuilder::new();
        builder.set_language_profile(profile).expect("profile");
        let type_parameters = builder.intern_type_parameters(&[]).expect("params");
        let order = if reverse { [2, 1] } else { [1, 2] };
        for seed in order {
            let facts = TypeScriptFacts {
                type_parameters,
                declared: None,
                observed: None,
            };
            add_extension(
                &mut builder,
                seed,
                LanguageExtensionInput::TypeScript(&facts),
            );
        }
        builder.finish().expect("IR")
    }

    #[test]
    fn declaration_reordering_preserves_keys_and_independent_reader_oracle() {
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let first = typescript_pair(false);
        let reordered = typescript_pair(true);
        let first_payloads = encode_extensions(&first, profile);
        let reordered_payloads = encode_extensions(&reordered, profile);
        let first_bytes = first_payloads
            .iter()
            .map(|segment| segment.bytes())
            .collect::<Vec<_>>();
        let reordered_bytes = reordered_payloads
            .iter()
            .map(|segment| segment.bytes())
            .collect::<Vec<_>>();
        assert_eq!(first_bytes, reordered_bytes);
        let descriptors = first_payloads
            .iter()
            .map(|segment| segment.metadata().expect("descriptor"))
            .collect::<Vec<_>>();
        verify_language_extension_plane_against_reader(
            &reordered,
            profile,
            witness(),
            &descriptors,
            &first_bytes,
            MAXIMUM_BYTES,
        )
        .expect("independent reordered reader has the exact family");
    }

    #[test]
    fn stable_key_domain_includes_profile_and_row_role() {
        let identity = version(1).identity();
        let typescript = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let tsx = LanguageProfile::TypeScript(TypeScriptSource::Tsx);
        assert_ne!(
            extension_row_key(typescript, identity, TYPESCRIPT_TAG),
            extension_row_key(tsx, identity, TYPESCRIPT_TAG)
        );
        assert_ne!(
            extension_row_key(typescript, identity, TYPESCRIPT_TAG),
            extension_row_key(typescript, identity, CSHARP_TAG)
        );
    }

    #[test]
    fn decoder_rejects_unknown_tags_wrong_domain_lengths_and_trailing_bytes() {
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let ir = fixture(profile);
        let payloads = encode_extensions(&ir, profile);
        let segment = payloads.first().expect("one extension segment");
        let view = decode_semantic_plane_segment(
            segment.kind(),
            &segment.metadata().expect("descriptor"),
            segment.bytes(),
        )
        .expect("valid segment");
        let record = view.records().next().expect("one row");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile));
        let mut trailing = record.payload().to_vec();
        trailing.push(0);
        assert!(matches!(
            validate_record(kind, record.key(), record.tag(), &trailing),
            Err(SemanticPlaneRecordError::RowTrailingBytes)
        ));
        assert!(matches!(
            validate_record(kind, record.key(), u8::MAX, record.payload()),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
        let mut wrong_domain = record.payload().to_vec();
        let domain_offset = 33;
        if let Some(domain) = wrong_domain.get_mut(domain_offset) {
            *domain = domain_code(TypesRowDomainV2::AtomList);
        }
        assert!(matches!(
            validate_record(kind, record.key(), record.tag(), &wrong_domain),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
        let mut unknown_domain = record.payload().to_vec();
        if let Some(domain) = unknown_domain.get_mut(domain_offset) {
            *domain = u8::MAX;
        }
        assert!(matches!(
            validate_record(kind, record.key(), record.tag(), &unknown_domain),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
        let mut unavailable = record.payload().to_vec();
        if let Some(availability) = unavailable.get_mut(32) {
            *availability = 0;
        }
        assert!(matches!(
            validate_record(kind, record.key(), record.tag(), &unavailable),
            Err(SemanticPlaneRecordError::RowGrammar)
        ));
        let mut short = record.payload().to_vec();
        let _ = short.pop();
        assert!(matches!(
            validate_record(kind, record.key(), record.tag(), &short),
            Err(SemanticPlaneRecordError::Truncated)
        ));
        let mut wrong_key = record.key();
        if let Some(first) = wrong_key.first_mut() {
            *first ^= 1;
        }
        assert!(matches!(
            validate_record(kind, wrong_key, record.tag(), record.payload()),
            Err(SemanticPlaneRecordError::StableKeyMismatch)
        ));
    }

    #[test]
    fn independent_reader_oracle_rejects_a_missing_extension_range() {
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let ir = fixture(profile);
        assert!(matches!(
            verify_language_extension_plane_against_reader(
                &ir,
                profile,
                witness(),
                &[],
                &[],
                MAXIMUM_BYTES,
            ),
            Err(SemanticPlaneRecordError::SegmentCount { .. })
        ));
    }
}
